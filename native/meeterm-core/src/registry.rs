use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::input::{KeyCode, Modifiers, SpecialKey};
use crate::snapshot::Snapshot;
use crate::terminal::{
    InputSender, ResizeSender, Terminal, TerminalError, configured_scrollback_lines,
    set_configured_scrollback_lines, validate_scrollback_lines,
};

pub type TerminalId = u64;
pub type SharedTerminal = Arc<Mutex<Terminal>>;

/// One native tmux capture supplied to the strict recovery transaction.
/// References are borrowed only for the duration of the registry batch; the
/// caller owns the staged bytes and cannot observe a partially applied batch.
pub(crate) struct ScreenCapture<'a> {
    pub(crate) terminal_id: TerminalId,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) bytes: &'a [u8],
    pub(crate) trailing_output: &'a [u8],
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<Mutex<HashMap<TerminalId, SharedTerminal>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<TerminalId, SharedTerminal>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn create_terminal(columns: u16, rows: u16) -> Result<TerminalId, TerminalError> {
    let mut terminals = registry()
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    // Construct while holding the registry lock so a terminal cannot observe
    // the old global scrollback setting in the small window between a
    // settings update and insertion into the registry.
    let terminal = Arc::new(Mutex::new(Terminal::new(columns, rows)?));

    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if id == 0 || terminals.contains_key(&id) {
            continue;
        }
        terminals.insert(id, terminal);
        return Ok(id);
    }
}

pub fn destroy_terminal(id: TerminalId) -> bool {
    let removed = registry()
        .lock()
        .map(|mut terminals| terminals.remove(&id).is_some())
        .unwrap_or(false);
    if removed {
        crate::ssh::terminal_destroyed(id);
    }
    removed
}

pub fn terminal_count() -> usize {
    registry()
        .lock()
        .map(|terminals| terminals.len())
        .unwrap_or(0)
}

pub fn snapshot(id: TerminalId) -> Result<Snapshot, TerminalError> {
    with_terminal(id, |terminal| terminal.snapshot())
}

pub(crate) fn shared_terminal(id: TerminalId) -> Result<SharedTerminal, TerminalError> {
    let terminals = registry()
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    terminals
        .get(&id)
        .cloned()
        .ok_or(TerminalError::UnknownTerminal)
}

pub(crate) fn begin_remote(id: TerminalId, generation: u64) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| terminal.begin_remote(generation))
}

pub(crate) fn reset_remote_binding(id: TerminalId, generation: u64) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| terminal.reset_remote_binding(generation))
}

pub(crate) fn prepare_pane_transport(
    id: TerminalId,
    generation: u64,
    size: (u16, u16),
    input: InputSender,
    resize: ResizeSender,
) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.begin_remote(generation)?;
        terminal.resize_from_remote(size.0, size.1)?;
        terminal.attach_transport(generation, input, resize)
    })
}

pub(crate) fn detach_transport(id: TerminalId, generation: u64) {
    let Ok(terminal) = shared_terminal(id) else {
        return;
    };
    if let Ok(mut terminal) = terminal.lock() {
        terminal.detach_transport(generation);
    }
}

pub(crate) fn mark_transport_ready(id: TerminalId, generation: u64) -> bool {
    let Ok(terminal) = shared_terminal(id) else {
        return false;
    };
    terminal
        .lock()
        .map(|mut terminal| terminal.mark_transport_ready(generation))
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn transport_ready(id: TerminalId, generation: u64) -> bool {
    with_terminal(id, |terminal| {
        Ok(terminal.transport_ready_for_generation(generation))
    })
    .unwrap_or(false)
}

pub(crate) fn transport_ready_or_local(id: TerminalId, generation: u64) -> bool {
    with_terminal(id, |terminal| {
        Ok(terminal.transport_ready_for_generation_or_local(generation))
    })
    .unwrap_or(false)
}

pub(crate) fn feed_remote(id: TerminalId, generation: u64, bytes: &[u8]) -> bool {
    let Ok(terminal) = shared_terminal(id) else {
        return false;
    };
    terminal
        .lock()
        .map(|mut terminal| terminal.feed_remote(generation, bytes))
        .unwrap_or(false)
}

/// Apply the first Herdr full frame and make its semantic transport usable as
/// one selected-terminal transaction. The registry map and Terminal lock stay
/// held from preflight through the non-fallible Term replacement and Ready
/// transition; callers may wrap this in `ConnectionShared`'s
/// `info -> session` epoch commit without introducing a reverse lock edge.
pub(crate) fn restore_remote_display_and_ready(
    id: TerminalId,
    generation: u64,
    columns: u16,
    rows: u16,
    bytes: &[u8],
) -> Result<(), TerminalError> {
    let terminals = registry()
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    let terminal = terminals
        .get(&id)
        .cloned()
        .ok_or(TerminalError::UnknownTerminal)?;
    let mut terminal = terminal
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    terminal.preflight_remote_display(generation, columns, rows)?;
    terminal.restore_remote_display_after_preflight(generation, columns, rows, bytes);
    terminal.mark_transport_ready_after_preflight(generation);
    Ok(())
}

pub(crate) fn terminal_revision(id: TerminalId) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.content_revision()))
}

