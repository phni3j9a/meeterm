//! Byte-oriented tmux Control Mode support.
//!
//! The SSH channel carrying a control-mode client is not a terminal stream.
//! tmux wraps pane output in `%output` notifications and octal-escapes the
//! payload.  This module keeps that framing separate from the terminal parser
//! and exposes only decoded pane bytes and structured command results to the
//! SSH lifecycle.

use std::fmt;

/// The managed tmux session used by meeterm.
pub const SESSION_NAME: &str = "meeterm";

/// Low-frequency state exposed to the control bridge.  `panes` is a flat
/// view for mobile list rendering; `windows` retains the canonical tmux
/// window/pane hierarchy for callers that need it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub windows: Vec<WindowSnapshot>,
    pub panes: Vec<PaneSnapshot>,
    pub selected_pane: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub window_id: u64,
    pub name: String,
    pub panes: Vec<PaneSnapshot>,
    pub selected: bool,
    pub zoomed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSnapshot {
    pub window_id: u64,
    pub pane_id: u64,
    pub terminal_id: u64,
    pub window_name: String,
    pub selected: bool,
    pub index: u32,
    pub columns: u16,
    pub rows: u16,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowInfo {
    pub(crate) window_id: u64,
    pub(crate) name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneInfo {
    pub(crate) window_id: u64,
    pub(crate) pane_id: u64,
    pub(crate) index: u32,
    pub(crate) active: bool,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) title: String,
    pub(crate) zoomed: bool,
    pub(crate) window_active: bool,
}

/// The control-mode hooks used while the mobile client owns a zoomed window.
///
/// tmux stores hooks as array options. A high, bounded index gives meeterm a
/// small namespace without replacing a user's unindexed hook or an unrelated
/// hook at the same index. The allocator below also records whether tmux
/// needs an unindexed placeholder removed after the hook runs (tmux can leave
/// an empty index-0 array member when a hook removes itself).
pub const ZOOM_RECOVERY_HOOK_INDEX_START: u32 = 1_000;
pub const ZOOM_RECOVERY_HOOK_INDEX_LIMIT: u32 = 1_100;

const ZOOM_RECOVERY_DETACHED_HOOK: &str = "client-detached";
const ZOOM_RECOVERY_SESSION_CHANGED_HOOK: &str = "client-session-changed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoomRecoveryHookAllocation {
    pub index: u32,
}

/// A command response block emitted by tmux Control Mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBlock {
    /// The command number assigned by tmux, when it was present in the
    /// control-mode header.
    pub number: u64,
    /// Lines between `%begin` and `%end`/`%error`, without line terminators.
    pub lines: Vec<Vec<u8>>,
    /// True when the block ended with `%error`.
    pub error: bool,
}

/// A decoded Control Mode event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Command(CommandBlock),
    /// Pane output after tmux's octal escaping has been decoded.
    Output {
        pane_id: u64,
        bytes: Vec<u8>,
    },
    /// A non-output notification.  The first token is the notification name
    /// without `%`; remaining tokens retain their byte representation.
    Notification {
        name: String,
        arguments: Vec<Vec<u8>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    InvalidHeader,
    InvalidCommandNumber,
    UnexpectedCommandEnd,
    InvalidNotification,
    InvalidPaneId,
    InvalidOctalEscape,
    BufferTooLarge,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidHeader => "invalid tmux control-mode header",
            Self::InvalidCommandNumber => "invalid tmux control-mode command number",
            Self::UnexpectedCommandEnd => "unexpected tmux control-mode command end",
            Self::InvalidNotification => "invalid tmux control-mode notification",
            Self::InvalidPaneId => "invalid tmux pane ID",
            Self::InvalidOctalEscape => "invalid tmux octal escape",
            Self::BufferTooLarge => "tmux control-mode input buffer is too large",
        })
    }
}

impl std::error::Error for DecodeError {}

const MAX_CONTROL_LINE: usize = 16 * 1024 * 1024;
const MAX_CONTROL_BUFFER: usize = 32 * 1024 * 1024;

/// Incremental parser for bytes read from a tmux `-C` client.
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    block: Option<CommandBlockBuilder>,
}

