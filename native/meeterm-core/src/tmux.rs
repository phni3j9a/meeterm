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

/// Runtime discovery bounds. These limits apply before a candidate is
/// copied into the native picker snapshot.
pub const MAX_RUNTIME_SESSIONS: usize = 256;
pub const MAX_SESSION_NAME_BYTES: usize = 4096;
const MAX_CREATE_NAME_BYTES: usize = 64;

/// The exact tmux session identity retained by a selected connection. A
/// `$N` id is only meaningful during the server epoch in which it was listed;
/// the server PID alone is not sufficient because the OS may reuse it. The
/// PID/start-time pair is retained separately from the display-name hint for
/// stale/replacement checks.
#[derive(Clone, Debug)]
pub(crate) struct SessionIdentity {
    pub(crate) session_id: String,
    pub(crate) name: String,
    pub(crate) server_pid: u64,
    pub(crate) server_start_time: u64,
}

impl PartialEq for SessionIdentity {
    fn eq(&self, other: &Self) -> bool {
        // Session names are mutable display hints. The runtime identity is
        // the tmux session ID within the server PID/start-time epoch.
        self.session_id == other.session_id
            && self.server_pid == other.server_pid
            && self.server_start_time == other.server_start_time
    }
}

impl Eq for SessionIdentity {}

impl SessionIdentity {
    pub(crate) fn epoch(&self) -> SessionEpoch {
        SessionEpoch {
            session_id: self.session_id.clone(),
            server_pid: self.server_pid,
            server_start_time: self.server_start_time,
        }
    }
}

/// The server-epoch portion of a session identity returned over an already
/// attached Control Mode stream. The display name is intentionally omitted:
/// it is mutable and is not part of the runtime identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionEpoch {
    pub(crate) session_id: String,
    pub(crate) server_pid: u64,
    pub(crate) server_start_time: u64,
}

impl SessionEpoch {
    pub(crate) fn matches(&self, expected: &SessionIdentity) -> bool {
        self.session_id == expected.session_id
            && self.server_pid == expected.server_pid
            && self.server_start_time == expected.server_start_time
    }
}

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
    /// The pane selected inside its own tmux window.  This remains true for
    /// every window's active pane; `selected` is reserved for the one pane
    /// selected by the mobile client/session.
    pub active: bool,
    pub selected: bool,
    pub index: u32,
    pub columns: u16,
    pub rows: u16,
    /// The tmux pane title (`select-pane -T`) exposed as the terminal-tab
    /// name in the native control plane.
    pub pane_name: String,
    /// Backwards-compatible alias retained for older native callers.
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
    pub(crate) pane_name: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZoomRecoveryHookState {
    Absent,
    Owned,
    Replaced,
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

impl fmt::Display for SessionIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.session_id)
    }
}

/// Errors returned while encoding a user-visible tmux name as one command
/// argument. Names are bounded and cannot contain control characters because
/// the Control Mode command stream is line-oriented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandArgumentError {
    Empty,
    TooLong,
    ControlCharacter,
}

impl fmt::Display for CommandArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "tmux name is empty",
            Self::TooLong => "tmux name is too long",
            Self::ControlCharacter => "tmux name contains a control character",
        })
    }
}

impl std::error::Error for CommandArgumentError {}

