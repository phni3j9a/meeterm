use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::input::SpecialKey;
use crate::snapshot::Snapshot;
use crate::terminal::{InputSender, ResizeSender, Terminal, TerminalError};

pub type TerminalId = u64;
pub type SharedTerminal = Arc<Mutex<Terminal>>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<Mutex<HashMap<TerminalId, SharedTerminal>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<TerminalId, SharedTerminal>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn create_terminal(columns: u16, rows: u16) -> Result<TerminalId, TerminalError> {
    let terminal = Arc::new(Mutex::new(Terminal::new(columns, rows)?));
    let mut terminals = registry()
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;

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

pub(crate) fn feed_remote(id: TerminalId, generation: u64, bytes: &[u8]) -> bool {
    let Ok(terminal) = shared_terminal(id) else {
        return false;
    };
    terminal
        .lock()
        .map(|mut terminal| terminal.feed_remote(generation, bytes))
        .unwrap_or(false)
}

pub(crate) fn terminal_revision(id: TerminalId) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.content_revision()))
}

pub(crate) fn terminal_dimensions(id: TerminalId) -> Result<(u16, u16), TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.dimensions()))
}

pub fn resize_terminal(id: TerminalId, columns: u16, rows: u16) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| terminal.resize(columns, rows))
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

pub fn commit_utf8(id: TerminalId, bytes: &[u8]) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| terminal.commit_utf8(bytes))
}

pub fn send_special_key(id: TerminalId, key: SpecialKey) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| terminal.send_special_key(key))
}

pub(crate) fn send_bytes(id: TerminalId, bytes: &[u8]) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| terminal.send_bytes(bytes))
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
    operation: impl FnOnce(&Terminal) -> R,
) -> Result<R, TerminalError> {
    let terminal = {
        let terminals = registry()
            .lock()
            .map_err(|_| TerminalError::RegistryPoisoned)?;
        terminals.get(&id).cloned()
    }
    .ok_or(TerminalError::UnknownTerminal)?;
    let terminal = terminal
        .lock()
        .map_err(|_| TerminalError::RegistryPoisoned)?;
    Ok(operation(&terminal))
}