#[derive(Debug)]
struct CommandBlockBuilder {
    header: CommandHeader,
    lines: Vec<Vec<u8>>,
    error: bool,
    body_bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct CommandHeader {
    timestamp: Vec<u8>,
    number: u64,
    flags: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one arbitrary SSH channel chunk and return all complete events.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, DecodeError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_CONTROL_BUFFER {
            return Err(DecodeError::BufferTooLarge);
        }
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            if newline > MAX_CONTROL_LINE {
                return Err(DecodeError::BufferTooLarge);
            }
            let mut line = self.buffer.drain(..=newline).collect::<Vec<_>>();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.consume_line(line, &mut events)?;
        }
        if self.buffer.len() > MAX_CONTROL_LINE {
            return Err(DecodeError::BufferTooLarge);
        }
        Ok(events)
    }

    /// Reject a truncated command block when the SSH channel closes.
    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.block.is_some() || !self.buffer.is_empty() {
            return Err(DecodeError::UnexpectedCommandEnd);
        }
        Ok(())
    }

    fn consume_line(&mut self, line: Vec<u8>, events: &mut Vec<Event>) -> Result<(), DecodeError> {
        if let Some(block) = self.block.as_mut() {
            if line.starts_with(b"%end ") || line.starts_with(b"%error ") {
                // Pane data returned by capture-pane is arbitrary bytes. It
                // may contain text that looks like a control-mode marker,
                // so only a well-formed marker for this exact command block
                // is allowed to terminate the response. A marker for a
                // different command is body data, as is a malformed marker.
                let matching_header = parse_block_header(&line).ok().is_some_and(|header| {
                    header.number == block.header.number
                        && header.timestamp == block.header.timestamp
                        && header.flags == block.header.flags
                });
                if matching_header {
                    let error = line.starts_with(b"%error ");
                    let mut completed = self.block.take().expect("command block exists");
                    completed.error = error;
                    events.push(Event::Command(CommandBlock {
                        number: completed.header.number,
                        lines: completed.lines,
                        error,
                    }));
                    return Ok(());
                }
            }
            // `%begin` can occur at the start of a captured pane line. It is
            // not a nested Control Mode response while a command block is
            // already open.
            let added = line.len().saturating_add(1);
            if block.body_bytes.saturating_add(added) > MAX_CONTROL_BUFFER {
                return Err(DecodeError::BufferTooLarge);
            }
            block.body_bytes += added;
            block.lines.push(line);
            return Ok(());
        }

        if line.starts_with(b"%begin ") {
            let header = parse_block_header(&line)?;
            self.block = Some(CommandBlockBuilder {
                header,
                lines: Vec::new(),
                error: false,
                body_bytes: 0,
            });
            return Ok(());
        }
        if line.starts_with(b"%end ") || line.starts_with(b"%error ") {
            return Err(DecodeError::UnexpectedCommandEnd);
        }
        self.parse_notification(line, events)
    }

    fn parse_notification(
        &self,
        line: Vec<u8>,
        events: &mut Vec<Event>,
    ) -> Result<(), DecodeError> {
        if !line.starts_with(b"%") {
            return Err(DecodeError::InvalidNotification);
        }
        let mut fields = line.split(|byte| *byte == b' ');
        let name = fields
            .next()
            .and_then(|field| field.strip_prefix(b"%"))
            .filter(|field| !field.is_empty())
            .and_then(|field| std::str::from_utf8(field).ok())
            .ok_or(DecodeError::InvalidNotification)?
            .to_owned();
        let arguments = fields
            .filter(|field| !field.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();

        if name == "output" {
            // A pane payload is allowed to be empty and may consist solely
            // of spaces. Parse only the pane-id separator from the original
            // bytes; splitting the whole line would lose that payload.
            let rest = line
                .strip_prefix(b"%output ")
                .ok_or(DecodeError::InvalidNotification)?;
            let separator = rest
                .iter()
                .position(|byte| *byte == b' ')
                .ok_or(DecodeError::InvalidNotification)?;
            let pane_id = parse_pane_id(&rest[..separator])?;
            let encoded = &rest[separator + 1..];
            events.push(Event::Output {
                pane_id,
                bytes: decode_octal(encoded)?,
            });
            return Ok(());
        }

        if name == "extended-output" {
            if arguments.len() < 2 {
                return Err(DecodeError::InvalidNotification);
            }
            let pane_id = parse_pane_id(&arguments[0])?;
            let colon = line
                .iter()
                .position(|byte| *byte == b':')
                .ok_or(DecodeError::InvalidNotification)?;
            if colon == 0 || line[colon - 1] != b' ' {
                return Err(DecodeError::InvalidNotification);
            }
            let encoded = line
                .get(colon.saturating_add(1)..)
                .ok_or(DecodeError::InvalidNotification)?
                .strip_prefix(b" ")
                .unwrap_or_default();
            events.push(Event::Output {
                pane_id,
                bytes: decode_octal(encoded)?,
            });
            return Ok(());
        }

        // The common notification forms are ASCII.  Keep unknown future
        // notifications observable rather than making the protocol parser
        // fail closed for an additive tmux feature.
        if name == "window-add"
            || name == "window-close"
            || name == "window-renamed"
            || name == "window-pane-changed"
            || name == "session-changed"
            || name == "session-renamed"
            || name == "layout-change"
            || name == "client-session-changed"
            || name == "sessions-changed"
            || name == "client-detached"
            || name == "exit"
            || name == "pane-mode-changed"
            || name == "unlinked-window-add"
            || name == "unlinked-window-close"
            || name == "unlinked-window-renamed"
        {
            // The fields have already been copied, and retaining them as
            // bytes is intentional: names can contain non-ASCII UTF-8.
            events.push(Event::Notification { name, arguments });
            return Ok(());
        }

        events.push(Event::Notification { name, arguments });
        Ok(())
    }
}