/// Quote one tmux command argument without allowing parser metacharacters to
/// become command separators or expansions. tmux's command parser expands
/// `$VAR` and `#()`/`#{...}` after tokenization, including inside double
/// quotes. A single-quoted argument suppresses `$` expansion; `##` is tmux's
/// documented literal-`#` format escape. The standard `'<quote>'` splice
/// keeps apostrophes inside the same argument.
pub fn quote_tmux_argument(value: &str) -> Result<String, CommandArgumentError> {
    const MAX_NAME_BYTES: usize = 4096;
    if value.is_empty() {
        return Err(CommandArgumentError::Empty);
    }
    if value.len() > MAX_NAME_BYTES {
        return Err(CommandArgumentError::TooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(CommandArgumentError::ControlCharacter);
    }

    let mut quoted = String::with_capacity(value.len().saturating_add(2));
    quoted.push('\'');
    for character in value.chars() {
        match character {
            '\'' => quoted.push_str("'\\''"),
            // `##` is reduced to one literal `#` by tmux's format parser and
            // prevents both `#{...}` lookup and `#()` command substitution.
            '#' => quoted.push_str("##"),
            _ => quoted.push(character),
        }
    }
    quoted.push('\'');
    Ok(quoted)
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

/// Split the four identity fields emitted by shell-level tmux discovery.
///
/// OpenSSH replaces control characters in an exec command with `_`, so the
/// shell-level format uses printable `|` separators. The session name is the
/// only free-form field and may itself contain an escaped pipe; taking the
/// first separator and the final two separators keeps that name opaque until
/// tmux's `q:` encoding is decoded. Tab input remains accepted for the
/// Control Mode fixtures and older captured evidence.
fn split_identity_fields(line: &[u8]) -> Result<[&[u8]; 4], DecodeError> {
    if line.contains(&b'\t') {
        let fields = line.split(|byte| *byte == b'\t').collect::<Vec<_>>();
        return fields
            .try_into()
            .map_err(|_| DecodeError::InvalidNotification);
    }

    let first = line
        .iter()
        .position(|byte| *byte == b'|')
        .ok_or(DecodeError::InvalidNotification)?;
    let last = line
        .iter()
        .rposition(|byte| *byte == b'|')
        .ok_or(DecodeError::InvalidNotification)?;
    let middle = line[..last]
        .iter()
        .rposition(|byte| *byte == b'|')
        .ok_or(DecodeError::InvalidNotification)?;
    if first >= middle || middle >= last {
        return Err(DecodeError::InvalidNotification);
    }
    Ok([
        &line[..first],
        &line[first + 1..middle],
        &line[middle + 1..last],
        &line[last + 1..],
    ])
}

/// Parse the fixed, side-effect-free session discovery format:
/// `$N|quoted name|server pid|server start time`.
pub(crate) fn parse_session_line(line: &[u8]) -> Result<SessionIdentity, DecodeError> {
    let fields = split_identity_fields(line)?;
    let session_id = parse_session_id(fields[0])?;
    let name = decode_tmux_quoted_strict(fields[1])?;
    if name.is_empty() || name.len() > MAX_SESSION_NAME_BYTES || name.chars().any(char::is_control)
    {
        return Err(DecodeError::InvalidNotification);
    }
    let server_pid = parse_nonzero_u64(fields[2])?;
    let server_start_time = parse_nonzero_u64(fields[3])?;
    Ok(SessionIdentity {
        session_id,
        name,
        server_pid,
        server_start_time,
    })
}

/// Parse the bounded identity format emitted by the post-attach epoch query:
/// `$N|server pid|server start time`.
pub(crate) fn parse_session_epoch_line(line: &[u8]) -> Result<SessionEpoch, DecodeError> {
    const MAX_EPOCH_LINE_BYTES: usize = 128;
    if line.is_empty() || line.len() > MAX_EPOCH_LINE_BYTES {
        return Err(DecodeError::InvalidNotification);
    }
    let fields = line.split(|byte| *byte == b'|').collect::<Vec<_>>();
    let [session_id, server_pid, server_start_time] = fields
        .try_into()
        .map_err(|_| DecodeError::InvalidNotification)?;
    Ok(SessionEpoch {
        session_id: parse_session_id(session_id)?,
        server_pid: parse_nonzero_u64(server_pid)?,
        server_start_time: parse_nonzero_u64(server_start_time)?,
    })
}

fn parse_session_id(value: &[u8]) -> Result<String, DecodeError> {
    value
        .strip_prefix(b"$")
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| parse_decimal_u64(digits).ok())
        .map(|number| format!("${number}"))
        .ok_or(DecodeError::InvalidNotification)
}

fn parse_nonzero_u64(value: &[u8]) -> Result<u64, DecodeError> {
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value != 0)
        .ok_or(DecodeError::InvalidNotification)
}

/// A tmux command returns exit status 1 when the ordinary server has not
/// been started (or when it has no sessions). Only this narrow diagnostic is
/// considered a verified empty section; every other non-zero result remains a
/// backend error.
pub(crate) fn is_verified_no_server(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let message = message.trim();
    message == "no server running"
        || message
            .strip_prefix("no server running on ")
            .is_some_and(|path| !path.is_empty())
        || message == "no sessions"
}

pub(crate) fn is_session_collision(stderr: &[u8]) -> bool {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    message.contains("duplicate session")
        || message.contains("session already exists")
        || message.contains("already exists")
}

/// Return true for the shell's conventional command-not-found status or its
/// usual diagnostic. This is used only to label the tmux picker section.
pub(crate) fn is_command_missing(status: Option<u32>, stderr: &[u8]) -> bool {
    if status == Some(127) {
        return true;
    }
    String::from_utf8_lossy(stderr)
        .to_ascii_lowercase()
        .contains("command not found")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowTopology {
    pub(crate) session_id: String,
    pub(crate) session_name: String,
    pub(crate) window_id: u64,
    pub(crate) linked_sessions: u32,
}

pub(crate) fn parse_topology_line(line: &[u8]) -> Result<WindowTopology, DecodeError> {
    let fields = split_identity_fields(line)?;
    let session_id = fields[0]
        .strip_prefix(b"$")
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| parse_decimal_u64(digits).ok())
        .map(|number| format!("${number}"))
        .ok_or(DecodeError::InvalidNotification)?;
    let session_name = decode_tmux_quoted_strict(fields[1])?;
    if session_name.is_empty()
        || session_name.len() > MAX_SESSION_NAME_BYTES
        || session_name.chars().any(char::is_control)
    {
        return Err(DecodeError::InvalidNotification);
    }
    let window_id = parse_window_id(fields[2])?;
    let linked_sessions = std::str::from_utf8(fields[3])
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|count| *count > 0)
        .ok_or(DecodeError::InvalidNotification)?;
    Ok(WindowTopology {
        session_id,
        session_name,
        window_id,
        linked_sessions,
    })
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
        pane_name: decode_tmux_quoted(fields[6]),
        title: decode_tmux_quoted(fields[6]),
        zoomed: parse_flag(fields[7])?,
        window_active: fields.get(8).map_or(Ok(true), |field| parse_flag(field))?,
    })
}

/// Decode the backslash quoting emitted by tmux's `q:` format modifier. The
/// format result has already passed through tmux's format-output escaping.
/// tmux 3.3 and 3.4 differ by one escaping layer for some fields, so a
/// literal backslash may be represented by two or four backslashes (and a
/// backslash before an escaped space by three or five). Decode runs as a
/// unit instead of consuming pairs independently so either form does not
/// leave spurious backslashes in a user-visible workspace name.
fn decode_tmux_quoted(value: &[u8]) -> String {
    String::from_utf8_lossy(&decode_tmux_quoted_bytes(value)).into_owned()
}