pub(crate) fn terminal_dimensions(id: TerminalId) -> Result<(u16, u16), TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.dimensions()))
}

/// Read the per-terminal operation epoch used to reject delayed native input
/// after a transport/controller binding has been revoked and reacquired.
pub(crate) fn operation_epoch(id: TerminalId) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.operation_epoch()))
}

pub fn resize_terminal(id: TerminalId, columns: u16, rows: u16) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| terminal.resize(columns, rows))
}

pub(crate) fn resize_terminal_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    columns: u16,
    rows: u16,
) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.resize_at_epoch(expected_epoch, columns, rows)
    })
}

pub(crate) fn restore_screen(
    id: TerminalId,
    generation: u64,
    columns: u16,
    rows: u16,
    bytes: &[u8],
) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.restore_screen(generation, columns, rows, bytes)
    })
}

/// Apply and publish one strict tmux capture batch atomically from the native
/// registry's point of view.
///
/// Lock order is deliberately `registry map -> Terminal IDs ascending`.
/// Resolving every map entry and retaining every Terminal lock before the
/// first preflight means a missing target, generation mismatch, binding loss,
/// invalid dimensions, or poisoned lock fails before any Term is changed. The
/// apply phase calls only prevalidated, infallible Terminal operations. Each
/// transport remains Attached while all capture bytes (including parser
/// replay) are fed; only after every capture succeeds are all gates changed to
/// Ready. This function never calls back into `ssh`, `SessionState`, or
/// `ConnectionInfo`, so the strict caller's `info -> session -> registry`
/// order has no reverse edge.
pub(crate) fn restore_strict_capture_batch(
    generation: u64,
    captures: &[ScreenCapture<'_>],
) -> Result<(), TerminalError> {
    if captures.is_empty() {
        return Ok(());
    }

    // Keep the map lock through lookup, Terminal locking, preflight, apply,
    // and Ready publication. `destroy_terminal` releases this lock before it
    // calls `ssh::terminal_destroyed`, so destruction cannot form a registry ↔
    // session deadlock with this batch.
    let terminals = registry()
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;

    let mut targets = captures
        .iter()
        .map(|capture| {
            terminals
                .get(&capture.terminal_id)
                .cloned()
                .map(|terminal| (capture.terminal_id, terminal))
                .ok_or(TerminalError::UnknownTerminal)
        })
        .collect::<Result<Vec<_>, _>>()?;
    targets.sort_unstable_by_key(|(id, _)| *id);
    if targets.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        // A duplicate target would require locking one Terminal twice and is
        // a malformed topology mapping, not a valid atomic batch.
        return Err(TerminalError::RemoteGenerationMismatch);
    }

    // All Terminal locks are acquired in the same order used by every batch.
    // No operation below can interleave a detach/destroy/resize/input mutation
    // between its preflight and application.
    let mut locked = Vec::with_capacity(targets.len());
    for (_, terminal) in &targets {
        locked.push(
            terminal
                .lock()
                .map_err(|_| TerminalError::RegistryPoisoned)?,
        );
    }

    let mut ordered = captures.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|capture| capture.terminal_id);
    for capture in &ordered {
        let target = targets
            .binary_search_by_key(&capture.terminal_id, |(id, _)| *id)
            .map_err(|_| TerminalError::UnknownTerminal)?;
        locked[target].preflight_strict_capture(
            generation,
            capture.columns,
            capture.rows,
            capture.bytes,
            capture.trailing_output,
        )?;
    }

    // Every fallible condition was checked above. In particular, replay stays
    // behind the Attached gate, so Event::PtyWrite replies are intentionally
    // discarded rather than buffered for a later connection.
    for capture in &ordered {
        let target = targets
            .binary_search_by_key(&capture.terminal_id, |(id, _)| *id)
            .expect("strict capture target was preflighted");
        locked[target].restore_screen_after_preflight(
            generation,
            capture.columns,
            capture.rows,
            capture.bytes,
            capture.trailing_output,
        );
    }

    // The gate transition is the final part of the native batch. It cannot
    // fail after the matching Attached binding was preflighted while the same
    // Terminal lock was held.
    for terminal in &mut locked {
        terminal.mark_transport_ready_after_preflight(generation);
    }
    Ok(())
}

pub fn commit_utf8(id: TerminalId, bytes: &[u8]) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| terminal.commit_utf8(bytes))
}

pub(crate) fn commit_utf8_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    bytes: &[u8],
) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| {
        terminal.commit_utf8_at_epoch(expected_epoch, bytes)
    })
}

pub fn send_special_key(id: TerminalId, key: SpecialKey) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| terminal.send_special_key(key))
}

pub(crate) fn send_special_key_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    key: SpecialKey,
) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| {
        terminal.send_special_key_at_epoch(expected_epoch, key)
    })
}