fn parse_block_header(line: &[u8]) -> Result<CommandHeader, DecodeError> {
    let mut fields = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let tag = fields.next().ok_or(DecodeError::InvalidHeader)?;
    if tag != b"%begin" && tag != b"%end" && tag != b"%error" {
        return Err(DecodeError::InvalidHeader);
    }
    let timestamp = fields.next().ok_or(DecodeError::InvalidHeader)?;
    let number = fields.next().ok_or(DecodeError::InvalidHeader)?;
    let flags = fields.next().ok_or(DecodeError::InvalidHeader)?;
    if fields.next().is_some() {
        return Err(DecodeError::InvalidHeader);
    }
    Ok(CommandHeader {
        timestamp: timestamp.to_vec(),
        number: parse_decimal_u64(number).map_err(|()| DecodeError::InvalidCommandNumber)?,
        flags: flags.to_vec(),
    })
}

fn parse_decimal_u64(digits: &[u8]) -> Result<u64, ()> {
    if digits.is_empty() {
        return Err(());
    }
    digits.iter().try_fold(0_u64, |value, byte| {
        if !byte.is_ascii_digit() {
            return Err(());
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or(())
    })
}

pub fn parse_pane_id(value: &[u8]) -> Result<u64, DecodeError> {
    let digits = value
        .strip_prefix(b"%")
        .filter(|digits| !digits.is_empty())
        .ok_or(DecodeError::InvalidPaneId)?;
    parse_decimal_u64(digits).map_err(|()| DecodeError::InvalidPaneId)
}

pub fn parse_window_id(value: &[u8]) -> Result<u64, DecodeError> {
    let digits = value
        .strip_prefix(b"@")
        .filter(|digits| !digits.is_empty())
        .ok_or(DecodeError::InvalidNotification)?;
    parse_decimal_u64(digits).map_err(|()| DecodeError::InvalidNotification)
}

pub(crate) fn parse_window_line(line: &[u8]) -> Result<WindowInfo, DecodeError> {
    let separator = line
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or(DecodeError::InvalidNotification)?;
    let (id, name) = (&line[..separator], &line[separator + 1..]);
    Ok(WindowInfo {
        window_id: parse_window_id(id)?,
        name: decode_tmux_quoted(name),
    })
}

pub(crate) fn parse_pane_line(line: &[u8]) -> Result<PaneInfo, DecodeError> {
    let fields = line.split(|byte| *byte == b'\t').collect::<Vec<_>>();
    if fields.len() < 8 {
        return Err(DecodeError::InvalidNotification);
    }
    let parse_u32 = |field: &[u8]| {
        std::str::from_utf8(field)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(DecodeError::InvalidNotification)
    };
    let parse_u16 = |field: &[u8]| {
        std::str::from_utf8(field)
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or(DecodeError::InvalidNotification)
    };
    let parse_flag = |field: &[u8]| match field {
        b"0" => Ok(false),
        b"1" => Ok(true),
        _ => Err(DecodeError::InvalidNotification),
    };
    Ok(PaneInfo {
        window_id: parse_window_id(fields[0])?,
        pane_id: parse_pane_id(fields[1])?,
        index: parse_u32(fields[2])?,
        active: parse_flag(fields[3])?,
        columns: parse_u16(fields[4])?,
        rows: parse_u16(fields[5])?,
        title: decode_tmux_quoted(fields[6]),
        zoomed: parse_flag(fields[7])?,
        window_active: fields.get(8).map_or(Ok(true), |field| parse_flag(field))?,
    })
}

/// Decode the backslash quoting emitted by tmux's `q:` format modifier. It
/// keeps unknown escapes losslessly enough for display while decoding the
/// whitespace and octal forms used for names and titles.
fn decode_tmux_quoted(value: &[u8]) -> String {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] != b'\\' || index + 1 == value.len() {
            decoded.push(value[index]);
            index += 1;
            continue;
        }
        if index + 3 < value.len()
            && (b'0'..=b'7').contains(&value[index + 1])
            && (b'0'..=b'7').contains(&value[index + 2])
            && (b'0'..=b'7').contains(&value[index + 3])
        {
            decoded.push(
                ((value[index + 1] - b'0') << 6)
                    | ((value[index + 2] - b'0') << 3)
                    | (value[index + 3] - b'0'),
            );
            index += 4;
            continue;
        }
        decoded.push(value[index + 1]);
        index += 2;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Decode tmux's `\\ooo` byte escaping used in `%output` notifications.
pub fn decode_octal(encoded: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] != b'\\' {
            decoded.push(encoded[index]);
            index += 1;
            continue;
        }
        if index + 3 >= encoded.len()
            || !encoded[index + 1].is_ascii_digit()
            || !(b'0'..=b'7').contains(&encoded[index + 1])
            || !(b'0'..=b'7').contains(&encoded[index + 2])
            || !(b'0'..=b'7').contains(&encoded[index + 3])
        {
            return Err(DecodeError::InvalidOctalEscape);
        }
        let byte = ((encoded[index + 1] - b'0') << 6)
            | ((encoded[index + 2] - b'0') << 3)
            | (encoded[index + 3] - b'0');
        decoded.push(byte);
        index += 4;
    }
    Ok(decoded)
}