fn decode_tmux_quoted_strict(value: &[u8]) -> Result<String, DecodeError> {
    let trailing_slashes = value
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    if trailing_slashes % 2 != 0 {
        return Err(DecodeError::InvalidNotification);
    }
    String::from_utf8(decode_tmux_quoted_bytes(value)).map_err(|_| DecodeError::InvalidNotification)
}

fn decode_tmux_quoted_bytes(value: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] != b'\\' {
            decoded.push(value[index]);
            index += 1;
            continue;
        }

        let run_start = index;
        while index < value.len() && value[index] == b'\\' {
            index += 1;
        }
        let slash_count = index - run_start;

        // Keep compatibility with the octal form used by older tmux format
        // output and by the parser fixture. It is an escape only when one
        // backslash introduces the three octal digits; a doubled run is a
        // literal backslash followed by ordinary digits.
        if slash_count == 1
            && index + 2 < value.len()
            && (b'0'..=b'7').contains(&value[index])
            && (b'0'..=b'7').contains(&value[index + 1])
            && (b'0'..=b'7').contains(&value[index + 2])
        {
            decoded.push(
                ((value[index] - b'0') << 6)
                    | ((value[index + 1] - b'0') << 3)
                    | (value[index + 2] - b'0'),
            );
            index += 3;
            continue;
        }

        let next = value.get(index).copied();
        // `$` has one extra format-output escape group when it is followed by
        // a variable-like word. A bare dollar and the first one or two format
        // groups are the dollar itself; further groups represent literal
        // backslashes before it.
        let literal_slashes = if next == Some(b'$') {
            slash_count.saturating_sub(4).saturating_add(3) / 4
        } else {
            slash_count.saturating_add(2) / 4
        };
        decoded.extend(std::iter::repeat_n(b'\\', literal_slashes));
        if let Some(next) = next {
            decoded.push(next);
            index += 1;
        }
    }
    decoded
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

/// Build a remote, side-effect-free session list command. `#{pid}` and
/// `#{start_time}` are kept as native server-epoch evidence and are never
/// exposed in the picker snapshot.
pub fn list_sessions_command() -> &'static str {
    "tmux list-sessions -F '#{session_id}|#{q:session_name}|#{pid}|#{start_time}'"
}

/// Explicitly create one detached session and print its exact identity. This
/// is intentionally separate from the attach path: callers must verify the
/// returned `$N` before attaching and must not retry an unknown outcome.
pub(crate) fn create_session_command(name: &str) -> Result<String, CommandArgumentError> {
    validate_create_name(name)?;
    Ok(format!(
        "tmux new-session -d -s {} -P -F '#{{session_id}}|#{{q:session_name}}|#{{pid}}|#{{start_time}}'",
        quote_tmux_argument(name)?
    ))
}

/// Validate names accepted by the explicit tmux create form. Existing names
/// discovered from tmux deliberately use the looser parser above and are not
/// rejected just because they would not be accepted by this UI form.
pub(crate) fn validate_create_name(name: &str) -> Result<(), CommandArgumentError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err(CommandArgumentError::Empty);
    }
    if bytes.len() > MAX_CREATE_NAME_BYTES {
        return Err(CommandArgumentError::TooLong);
    }
    if !bytes[0].is_ascii_alphanumeric()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CommandArgumentError::ControlCharacter);
    }
    Ok(())
}

fn session_target(session: &str) -> Result<String, CommandArgumentError> {
    if session.is_empty() || session.len() > MAX_SESSION_NAME_BYTES {
        return Err(if session.is_empty() {
            CommandArgumentError::Empty
        } else {
            CommandArgumentError::TooLong
        });
    }
    quote_tmux_argument(&format!("={session}"))
}

pub(crate) fn session_target_for_hooks(session: &str) -> Result<String, CommandArgumentError> {
    session_target(session)
}

fn window_target(session: &str, window_id: u64) -> Result<String, CommandArgumentError> {
    quote_tmux_argument(&format!("={session}:@{window_id}"))
}

fn pane_target(
    session: &str,
    window_id: u64,
    pane_id: u64,
) -> Result<String, CommandArgumentError> {
    quote_tmux_argument(&format!("={session}:@{window_id}.%{pane_id}"))
}

fn pane_only_target(session: &str, pane_id: u64) -> Result<String, CommandArgumentError> {
    // tmux target syntax is session:window.pane. An empty/current window is
    // written as `.` before the stable `%N` pane ID; `session:%N` treats the
    // pane ID as a window target and fails with "can't find window".
    quote_tmux_argument(&format!("={session}:.%{pane_id}"))
}

pub(crate) fn attach_command_for_session(session: &str) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "tmux -C -u attach-session -t {}",
        session_target(session)?
    ))
}

/// Read the current client's attached session identity from the same Control
/// Mode stream used for pane synchronization. `display-message -t` expects a
/// pane target and loses session context for an exact `$N`, so this query must
/// deliberately use the calling control client's current session.
pub(crate) fn attached_session_epoch_command() -> &'static str {
    "display-message -p '#{session_id}|#{pid}|#{start_time}'"
}

pub(crate) fn list_windows_command_for_session(
    session: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "list-windows -t {} -F '#{{window_id}}\t#{{q:window_name}}'",
        session_target(session)?
    ))
}

pub(crate) fn list_panes_command_for_session(
    session: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "list-panes -s -t {} -F '#{{window_id}}\t#{{pane_id}}\t#{{pane_index}}\t#{{pane_active}}\t#{{pane_width}}\t#{{pane_height}}\t#{{q:pane_title}}\t#{{window_zoomed_flag}}\t#{{window_active}}'",
        session_target(session)?
    ))
}

