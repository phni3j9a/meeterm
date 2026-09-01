use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::input::SpecialKey;
use crate::snapshot::Snapshot;
use crate::terminal::{Terminal, TerminalError};

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
    registry()
        .lock()
        .map(|mut terminals| terminals.remove(&id).is_some())
        .unwrap_or(false)
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

pub fn resize_terminal(id: TerminalId, columns: u16, rows: u16) -> Result<(), TerminalError> {
    with_terminal(id, |terminal| terminal.resize(columns, rows))
}

pub fn commit_utf8(id: TerminalId, bytes: &[u8]) -> Result<u64, TerminalError> {
    with_terminal(id, |terminal| terminal.commit_utf8(bytes))
}

pub fn send_special_key(id: TerminalId, key: SpecialKey) -> Result<usize, TerminalError> {
    with_terminal(id, |terminal| Ok(terminal.send_special_key(key)))
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