/// `capture-pane -C` quotes a literal backslash as `\\`, unlike `%output`,
/// which encodes it as an octal byte. Keep the two protocol forms distinct.
pub fn decode_capture(encoded: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] != b'\\' {
            decoded.push(encoded[index]);
            index += 1;
        } else if encoded.get(index + 1) == Some(&b'\\') {
            decoded.push(b'\\');
            index += 2;
        } else {
            let escape = encoded
                .get(index..index + 4)
                .ok_or(DecodeError::InvalidOctalEscape)?;
            decoded.extend(decode_octal(escape)?);
            index += 4;
        }
    }
    Ok(decoded)
}

/// Build a Control Mode command. The session name is fixed by product
/// invariants, while pane/window IDs are validated before being interpolated.
pub fn initial_command() -> &'static [u8] {
    b"tmux -C -u new-session -A -s meeterm"
}

pub fn list_windows_command() -> &'static [u8] {
    b"list-windows -t =meeterm -F '#{window_id}\t#{q:window_name}'"
}

pub fn list_panes_command() -> &'static [u8] {
    b"list-panes -s -t =meeterm -F '#{window_id}\t#{pane_id}\t#{pane_index}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{q:pane_title}\t#{window_zoomed_flag}\t#{window_active}'"
}

pub fn refresh_client_command(columns: u16, rows: u16) -> String {
    format!("refresh-client -C {columns}x{rows}")
}

pub fn select_pane_command(
    previous_zoomed_pane: Option<u64>,
    window_id: u64,
    pane_id: u64,
) -> String {
    // The conditional commands make selection idempotent: collapse a stale
    // zoom before selecting an invisible pane, then zoom the target if needed.
    // All interpolated values are validated numeric IDs.
    let restore = previous_zoomed_pane
        .map(|id| {
            format!(
                "if-shell -F -t %{id} '#{{window_zoomed_flag}}' 'resize-pane -Z -t %{id}' '' ; "
            )
        })
        .unwrap_or_default();
    format!(
        "{restore}select-window -t @{window_id} ; select-pane -t %{pane_id} ; if-shell -F -t %{pane_id} '#{{window_zoomed_flag}}' '' 'resize-pane -Z -t %{pane_id}'"
    )
}