pub(crate) fn capture_pane_command_for_session(
    session: &str,
    pane_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = pane_only_target(session, pane_id)?;
    Ok(format!(
        "capture-pane -p -e -C -N -S -2000 -t {target} ; display-message -p -t {target} '#{{pane_width}},#{{pane_height}},#{{cursor_x}},#{{cursor_y}},#{{alternate_on}},#{{cursor_flag}},#{{keypad_cursor_flag}},#{{keypad_flag}},#{{?bracket_paste_flag,1,0}},#{{insert_flag}},#{{origin_flag}},#{{wrap_flag}}'"
    ))
}

/// List every linked window at the action boundary. This is deliberately
/// broader than the selected session so an unsafe `kill-window`/`kill-pane`
/// can fail closed before it mutates another session.
pub(crate) fn list_topology_command() -> &'static str {
    "list-windows -a -F '#{session_id}\t#{q:session_name}\t#{window_id}\t#{window_linked_sessions}'"
}

#[allow(dead_code)]
pub fn list_windows_command() -> &'static [u8] {
    b"list-windows -t =meeterm -F '#{window_id}\t#{q:window_name}'"
}

#[allow(dead_code)]
pub fn list_panes_command() -> &'static [u8] {
    b"list-panes -s -t =meeterm -F '#{window_id}\t#{pane_id}\t#{pane_index}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{q:pane_title}\t#{window_zoomed_flag}\t#{window_active}'"
}

/// Create a detached workspace window in the managed session. The exact
/// session target prevents a similarly named session from receiving it.
#[allow(dead_code)]
pub fn create_workspace_command(name: &str) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "new-window -d -t =meeterm -n {}",
        quote_tmux_argument(name)?
    ))
}

pub(crate) fn create_workspace_command_for_session(
    session: &str,
    name: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "new-window -d -t {} -n {}",
        session_target(session)?,
        quote_tmux_argument(name)?
    ))
}

#[allow(dead_code)]
pub fn rename_workspace_command(
    window_id: u64,
    name: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "rename-window -t =meeterm:@{window_id} {}",
        quote_tmux_argument(name)?
    ))
}

pub(crate) fn rename_workspace_command_for_session(
    session: &str,
    window_id: u64,
    name: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "rename-window -t {} {}",
        window_target(session, window_id)?,
        quote_tmux_argument(name)?
    ))
}

#[allow(dead_code)]
pub fn close_workspace_command(window_id: u64) -> String {
    format!("kill-window -t =meeterm:@{window_id}")
}

pub(crate) fn close_workspace_command_for_session(
    session: &str,
    window_id: u64,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "kill-window -t {}",
        window_target(session, window_id)?
    ))
}

/// Create a detached pane in a numeric target window. Using `-d` keeps the
/// currently selected mobile pane and the desktop layout stable.
#[allow(dead_code)]
pub fn create_pane_command(window_id: u64) -> String {
    format!("split-window -d -t =meeterm:@{window_id}")
}

pub(crate) fn create_pane_command_for_session(
    session: &str,
    window_id: u64,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "split-window -d -t {}",
        window_target(session, window_id)?
    ))
}

#[allow(dead_code)]
pub fn rename_pane_command(
    window_id: u64,
    pane_id: u64,
    name: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "select-pane -t =meeterm:@{window_id}.%{pane_id} -T {}",
        quote_tmux_argument(name)?
    ))
}

pub(crate) fn rename_pane_command_for_session(
    session: &str,
    window_id: u64,
    pane_id: u64,
    name: &str,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "select-pane -t {} -T {}",
        pane_target(session, window_id, pane_id)?,
        quote_tmux_argument(name)?
    ))
}

#[allow(dead_code)]
pub fn close_pane_command(window_id: u64, pane_id: u64) -> String {
    format!("kill-pane -t =meeterm:@{window_id}.%{pane_id}")
}

pub(crate) fn close_pane_command_for_session(
    session: &str,
    window_id: u64,
    pane_id: u64,
) -> Result<String, CommandArgumentError> {
    Ok(format!(
        "kill-pane -t {}",
        pane_target(session, window_id, pane_id)?
    ))
}

pub fn refresh_client_command(columns: u16, rows: u16) -> String {
    format!("refresh-client -C {columns}x{rows}")
}

#[allow(dead_code)]
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