/// Send a generic ABI key. The raw values are parsed here so platform code
/// cannot smuggle unknown key or modifier bits into the terminal encoder.
pub fn send_key(id: TerminalId, key: u32, modifiers: u32) -> Result<usize, TerminalError> {
    let key = KeyCode::try_from(key).map_err(|_| TerminalError::InvalidKey)?;
    let modifiers = Modifiers::from_bits(modifiers).ok_or(TerminalError::InvalidModifiers)?;
    with_terminal(id, |terminal| terminal.send_key(key, modifiers))
}

pub(crate) fn send_key_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    key: u32,
    modifiers: u32,
) -> Result<usize, TerminalError> {
    let key = KeyCode::try_from(key).map_err(|_| TerminalError::InvalidKey)?;
    let modifiers = Modifiers::from_bits(modifiers).ok_or(TerminalError::InvalidModifiers)?;
    with_terminal(id, |terminal| {
        terminal.send_key_at_epoch(expected_epoch, key, modifiers)
    })
}

pub fn paste_utf8(id: TerminalId, bytes: &[u8]) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| terminal.paste_utf8(bytes))
}

pub(crate) fn paste_utf8_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    bytes: &[u8],
) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| {
        terminal.paste_utf8_at_epoch(expected_epoch, bytes)
    })
}

pub fn commit_modified_utf8(
    id: TerminalId,
    bytes: &[u8],
    modifiers: u32,
) -> Result<usize, TerminalError> {
    let modifiers = Modifiers::from_bits(modifiers).ok_or(TerminalError::InvalidModifiers)?;
    with_terminal(id, |terminal| {
        terminal.commit_modified_utf8(bytes, modifiers)
    })
}

pub(crate) fn commit_modified_utf8_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    bytes: &[u8],
    modifiers: u32,
) -> Result<usize, TerminalError> {
    let modifiers = Modifiers::from_bits(modifiers).ok_or(TerminalError::InvalidModifiers)?;
    with_terminal(id, |terminal| {
        terminal.commit_modified_utf8_at_epoch(expected_epoch, bytes, modifiers)
    })
}

pub fn select_start(
    id: TerminalId,
    viewport_row: u32,
    viewport_column: u32,
) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.select_start(viewport_row, viewport_column)
    })
}

pub fn select_update(
    id: TerminalId,
    viewport_row: u32,
    viewport_column: u32,
) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.select_update(viewport_row, viewport_column)
    })
}

pub fn clear_selection(id: TerminalId) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.clear_selection();
        Ok(())
    })
}

pub fn selection_text(id: TerminalId) -> Result<Option<String>, TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.selection_text()))
}

pub fn set_theme(id: TerminalId, light: bool) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.set_theme(light);
        Ok(())
    })
}

pub fn scrollback_lines() -> usize {
    configured_scrollback_lines()
}

pub fn set_scrollback_limit(lines: usize) -> Result<(), TerminalError> {
    validate_scrollback_lines(lines)?;
    let terminals = registry()
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    if configured_scrollback_lines() == lines {
        return Ok(());
    }
    set_configured_scrollback_lines(lines)?;
    for terminal in terminals.values() {
        terminal
            .lock()
            .map_err(|_| TerminalError::RegistryPoisoned)?
            .apply_scrollback_limit(lines)?;
    }
    Ok(())
}

pub fn scroll_lines(id: TerminalId, lines: i32) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.scroll_lines(lines);
        Ok(())
    })
}

pub(crate) fn scroll_lines_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    lines: i32,
) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| {
        terminal.scroll_lines_at_epoch(expected_epoch, lines)
    })
}

pub(crate) fn send_bytes(id: TerminalId, bytes: &[u8]) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| terminal.send_bytes(bytes))
}

pub(crate) fn send_bytes_at_epoch(
    id: TerminalId,
    expected_epoch: u64,
    bytes: &[u8],
) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| {
        terminal.send_bytes_at_epoch(expected_epoch, bytes)
    })
}

pub fn input_commit_count(id: TerminalId) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.input_commit_count()))
}

fn with_terminal<R>(
    id: TerminalId,
    operation: impl FnOnce(&mut Terminal) -> Result<R, TerminalError>,
) -> Result<R, TerminalError> {
    let terminal = {
        let terminals = registry()
            .lock()
            .map_err(|_| TerminalError::RegistryPoisoned)?;
        terminals.get(&id).cloned()
    }
    .ok_or(TerminalError::UnknownTerminal)?;

    let mut terminal = terminal
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    operation(&mut terminal)
}

#[cfg(test)]
pub(crate) fn with_terminal_for_test<R>(
    id: TerminalId,
    operation: impl FnOnce(&mut Terminal) -> R,
) -> Result<R, TerminalError> {
    let terminal = {
        let terminals = registry()
            .lock()
            .map_err(|_| TerminalError::RegistryPoisoned)?;
        terminals.get(&id).cloned()
    }
    .ok_or(TerminalError::UnknownTerminal)?;
    let mut terminal = terminal
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    Ok(operation(&mut terminal))
}