pub fn restore_layout_command(pane_id: u64) -> String {
    // The target condition makes this idempotent if a recovery hook already
    // returned the window to its normal layout.
    format!("if-shell -F -t %{pane_id} '#{{window_zoomed_flag}}' 'resize-pane -Z -t %{pane_id}' ''")
}

/// Choose an unused indexed hook slot for the two session-scoped recovery
/// hooks. The input is the byte output of `show-hooks -t =meeterm:`.
///
/// Every indexed hook in the reserved range is treated as occupied, even if
/// it belongs to another hook name. This is conservative and keeps meeterm
/// from reusing an index a user is already using. A malformed hook line makes
/// allocation fail closed because replacing an unknown hook is worse than
/// declining crash recovery for this connection.
pub fn choose_zoom_recovery_hook(hooks: &[u8]) -> Option<ZoomRecoveryHookAllocation> {
    let mut occupied = Vec::new();

    for line in hooks.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some((_name, index)) = parse_hook_entry(line).ok()? else {
            continue;
        };
        let index = index.unwrap_or(0);
        if (ZOOM_RECOVERY_HOOK_INDEX_START..ZOOM_RECOVERY_HOOK_INDEX_LIMIT).contains(&index)
            && !occupied.contains(&index)
        {
            occupied.push(index);
        }
    }

    let index = (ZOOM_RECOVERY_HOOK_INDEX_START..ZOOM_RECOVERY_HOOK_INDEX_LIMIT)
        .find(|index| !occupied.contains(index))?;
    Some(ZoomRecoveryHookAllocation { index })
}

/// Install the session-scoped recovery pair. The hook is intentionally
/// limited to `=meeterm:` and to one numeric pane ID. It unzooms that pane's
/// window only when `window_zoomed_flag` is still set, then removes both
/// indexed hooks. An empty index-0 placeholder is harmless and remains;
/// unsetting an unindexed hook would delete unrelated user entries.
pub fn install_zoom_recovery_hooks_command(
    allocation: ZoomRecoveryHookAllocation,
    pane_id: u64,
) -> String {
    let body = zoom_recovery_hook_body(allocation, pane_id);
    format!(
        "set-hook -t =meeterm: {ZOOM_RECOVERY_DETACHED_HOOK}[{}] '{body}' ; set-hook -t =meeterm: {ZOOM_RECOVERY_SESSION_CHANGED_HOOK}[{}] '{body}'",
        allocation.index, allocation.index
    )
}

/// Remove the indexed recovery pair without changing the current layout.
pub fn remove_zoom_recovery_hooks_command(allocation: ZoomRecoveryHookAllocation) -> String {
    let commands = [
        format!(
            "set-hook -u -t =meeterm: {ZOOM_RECOVERY_DETACHED_HOOK}[{}]",
            allocation.index
        ),
        format!(
            "set-hook -u -t =meeterm: {ZOOM_RECOVERY_SESSION_CHANGED_HOOK}[{}]",
            allocation.index
        ),
    ];
    commands.join(" ; ")
}

/// Restore a meeterm-owned zoom and remove its recovery hooks during an
/// orderly disconnect. The conditional restore makes this safe when another
/// command has already returned the window to its normal layout.
pub fn cleanup_zoom_recovery_hooks_command(
    allocation: ZoomRecoveryHookAllocation,
    pane_id: u64,
) -> String {
    format!(
        "if-shell -F -t %{pane_id} \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t %{pane_id}\" ; {}",
        remove_zoom_recovery_hooks_command(allocation)
    )
}

fn zoom_recovery_hook_body(allocation: ZoomRecoveryHookAllocation, pane_id: u64) -> String {
    let mut commands = vec![format!(
        "if-shell -F -t %{pane_id} \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t %{pane_id}\""
    )];
    commands.extend(remove_zoom_recovery_commands(allocation));
    commands.join(" ; ")
}

fn remove_zoom_recovery_commands(allocation: ZoomRecoveryHookAllocation) -> Vec<String> {
    let commands = vec![
        format!(
            "set-hook -u -t =meeterm: {ZOOM_RECOVERY_DETACHED_HOOK}[{}]",
            allocation.index
        ),
        format!(
            "set-hook -u -t =meeterm: {ZOOM_RECOVERY_SESSION_CHANGED_HOOK}[{}]",
            allocation.index
        ),
    ];
    commands
}

type HookEntry<'a> = (&'a [u8], Option<u32>);