pub(crate) fn select_pane_command_for_session(
    session: &str,
    previous_zoomed_pane: Option<u64>,
    window_id: u64,
    pane_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = pane_only_target(session, pane_id)?;
    let window = window_target(session, window_id)?;
    let restore = previous_zoomed_pane
        .map(|id| {
            pane_only_target(session, id).map(|target| {
                format!(
                    "if-shell -F -t {target} '#{{window_zoomed_flag}}' 'resize-pane -Z -t {target}' '' ; "
                )
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(format!(
        "{restore}select-window -t {window} ; select-pane -t {target} ; if-shell -F -t {target} '#{{window_zoomed_flag}}' '' 'resize-pane -Z -t {target}'"
    ))
}

#[allow(dead_code)]
pub fn restore_layout_command(pane_id: u64) -> String {
    // The target condition makes this idempotent if a recovery hook already
    // returned the window to its normal layout.
    format!("if-shell -F -t %{pane_id} '#{{window_zoomed_flag}}' 'resize-pane -Z -t %{pane_id}' ''")
}

#[allow(dead_code)]
pub(crate) fn restore_layout_command_for_session(
    session: &str,
    pane_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = pane_only_target(session, pane_id)?;
    Ok(format!(
        "if-shell -F -t {target} '#{{window_zoomed_flag}}' 'resize-pane -Z -t {target}' ''"
    ))
}

/// Restore a zoomed window when the pane that originally owned the zoom has
/// disappeared. The window target is still scoped to the selected session;
/// callers must first prove that the window exists in authoritative topology.
pub(crate) fn restore_layout_command_for_session_window(
    session: &str,
    window_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = window_target(session, window_id)?;
    Ok(format!(
        "if-shell -F -t {target} '#{{window_zoomed_flag}}' 'resize-pane -Z -t {target}' ''"
    ))
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
#[allow(dead_code)]
pub fn install_zoom_recovery_hooks_command(
    allocation: ZoomRecoveryHookAllocation,
    window_id: u64,
) -> String {
    let body = zoom_recovery_hook_body(allocation, window_id);
    format!(
        "set-hook -t =meeterm: {ZOOM_RECOVERY_DETACHED_HOOK}[{}] '{body}' ; set-hook -t =meeterm: {ZOOM_RECOVERY_SESSION_CHANGED_HOOK}[{}] '{body}'",
        allocation.index, allocation.index
    )
}

pub(crate) fn install_zoom_recovery_hooks_command_for_session(
    session: &str,
    allocation: ZoomRecoveryHookAllocation,
    window_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = session_target(session)?;
    let body = zoom_recovery_hook_body_for_session(session, allocation, window_id)?;
    Ok(format!(
        "set-hook -t {target}: {ZOOM_RECOVERY_DETACHED_HOOK}[{}] '{body}' ; set-hook -t {target}: {ZOOM_RECOVERY_SESSION_CHANGED_HOOK}[{}] '{body}'",
        allocation.index, allocation.index
    ))
}

/// Remove the indexed recovery pair without changing the current layout.
#[allow(dead_code)]
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

pub(crate) fn remove_zoom_recovery_hooks_command_for_session(
    session: &str,
    allocation: ZoomRecoveryHookAllocation,
) -> Result<String, CommandArgumentError> {
    let target = session_target(session)?;
    let commands = [
        format!(
            "set-hook -u -t {target}: {ZOOM_RECOVERY_DETACHED_HOOK}[{}]",
            allocation.index
        ),
        format!(
            "set-hook -u -t {target}: {ZOOM_RECOVERY_SESSION_CHANGED_HOOK}[{}]",
            allocation.index
        ),
    ];
    Ok(commands.join(" ; "))
}

/// Restore a meeterm-owned zoom and remove its recovery hooks during an
/// orderly disconnect. The conditional restore makes this safe when another
/// command has already returned the window to its normal layout.
#[allow(dead_code)]
pub fn cleanup_zoom_recovery_hooks_command(
    allocation: ZoomRecoveryHookAllocation,
    pane_id: u64,
) -> String {
    format!(
        "if-shell -F -t %{pane_id} \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t %{pane_id}\" ; {}",
        remove_zoom_recovery_hooks_command(allocation)
    )
}

#[allow(dead_code)]
pub(crate) fn cleanup_zoom_recovery_hooks_command_for_session(
    session: &str,
    allocation: ZoomRecoveryHookAllocation,
    window_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = window_target(session, window_id)?;
    let remove = remove_zoom_recovery_hooks_command_for_session(session, allocation)?;
    Ok(format!(
        "if-shell -F -t {target} \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t {target}\" ; {remove}"
    ))
}

#[allow(dead_code)]
fn zoom_recovery_hook_body(allocation: ZoomRecoveryHookAllocation, window_id: u64) -> String {
    let target = format!("={}:@{}", SESSION_NAME, window_id);
    let mut commands = vec![format!(
        "if-shell -F -t '{target}' \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t '{target}'\""
    )];
    commands.extend(remove_zoom_recovery_commands(allocation));
    commands.join(" ; ")
}

fn zoom_recovery_hook_body_for_session(
    session: &str,
    allocation: ZoomRecoveryHookAllocation,
    window_id: u64,
) -> Result<String, CommandArgumentError> {
    let target = window_target(session, window_id)?;
    let mut commands = vec![format!(
        "if-shell -F -t {target} \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t {target}\""
    )];
    commands.push(remove_zoom_recovery_hooks_command_for_session(
        session, allocation,
    )?);
    Ok(commands.join(" ; "))
}

#[allow(dead_code)]
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

/// Confirm that both indexed hooks allocated by meeterm are absent. Other
/// user hooks are intentionally ignored so cleanup never treats their
/// presence as a failure or removes them.
pub(crate) fn zoom_recovery_hooks_absent(
    hooks: &[u8],
    allocation: ZoomRecoveryHookAllocation,
) -> Result<bool, ()> {
    for line in hooks.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some((name, index)) = parse_hook_entry(line)? else {
            continue;
        };
        let is_owned_name = name == ZOOM_RECOVERY_DETACHED_HOOK.as_bytes()
            || name == ZOOM_RECOVERY_SESSION_CHANGED_HOOK.as_bytes();
        if is_owned_name && index == Some(allocation.index) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Classify only the two exact indexed entries owned by one cleanup record.
/// A matching name/index with a different body is treated as replaced rather
/// than deleted: another client may have taken the slot after a transport
/// loss, and `set-hook -u` would otherwise remove third-party state.
///
/// tmux re-parses the stored hook body when rendering `show-hooks`, so the
/// exact quoting we sent is not preserved. A numeric session target sent as
/// `-t '=$0':` is rendered `-t "=$0:"`, while a name target loses its quotes
/// entirely (`-t =meeterm:`), and quotes inside double-quoted arguments
/// remain. Comparisons therefore ignore both quote characters on both sides.
/// Session names are restricted to `[0-9A-Za-z._-]` and window IDs are
/// numeric, so stripping quotes cannot make a different body compare equal.
pub(crate) fn zoom_recovery_hooks_state(
    hooks: &[u8],
    allocation: ZoomRecoveryHookAllocation,
    session: &str,
    window_id: u64,
) -> Result<ZoomRecoveryHookState, ()> {
    fn unquoted(value: &[u8]) -> Vec<u8> {
        // tmux re-renders stored hook bodies with its own quoting: a target
        // sent as `-t '=$0':` is shown as `-t "=$0:"`, and a name session as
        // `-t =meeterm:`. Strip both quote styles before comparing.
        value
            .iter()
            .copied()
            .filter(|byte| *byte != b'\'' && *byte != b'"')
            .collect()
    }
    let target = window_target(session, window_id).map_err(|_| ())?;
    let target = unquoted(target.as_bytes());
    let session = session_target(session).map_err(|_| ())?;
    let session = unquoted(session.as_bytes());
    let session = String::from_utf8_lossy(&session);
    let resize = format!("resize-pane -Z -t {}", String::from_utf8_lossy(&target));
    let remove_detached = format!(
        "set-hook -u -t {session}: client-detached[{}]",
        allocation.index
    );
    let remove_session_changed = format!(
        "set-hook -u -t {session}: client-session-changed[{}]",
        allocation.index
    );
    let mut found = [false; 2];
    let mut owned = [false; 2];
    for raw_line in hooks.split(|byte| *byte == b'\n') {
        let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let Some((name, index)) = parse_hook_entry(raw_line)? else {
            continue;
        };
        if index != Some(allocation.index) {
            continue;
        }
        let slot = if name == ZOOM_RECOVERY_DETACHED_HOOK.as_bytes() {
            0
        } else if name == ZOOM_RECOVERY_SESSION_CHANGED_HOOK.as_bytes() {
            1
        } else {
            continue;
        };
        let line = unquoted(raw_line);
        found[slot] = true;
        owned[slot] = line
            .windows(target.len())
            .any(|part| part == target.as_slice())
            && line
                .windows(resize.len())
                .any(|part| part == resize.as_bytes())
            && line
                .windows(remove_detached.len())
                .any(|part| part == remove_detached.as_bytes())
            && line
                .windows(remove_session_changed.len())
                .any(|part| part == remove_session_changed.as_bytes());
    }
    if !found[0] && !found[1] {
        Ok(ZoomRecoveryHookState::Absent)
    } else if found == [true, true] && owned == [true, true] {
        Ok(ZoomRecoveryHookState::Owned)
    } else {
        Ok(ZoomRecoveryHookState::Replaced)
    }
}

#[allow(dead_code)]
pub fn send_bytes_command(pane_id: u64, bytes: &[u8]) -> String {
    let mut command = format!("send-keys -t %{pane_id} -H");
    for byte in bytes {
        command.push_str(&format!(" {byte:02x}"));
    }
    command
}

pub(crate) fn send_bytes_command_for_session(
    session: &str,
    pane_id: u64,
    bytes: &[u8],
) -> Result<String, CommandArgumentError> {
    let target = pane_only_target(session, pane_id)?;
    let mut command = format!("send-keys -t {target} -H");
    for byte in bytes {
        command.push_str(&format!(" {byte:02x}"));
    }
    Ok(command)
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
        assert_eq!(pane.pane_name, "tab\tline\\q");
        assert!(pane.zoomed);
        assert!(!pane.window_active);
    }

    #[test]
    fn q_quoted_names_collapse_format_escapes_without_losing_backslashes() {
        // This is the byte shape emitted by tmux 3.4 for the name
        // `daily ; # $HOME \\ \" 日本語`. The four slashes before `$HOME`
        // are tmux's format-output escape layers, while the five before the
        // space are one literal backslash plus q:'s escaped space.
        let encoded = r##"daily\ \;\ \#\ \\\\$HOME\ \\\\\ \"\ 日本語"##.as_bytes();
        assert_eq!(decode_tmux_quoted(encoded), "daily ; # $HOME \\ \" 日本語");

        assert_eq!(decode_tmux_quoted(br"$"), "$");
        assert_eq!(decode_tmux_quoted(br"\$HOME"), "$HOME");
        assert_eq!(decode_tmux_quoted(br"\\$HOME"), "$HOME");
        assert_eq!(decode_tmux_quoted(br"\\\ "), "\\ ");
        assert_eq!(decode_tmux_quoted(br"\\\\$HOME"), "$HOME");
        assert_eq!(decode_tmux_quoted(br"\\\\\\\\$HOME"), "\\$HOME");
        assert_eq!(decode_tmux_quoted(br"\\\\"), "\\");
    }

    #[test]
    fn user_names_are_quoted_as_one_tmux_argument() {
        let name = "desk; # comment $HOME ~root \\\\ \" ' 日本語";
        let quoted = quote_tmux_argument(name).unwrap();
        assert!(quoted.starts_with('\'') && quoted.ends_with('\''));
        assert!(quoted.contains("$HOME"));
        assert!(quoted.contains("## comment"));
        assert!(quoted.contains("'\\''"));
        assert!(quoted.contains("日本語"));
        assert!(
            !create_workspace_command(name)
                .unwrap()
                .contains("; # comment $HOME")
        );
        assert_eq!(
            rename_workspace_command(42, name).unwrap(),
            format!("rename-window -t =meeterm:@42 {quoted}")
        );
        assert_eq!(
            rename_pane_command(42, 7, name).unwrap(),
            format!("select-pane -t =meeterm:@42.%7 -T {quoted}")
        );
        assert_eq!(close_workspace_command(42), "kill-window -t =meeterm:@42");
        assert_eq!(create_pane_command(42), "split-window -d -t =meeterm:@42");
        assert_eq!(close_pane_command(42, 7), "kill-pane -t =meeterm:@42.%7");
    }

    #[test]
    fn user_names_reject_line_controls_and_unbounded_values() {
        assert_eq!(quote_tmux_argument(""), Err(CommandArgumentError::Empty));
        assert_eq!(
            quote_tmux_argument("line\nfeed"),
            Err(CommandArgumentError::ControlCharacter)
        );
        assert_eq!(
            quote_tmux_argument(&"x".repeat(4097)),
            Err(CommandArgumentError::TooLong)
        );
    }

    #[test]
    fn runtime_discovery_retains_opaque_id_server_identity_and_unicode_name() {
        let line = "$7|daily\\ \\;\\ \\#\\ $HOME\\ 日本語|4242|1700000000";
        let identity = parse_session_line(line.as_bytes()).unwrap();
        assert_eq!(identity.session_id, "$7");
        assert_eq!(identity.name, "daily ; # $HOME 日本語");
        assert_eq!(identity.server_pid, 4242);
        assert_eq!(identity.server_start_time, 1700000000);
        let mut same_pid_after_restart = identity.clone();
        same_pid_after_restart.server_start_time += 1;
        assert_ne!(identity, same_pid_after_restart);
        let mut renamed = identity.clone();
        renamed.name = "renamed-hint".into();
        assert_eq!(identity, renamed);

        let attach = attach_command_for_session(&identity.session_id).unwrap();
        assert_eq!(attach, "tmux -C -u attach-session -t '=$7'");
        assert!(!attach.contains("new-session -A"));

        let pipe_name = parse_session_line(b"$8|daily\\|ops|4242|1700000001").unwrap();
        assert_eq!(pipe_name.name, "daily|ops");
    }

    #[test]
    fn attached_epoch_query_rejects_preflight_to_attach_server_replacements() {
        let preflight = parse_session_line(b"$7|meeterm|4242|1700000000").unwrap();
        let query = attached_session_epoch_command();
        assert_eq!(
            query,
            "display-message -p '#{session_id}|#{pid}|#{start_time}'"
        );

        // The same session ID and unchanged PID/start time prove that the
        // Control Mode stream reached the server observed by preflight.
        let attached = parse_session_epoch_line(b"$7|4242|1700000000").unwrap();
        assert!(attached.matches(&preflight));

        // A session replacement may retain the name, the ID, or even the OS
        // PID. The complete server epoch is required in every case.
        for replacement in [
            b"$8|4242|1700000000".as_slice(),
            b"$7|4243|1700000000".as_slice(),
            b"$7|4242|1700000001".as_slice(),
        ] {
            let replacement = parse_session_epoch_line(replacement).unwrap();
            assert!(!replacement.matches(&preflight));
        }

        for malformed in [
            b"$7|4242".as_slice(),
            b"$7|0|1700000000".as_slice(),
            b"$7|4242|0".as_slice(),
            b"$7|4242|1700000000|extra".as_slice(),
        ] {
            assert!(parse_session_epoch_line(malformed).is_err());
        }
    }

    #[test]
    fn runtime_discovery_rejects_invalid_utf8_controls_and_server_identity() {
        assert!(parse_session_line(b"$1\t\xff\t1\t1").is_err());
        assert!(parse_session_line(b"$1\tbad\x01name\t1\t1").is_err());
        assert!(parse_session_line(b"$1\tname\\\t1\t1").is_err());
        assert!(parse_session_line(b"$1\tname\t0\t1").is_err());
        assert!(parse_session_line(b"$1\tname\t1\t0").is_err());
        assert!(parse_session_line(b"$1\tname\t1").is_err());
        assert!(parse_session_line(b"$1|name|1").is_err());
        assert!(parse_session_line(b"$1|name|0|1").is_err());
        assert!(parse_topology_line(b"$1\tname\t@2\t0").is_err());
        assert!(parse_topology_line(b"$1\t\xff\t@2\t1").is_err());
    }

    #[test]
    fn discovery_exit_diagnostics_are_narrow_and_fail_closed() {
        assert!(is_verified_no_server(
            b"no server running on /tmp/tmux-1000/default\n"
        ));
        assert!(is_verified_no_server(b"no sessions\n"));
        assert!(!is_verified_no_server(b"fatal: no sessions\n"));
        assert!(!is_verified_no_server(b"permission denied\n"));
        assert!(is_command_missing(Some(127), b"sh: tmux: not found\n"));
        assert!(!is_command_missing(Some(1), b"not found in a data field\n"));
        assert!(is_session_collision(b"duplicate session: work\n"));
        assert!(!is_session_collision(b"permission denied\n"));
    }

    #[test]
    fn runtime_commands_are_session_scoped_and_create_is_explicit() {
        let name = "desk-1";
        let create = create_session_command(name).unwrap();
        assert!(create.starts_with("tmux new-session -d -s '"));
        assert!(create.contains("-P -F"));
        assert!(create.contains("#{session_id}"));
        assert!(create.contains("#{pid}"));
        assert!(create.contains("#{start_time}"));
        assert!(create.contains("#{session_id}|#{q:session_name}|#{pid}|#{start_time}"));
        assert!(!create.contains('\t'));
        assert!(!create.contains("\\t"));
        assert!(!create.contains("new-session -A"));
        assert!(create_session_command("desk; # $HOME 日本語").is_err());
        assert_eq!(
            validate_create_name("."),
            Err(CommandArgumentError::ControlCharacter)
        );
        assert!(validate_create_name("日本語").is_err());
        assert!(validate_create_name("a.b_c-2").is_ok());
        assert!(validate_create_name("1-start").is_ok());
        assert!(validate_create_name("-start").is_err());
        assert!(validate_create_name("_start").is_err());
        assert!(validate_create_name(".start").is_err());
        assert!(validate_create_name(&"a".repeat(64)).is_ok());
        assert!(validate_create_name(&"a".repeat(65)).is_err());
        assert!(validate_create_name("a#b").is_err());
        assert!(validate_create_name("a\nb").is_err());

        let command = create_workspace_command_for_session("team; # 日本語", "work").unwrap();
        assert!(command.contains("new-window -d -t '"));
        assert!(command.contains("work"));
        assert!(list_sessions_command().contains("#{pid}"));
        assert!(list_sessions_command().contains("#{start_time}"));
        assert!(list_sessions_command().contains("#{session_id}|#{q:session_name}"));
        assert!(!list_sessions_command().contains('\t'));
        assert!(!list_sessions_command().contains("new-session"));

        let windows = list_windows_command_for_session("$7").unwrap();
        let panes = list_panes_command_for_session("$7").unwrap();
        assert!(windows.contains("#{window_id}\t#{q:window_name}"));
        assert!(panes.contains("#{window_id}\t#{pane_id}"));
        assert!(!windows.contains("\\t"));
        assert!(!panes.contains("\\t"));
        assert!(
            capture_pane_command_for_session("$7", 9)
                .unwrap()
                .contains("-t '=$7:.%9'")
        );
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
                .contains("if-shell -F -t '=meeterm:@23' \"#{window_zoomed_flag}\" \"resize-pane -Z -t '=meeterm:@23'\"")
        );
        assert!(install.contains("set-hook -u -t =meeterm: client-detached[1002]"));
        assert!(install.contains("set-hook -u -t =meeterm: client-session-changed[1002]"));
        assert!(!install.contains("set-hook -u -t =meeterm: client-detached'"));
        assert!(!install.contains("set-hook -u -t =meeterm: client-session-changed'"));

        let cleanup = cleanup_zoom_recovery_hooks_command(allocation, 23);
        assert!(cleanup.starts_with("if-shell -F -t %23"));
        assert!(cleanup.contains("client-detached[1002]"));
    }

    #[test]
    fn zoom_recovery_hook_state_requires_both_exact_window_scoped_entries() {
        let allocation = ZoomRecoveryHookAllocation { index: 1003 };
        let session = "meeterm";
        let body =
            zoom_recovery_hook_body_for_session(session, allocation, 23).expect("owned hook body");
        let owned = format!(
            "client-detached[{}] {body}\nclient-session-changed[{}] {body}\n",
            allocation.index, allocation.index
        );
        assert_eq!(
            zoom_recovery_hooks_state(&owned.into_bytes(), allocation, session, 23),
            Ok(ZoomRecoveryHookState::Owned)
        );
        // tmux re-parses the stored body for `show-hooks`: outer single
        // quotes are normalized away while quotes inside double-quoted
        // arguments remain. This is the observed tmux 3.4 stored form.
        let normalized_body = format!(
            "if-shell -F -t =meeterm:@23 \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t '=meeterm:@23'\" ; set-hook -u -t =meeterm: client-detached[{}] ; set-hook -u -t =meeterm: client-session-changed[{}]",
            allocation.index, allocation.index
        );
        let normalized = format!(
            "client-detached[{}] {normalized_body}\nclient-session-changed[{}] {normalized_body}\n",
            allocation.index, allocation.index
        );
        assert_eq!(
            zoom_recovery_hooks_state(&normalized.into_bytes(), allocation, session, 23),
            Ok(ZoomRecoveryHookState::Owned)
        );
        // A numeric `$N` session target is rendered with the colon inside
        // double quotes: `-t '=$0':` is stored as `-t "=$0:"`. Observed on
        // the real OpenSSH fixture, which selects runtimes by `$` identity.
        let numeric_session = "$0";
        let numeric_body = format!(
            "if-shell -F -t \"=$0:@0\" \"#{{window_zoomed_flag}}\" \"resize-pane -Z -t =$0:@0\" ; set-hook -u -t \"=$0:\" client-detached[{}] ; set-hook -u -t \"=$0:\" client-session-changed[{}]",
            allocation.index, allocation.index
        );
        let numeric = format!(
            "client-detached[{}] {numeric_body}\nclient-session-changed[{}] {numeric_body}\n",
            allocation.index, allocation.index
        );
        assert_eq!(
            zoom_recovery_hooks_state(&numeric.into_bytes(), allocation, numeric_session, 0),
            Ok(ZoomRecoveryHookState::Owned)
        );
        assert_eq!(
            zoom_recovery_hooks_state(b"", allocation, session, 23),
            Ok(ZoomRecoveryHookState::Absent)
        );
        assert_eq!(
            zoom_recovery_hooks_state(
                format!(
                    "client-detached[{}] display-message third-party\n",
                    allocation.index
                )
                .as_bytes(),
                allocation,
                session,
                23,
            ),
            Ok(ZoomRecoveryHookState::Replaced)
        );
    }
}