fn parse_hook_entry(line: &[u8]) -> Result<Option<HookEntry<'_>>, ()> {
    let token = line
        .split(|byte| byte.is_ascii_whitespace())
        .find(|field| !field.is_empty())
        .unwrap_or_default();
    if token.is_empty() {
        return Ok(None);
    }
    let Some(open) = token.iter().position(|byte| *byte == b'[') else {
        if token.contains(&b']') {
            return Err(());
        }
        return Ok(Some((token, None)));
    };
    if !token.ends_with(b"]") || open == 0 || open + 1 >= token.len() - 1 {
        return Err(());
    }
    let name = &token[..open];
    let digits = &token[open + 1..token.len() - 1];
    // Recent tmux versions support named array keys. They cannot collide
    // with our numeric range and must not disable an otherwise valid session.
    if digits.contains(&b'[') || digits.contains(&b']') {
        return Err(());
    }
    if !digits.iter().all(u8::is_ascii_digit) {
        return Ok(Some((name, None)));
    }
    let index = digits.iter().try_fold(0_u32, |value, byte| {
        if !byte.is_ascii_digit() {
            return Err(());
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(byte - b'0')))
            .ok_or(())
    })?;
    Ok(Some((name, Some(index))))
}

pub fn send_bytes_command(pane_id: u64, bytes: &[u8]) -> String {
    let mut command = format!("send-keys -t %{pane_id} -H");
    for byte in bytes {
        command.push_str(&format!(" {byte:02x}"));
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_quotes_backslashes_differently_from_output_notifications() {
        assert_eq!(
            decode_capture(br"literal\\033 \\xyz \033[31m").unwrap(),
            b"literal\\033 \\xyz \x1b[31m"
        );
        assert_eq!(decode_octal(br"literal\134033").unwrap(), b"literal\\033");
    }

    #[test]
    fn decodes_fragmented_output_and_preserves_spaces() {
        let mut decoder = Decoder::new();
        assert!(decoder.feed(b"%output %7 hello\\040").unwrap().is_empty());
        let events = decoder.feed(b"world\\012\\134\\141\n").unwrap();
        assert_eq!(
            events,
            vec![Event::Output {
                pane_id: 7,
                bytes: b"hello world\n\\a".to_vec()
            }]
        );
    }

    #[test]
    fn output_payload_may_be_empty_or_only_spaces() {
        let mut decoder = Decoder::new();
        let events = decoder.feed(b"%output %0  \n%output %1 \n").unwrap();
        assert_eq!(
            events,
            vec![
                Event::Output {
                    pane_id: 0,
                    bytes: b" ".to_vec(),
                },
                Event::Output {
                    pane_id: 1,
                    bytes: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn command_blocks_and_notifications_do_not_mix() {
        let mut decoder = Decoder::new();
        let events = decoder
            .feed(b"%begin 1 42 0\n@0\tmain\n%end 1 42 0\n%window-add @1\n")
            .unwrap();
        assert_eq!(
            events,
            vec![
                Event::Command(CommandBlock {
                    number: 42,
                    lines: vec![b"@0\tmain".to_vec()],
                    error: false,
                }),
                Event::Notification {
                    name: "window-add".to_owned(),
                    arguments: vec![b"@1".to_vec()]
                }
            ]
        );
    }

    #[test]
    fn marker_like_capture_lines_stay_inside_the_command_block() {
        let mut decoder = Decoder::new();
        let events = decoder
            .feed(
                b"%begin 10 42 0\n%begin 11 99 0\n%end 11 99 0\n%end 10 42 1\n%end 10 41 0\nbody\n%end 10 42 0\n",
            )
            .unwrap();
        assert_eq!(
            events,
            vec![Event::Command(CommandBlock {
                number: 42,
                lines: vec![
                    b"%begin 11 99 0".to_vec(),
                    b"%end 11 99 0".to_vec(),
                    b"%end 10 42 1".to_vec(),
                    b"%end 10 41 0".to_vec(),
                    b"body".to_vec(),
                ],
                error: false,
            })]
        );
    }

    #[test]
    fn adjacent_command_blocks_are_framed_independently() {
        let mut decoder = Decoder::new();
        let events = decoder
            .feed(b"%begin 1 1 0\nfirst\n%end 1 1 0\n%begin 2 2 0\nsecond\n%error 2 2 0\n")
            .unwrap();
        assert_eq!(
            events,
            vec![
                Event::Command(CommandBlock {
                    number: 1,
                    lines: vec![b"first".to_vec()],
                    error: false,
                }),
                Event::Command(CommandBlock {
                    number: 2,
                    lines: vec![b"second".to_vec()],
                    error: true,
                }),
            ]
        );
    }

    #[test]
    fn extended_output_uses_the_first_protocol_colon() {
        let mut decoder = Decoder::new();
        let events = decoder
            .feed(b"%extended-output %7 12 : first:second\\040\n")
            .unwrap();
        assert_eq!(
            events,
            vec![Event::Output {
                pane_id: 7,
                bytes: b"first:second ".to_vec(),
            }]
        );
    }

    #[test]
    fn rejects_malformed_escape_and_ids() {
        assert_eq!(
            decode_octal(b"bad\\08x"),
            Err(DecodeError::InvalidOctalEscape)
        );
        assert_eq!(parse_pane_id(b"%"), Err(DecodeError::InvalidPaneId));
        assert_eq!(parse_pane_id(b"%a"), Err(DecodeError::InvalidPaneId));
        assert_eq!(
            parse_window_id(b"%1"),
            Err(DecodeError::InvalidNotification)
        );
    }

    #[test]
    fn selection_command_contains_only_validated_numeric_target() {
        assert_eq!(
            select_pane_command(None, 4, 12),
            "select-window -t @4 ; select-pane -t %12 ; if-shell -F -t %12 '#{window_zoomed_flag}' '' 'resize-pane -Z -t %12'"
        );
        assert_eq!(
            select_pane_command(Some(7), 4, 12),
            "if-shell -F -t %7 '#{window_zoomed_flag}' 'resize-pane -Z -t %7' '' ; select-window -t @4 ; select-pane -t %12 ; if-shell -F -t %12 '#{window_zoomed_flag}' '' 'resize-pane -Z -t %12'"
        );
        assert_eq!(
            restore_layout_command(12),
            "if-shell -F -t %12 '#{window_zoomed_flag}' 'resize-pane -Z -t %12' ''"
        );
    }

    #[test]
    fn quoted_window_and_pane_fields_are_decoded_without_delimiter_loss() {
        let window = parse_window_line(b"@4\twork\\040desk\\011x\\012y\\134").unwrap();
        assert_eq!(window.name, "work desk\tx\ny\\");

        let pane = parse_pane_line(b"@4\t%8\t2\t0\t80\t24\ttab\\011line\\134q\t1\t0").unwrap();
        assert_eq!(pane.window_id, 4);
        assert_eq!(pane.pane_id, 8);
        assert_eq!(pane.title, "tab\tline\\q");
        assert!(pane.zoomed);
        assert!(!pane.window_active);
    }

    #[test]
    fn hook_allocator_preserves_existing_indices_and_base_hooks() {
        let hooks = b"client-detached[0] display-message user\nclient-session-changed[1000] user\npane-died[1001] user\nwindow-renamed\n";
        let allocation = choose_zoom_recovery_hook(hooks).unwrap();
        assert_eq!(allocation.index, 1002);

        assert!(choose_zoom_recovery_hook(b"pane-died[message] user\n").is_some());
        assert!(choose_zoom_recovery_hook(b"pane-died[broken[1] user\n").is_none());
    }

    #[test]
    fn zoom_recovery_commands_are_session_scoped_and_numeric() {
        let allocation = ZoomRecoveryHookAllocation { index: 1002 };
        let install = install_zoom_recovery_hooks_command(allocation, 23);
        assert!(install.starts_with("set-hook -t =meeterm: client-detached[1002] '"));
        assert!(install.contains("client-session-changed[1002]"));
        assert!(
            install
                .contains("if-shell -F -t %23 \"#{window_zoomed_flag}\" \"resize-pane -Z -t %23\"")
        );
        assert!(install.contains("set-hook -u -t =meeterm: client-detached[1002]"));
        assert!(install.contains("set-hook -u -t =meeterm: client-session-changed[1002]"));
        assert!(!install.contains("set-hook -u -t =meeterm: client-detached'"));
        assert!(!install.contains("set-hook -u -t =meeterm: client-session-changed'"));

        let cleanup = cleanup_zoom_recovery_hooks_command(allocation, 23);
        assert!(cleanup.starts_with("if-shell -F -t %23"));
        assert!(cleanup.contains("client-detached[1002]"));
    }
}
