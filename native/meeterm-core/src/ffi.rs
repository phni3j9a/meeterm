use std::path::Path;
use std::slice;

use crate::input::SpecialKey;
use crate::registry;
use crate::ssh::{
    AuthOptions, ConnectOptions, ConnectionError, ConnectionSnapshot, connection_snapshot,
    disconnect_terminal, forget_host_key, respond_to_host_key, terminal_revision,
};
use crate::workspace::{
    Backend, RuntimeCandidate, RuntimeSection, RuntimeSectionState, RuntimeState,
};
use zeroize::Zeroizing;

const FFI_ERROR: i32 = -1;
const FFI_INVALID_KEY: i32 = -2;
const MAX_RECOVERY_TOKEN_BYTES: usize = 128;
pub const MAX_WORKSPACE_STATE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RUNTIME_DISCOVERY_BYTES: usize = 1024 * 1024;
const RUNTIME_DISPLAY_NAME_BYTES: usize = 256;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeDiscoveryBridge {
    connection_generation: String,
    revision: u64,
    backends: Vec<RuntimeBackendBridge>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeBackendBridge {
    backend: &'static str,
    state: &'static str,
    error_code: String,
    error_message: String,
    candidates: Vec<RuntimeCandidateBridge>,
    can_create: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeCandidateBridge {
    id: String,
    backend: &'static str,
    name: String,
    state: &'static str,
    selectable: bool,
    is_default: bool,
    last_used: bool,
    error_code: String,
    error_message: String,
}

fn runtime_backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Tmux => "tmux",
        Backend::Herdr => "herdr",
    }
}

fn runtime_section_state(state: &RuntimeSectionState) -> &'static str {
    match state {
        RuntimeSectionState::Loading => "loading",
        RuntimeSectionState::Success | RuntimeSectionState::Empty => "ready",
        RuntimeSectionState::Error => "error",
    }
}

fn runtime_candidate_state(state: &RuntimeState) -> &'static str {
    match state {
        RuntimeState::Running => "running",
        RuntimeState::Stopped | RuntimeState::Unknown => "stopped",
    }
}

fn runtime_display_name(name: &str) -> String {
    let mut end = name.len().min(RUNTIME_DISPLAY_NAME_BYTES);
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

fn runtime_error_text(value: Option<&str>, max_bytes: usize) -> String {
    let value = value.unwrap_or_default();
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn runtime_backend_bridge(
    backend: Backend,
    section: &RuntimeSection,
    can_create: bool,
) -> RuntimeBackendBridge {
    let backend_name = runtime_backend_name(backend);
    RuntimeBackendBridge {
        backend: backend_name,
        state: runtime_section_state(&section.state),
        error_code: section.error_code.clone().unwrap_or_default(),
        error_message: section.error_message.clone().unwrap_or_default(),
        candidates: section
            .candidates
            .iter()
            .map(|candidate: &RuntimeCandidate| RuntimeCandidateBridge {
                id: candidate.id.clone(),
                backend: backend_name,
                // The platform adapters expose a 256-byte display field;
                // the native binding still retains the complete opaque
                // candidate and remote name for selection.
                name: runtime_display_name(&candidate.name),
                state: runtime_candidate_state(&candidate.state),
                selectable: candidate.selectable,
                is_default: candidate.suggested,
                // Persisted profile hints are applied by the platform layer;
                // the native host connection does not know the profile ID.
                last_used: false,
                error_code: runtime_error_text(
                    candidate.error_code.as_deref(),
                    crate::ssh::ERROR_CODE_CAPACITY,
                ),
                error_message: runtime_error_text(
                    candidate.error_message.as_deref(),
                    crate::ssh::ERROR_MESSAGE_CAPACITY,
                ),
            })
            .collect(),
        can_create,
    }
}

fn runtime_discovery_bytes(id: u64) -> Option<Vec<u8>> {
    let snapshot = crate::ssh::runtime_discovery_snapshot(id).ok()?;
    let bridge = RuntimeDiscoveryBridge {
        connection_generation: snapshot.connection_generation.to_string(),
        revision: snapshot.discovery_revision,
        backends: vec![
            runtime_backend_bridge(Backend::Tmux, &snapshot.tmux, true),
            runtime_backend_bridge(Backend::Herdr, &snapshot.herdr, false),
        ],
    };
    let bytes = serde_json::to_vec(&bridge).ok()?;
    (bytes.len() <= MAX_RUNTIME_DISCOVERY_BYTES).then_some(bytes)
}

fn terminal_error_code(error: crate::terminal::TerminalError) -> i32 {
    match error {
        crate::terminal::TerminalError::UnknownTerminal => -2,
        crate::terminal::TerminalError::InputNotReady => -7,
        crate::terminal::TerminalError::InputQueueFull => -8,
        crate::terminal::TerminalError::TransportClosed => -9,
        crate::terminal::TerminalError::InputTooLarge => -10,
        _ => FFI_ERROR,
    }
}

unsafe fn utf8_argument(pointer: *const u8, length: usize) -> Result<String, ()> {
    if length != 0 && pointer.is_null() {
        return Err(());
    }
    let bytes = if length == 0 {
        &[]
    } else {
        // The caller promises that `pointer` points to `length` readable
        // bytes.  The slice is copied before this function returns.
        unsafe { slice::from_raw_parts(pointer, length) }
    };
    String::from_utf8(bytes.to_vec()).map_err(|_| ())
}

/// Parse the decimal representation used by the JavaScript-facing recovery
/// bridge without passing through a platform number type.  The native
/// adapters call this before invoking the u64 ABI, while JNI uses the same
/// helper after copying a Java string.
#[cfg(target_os = "android")]
pub(crate) fn parse_decimal_u64(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok()
}

unsafe fn recovery_token_argument(pointer: *const u8, length: usize) -> Result<String, ()> {
    if length == 0 || length > MAX_RECOVERY_TOKEN_BYTES {
        return Err(());
    }
    let token = unsafe { utf8_argument(pointer, length) }?;
    if token.is_empty() || token.contains('\0') || token.chars().any(char::is_control) {
        return Err(());
    }
    Ok(token)
}

fn connection_error_code(error: ConnectionError) -> i32 {
    error.code()
}

/// Create a registry-backed terminal and feed its built-in demo once.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_create_terminal(columns: u16, rows: u16) -> u64 {
    registry::create_terminal(columns, rows).unwrap_or(0)
}

/// Return the encoded snapshot size, or zero for an unknown/invalid ID.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_snapshot_size(id: u64) -> usize {
    registry::snapshot(id)
        .map(|snapshot| snapshot.len())
        .unwrap_or(0)
}

/// Copy a snapshot into a native caller-owned buffer.
///
/// If `capacity` is too small, the required size is returned and no bytes are
/// written. Zero means an unknown ID or invalid output pointer.
///
/// # Safety
///
/// When `capacity` is at least the returned snapshot size, `out` must point to
/// a writable buffer of at least `capacity` bytes. A null pointer is accepted
/// only when the function returns before copying because the buffer is too
/// small or the ID is invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_snapshot(id: u64, out: *mut u8, capacity: usize) -> usize {
    let Ok(snapshot) = registry::snapshot(id) else {
        return 0;
    };
    let bytes = snapshot.as_bytes();
    if bytes.len() > capacity {
        return bytes.len();
    }
    if bytes.is_empty() {
        return 0;
    }
    if out.is_null() {
        return 0;
    }

    // The caller promises that `out` points to `capacity` writable bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    bytes.len()
}

/// Resize a registered terminal. Returns zero on success and a negative value
/// on invalid dimensions or an unknown ID.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_resize_terminal(id: u64, columns: u16, rows: u16) -> i32 {
    registry::resize_terminal(id, columns, rows)
        .map(|()| 0)
        .unwrap_or(FFI_ERROR)
}

/// Resize only for the operation epoch captured by the native layout pass.
/// A stale resize is rejected without changing the cached terminal layout or
/// replaying the dimensions after recovery.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_resize_terminal_at_epoch(
    id: u64,
    expected_epoch: u64,
    columns: u16,
    rows: u16,
) -> i32 {
    registry::resize_terminal_at_epoch(id, expected_epoch, columns, rows)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

/// Commit a native UTF-8 string exactly once. The return value is the new
/// commit count, or zero on invalid UTF-8, a null pointer, or an unknown ID.
///
/// # Safety
///
/// When `length` is non-zero, `bytes` must point to `length` readable bytes.
/// The bytes are copied before this function returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_commit_utf8(id: u64, bytes: *const u8, length: usize) -> u64 {
    if length != 0 && bytes.is_null() {
        return 0;
    }
    let input = if length == 0 {
        &[]
    } else {
        // The caller promises that `bytes` points to `length` readable bytes.
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::commit_utf8(id, input).unwrap_or(0)
}

/// Return the current nonzero per-terminal operation epoch, or zero for an
/// unknown terminal.  This is deliberately separate from the SSH/session
/// recovery epoch and is consumed only by native input adapters.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_operation_epoch(id: u64) -> u64 {
    registry::operation_epoch(id).unwrap_or(0)
}

/// Commit UTF-8 only when the caller's native operation epoch is still
/// current.  A stale completion returns zero and is never queued for replay.
///
/// # Safety
/// For nonzero length, bytes must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_commit_utf8_at_epoch(
    id: u64,
    expected_epoch: u64,
    bytes: *const u8,
    length: usize,
) -> u64 {
    if length != 0 && bytes.is_null() {
        return 0;
    }
    let input = if length == 0 {
        &[]
    } else {
        // SAFETY: the caller promises a readable buffer for this call.
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::commit_utf8_at_epoch(id, expected_epoch, input).unwrap_or(0)
}

/// Enqueue native terminal bytes without UTF-8 validation.  This path is used
/// for already encoded input and remains bounded by the Rust terminal queue.
/// A non-negative result is the number of accepted bytes; negative values are
/// stable native error codes.
///
/// # Safety
///
/// When `length` is non-zero, `bytes` must point to `length` readable bytes
/// for the duration of this call. The bytes are copied before returning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_send_bytes(id: u64, bytes: *const u8, length: usize) -> i32 {
    if length != 0 && bytes.is_null() {
        return FFI_ERROR;
    }
    let input = if length == 0 {
        &[]
    } else {
        // The caller promises that `bytes` points to `length` readable bytes.
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::send_bytes(id, input)
        .map(|length| i32::try_from(length).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Enqueue raw native bytes only for the captured per-terminal operation
/// epoch.  Stale callbacks are rejected and are not retained.
///
/// # Safety
/// For nonzero length, bytes must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_send_bytes_at_epoch(
    id: u64,
    expected_epoch: u64,
    bytes: *const u8,
    length: usize,
) -> i32 {
    if length != 0 && bytes.is_null() {
        return FFI_ERROR;
    }
    let input = if length == 0 {
        &[]
    } else {
        // SAFETY: the caller promises a readable buffer for this call.
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::send_bytes_at_epoch(id, expected_epoch, input)
        .map(|accepted| i32::try_from(accepted).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Native paste input, encoded using the terminal's current bracketed-paste mode.
///
/// # Safety
/// For nonzero length, bytes must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_paste_utf8(id: u64, bytes: *const u8, length: usize) -> i32 {
    if length != 0 && bytes.is_null() {
        return FFI_ERROR;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::paste_utf8(id, bytes)
        .map(|length| i32::try_from(length).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Paste UTF-8 only for the operation epoch captured before an asynchronous
/// native provider completed.
///
/// # Safety
/// For nonzero length, bytes must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_paste_utf8_at_epoch(
    id: u64,
    expected_epoch: u64,
    bytes: *const u8,
    length: usize,
) -> i32 {
    if length != 0 && bytes.is_null() {
        return FFI_ERROR;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        // SAFETY: the caller promises a readable buffer for this call.
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::paste_utf8_at_epoch(id, expected_epoch, bytes)
        .map(|accepted| i32::try_from(accepted).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Positive lines scroll toward history; negative lines toward live output.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_scroll_lines(id: u64, lines: i32) -> i32 {
    registry::scroll_lines(id, lines)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

/// Move the native viewport only for the captured operation epoch.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_scroll_lines_at_epoch(id: u64, expected_epoch: u64, lines: i32) -> i32 {
    registry::scroll_lines_at_epoch(id, expected_epoch, lines)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_send_key(id: u64, key: u32, modifiers: u32) -> i32 {
    registry::send_key(id, key, modifiers)
        .map(|length| i32::try_from(length).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Send a generic key only for the captured operation epoch.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_send_key_at_epoch(
    id: u64,
    expected_epoch: u64,
    key: u32,
    modifiers: u32,
) -> i32 {
    registry::send_key_at_epoch(id, expected_epoch, key, modifiers)
        .map(|accepted| i32::try_from(accepted).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// # Safety
/// For nonzero length, bytes must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_commit_modified_utf8(
    id: u64,
    bytes: *const u8,
    length: usize,
    modifiers: u32,
) -> i32 {
    if length > 65536 || (length != 0 && bytes.is_null()) {
        return FFI_ERROR;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::commit_modified_utf8(id, bytes, modifiers)
        .map(|length| i32::try_from(length).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Commit modified UTF-8 only for the captured operation epoch.
///
/// # Safety
/// For nonzero length, bytes must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_commit_modified_utf8_at_epoch(
    id: u64,
    expected_epoch: u64,
    bytes: *const u8,
    length: usize,
    modifiers: u32,
) -> i32 {
    if length > 65536 || (length != 0 && bytes.is_null()) {
        return FFI_ERROR;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        // SAFETY: the caller promises a readable buffer for this call.
        unsafe { slice::from_raw_parts(bytes, length) }
    };
    registry::commit_modified_utf8_at_epoch(id, expected_epoch, bytes, modifiers)
        .map(|accepted| i32::try_from(accepted).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Viewport coordinates, never pixel coordinates or JavaScript terminal data.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_select_start(id: u64, row: u32, column: u32) -> i32 {
    registry::select_start(id, row, column)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_select_update(id: u64, row: u32, column: u32) -> i32 {
    registry::select_update(id, row, column)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_clear_selection(id: u64) -> i32 {
    registry::clear_selection(id)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

/// Native clipboard copy. Returns required UTF-8 bytes, zero for no selection,
/// or SIZE_MAX for an invalid terminal/oversized selection. No partial copy.
///
/// # Safety
/// A non-null output must be writable for capacity bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_selection_text(id: u64, out: *mut u8, capacity: usize) -> usize {
    let Ok(text) = registry::selection_text(id) else {
        return usize::MAX;
    };
    let Some(text) = text else {
        return 0;
    };
    if text.len() > 4 * 1024 * 1024 {
        return usize::MAX;
    }
    if out.is_null() || capacity < text.len() {
        return text.len();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), out, text.len());
    }
    text.len()
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_set_theme(id: u64, light: u8) -> i32 {
    if light > 1 {
        return FFI_ERROR;
    }
    registry::set_theme(id, light != 0)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_set_scrollback_limit(lines: u32) -> i32 {
    registry::set_scrollback_limit(lines as usize)
        .map(|()| 0)
        .unwrap_or_else(terminal_error_code)
}

/// Send one of the stable `SpecialKey` enum values. The return value is the
/// number of encoded bytes, or a negative error code.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_send_special_key(id: u64, key: u32) -> i32 {
    let Ok(key) = SpecialKey::try_from(key) else {
        return FFI_INVALID_KEY;
    };
    registry::send_special_key(id, key)
        .map(|length| i32::try_from(length).unwrap_or(FFI_ERROR))
        .unwrap_or(FFI_ERROR)
}

/// Send a special key only for the captured operation epoch.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_send_special_key_at_epoch(id: u64, expected_epoch: u64, key: u32) -> i32 {
    let Ok(key) = SpecialKey::try_from(key) else {
        return FFI_INVALID_KEY;
    };
    registry::send_special_key_at_epoch(id, expected_epoch, key)
        .map(|accepted| i32::try_from(accepted).unwrap_or(FFI_ERROR))
        .unwrap_or_else(terminal_error_code)
}

/// Return the number of successful non-empty UTF-8 commits for a terminal.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_input_commit_count(id: u64) -> u64 {
    registry::input_commit_count(id).unwrap_or(0)
}

/// Destroy a registry entry. Returns one when an entry was removed.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_destroy_terminal(id: u64) -> i32 {
    i32::from(registry::destroy_terminal(id))
}

/// Low-frequency typed tmux operation. No shell text is accepted here.
///
/// # Safety
/// For nonzero length, name must point to that many readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_tmux_command(
    id: u64,
    operation: u32,
    target: u64,
    name: *const u8,
    name_length: usize,
) -> i32 {
    if name_length > 1024 {
        return FFI_ERROR;
    }
    let Ok(name) = (unsafe { utf8_argument(name, name_length) }) else {
        return FFI_ERROR;
    };
    let result = match operation {
        0 => crate::ssh::create_workspace(id, &name),
        1 => crate::ssh::rename_workspace(id, target, &name),
        2 if name.is_empty() => crate::ssh::close_workspace(id, target),
        3 if name.is_empty() => crate::ssh::create_pane(id, target),
        4 => crate::ssh::rename_pane(id, target, &name),
        5 if name.is_empty() => crate::ssh::close_pane(id, target),
        6 if name.is_empty() => crate::ssh::refresh_terminal(id),
        7 => crate::ssh::create_group(id, target, &name),
        8 => crate::ssh::rename_group(id, target, &name),
        9 if name.is_empty() => crate::ssh::close_group(id, target),
        10 if name.is_empty() => crate::ssh::select_group(id, target),
        _ => return FFI_ERROR,
    };
    result.map(|()| 0).unwrap_or_else(connection_error_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_set_foreground(id: u64, foreground: u8) -> i32 {
    if foreground > 1 {
        return FFI_ERROR;
    }
    crate::ssh::set_foreground(id, foreground != 0)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Mark whether the terminal is currently visible in the native workspace
/// view. Herdr uses this lifecycle edge to release a controller lease when a
/// pane is hidden; tmux keeps its existing foreground behavior.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_set_terminal_visible(id: u64, visible: u8) -> i32 {
    if visible > 1 {
        return FFI_ERROR;
    }
    crate::ssh::set_terminal_visible(id, visible != 0)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_set_automatic_reconnect(id: u64, enabled: u8) -> i32 {
    if enabled > 1 {
        return FFI_ERROR;
    }
    crate::ssh::set_automatic_reconnect(id, enabled != 0)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Start the legacy host-only SSH connection. All string arguments are UTF-8
/// byte slices; the platform supplies the app-private known-hosts path. The
/// authentication arguments are appended after the original endpoint/key/trust
/// store arguments, keeping the original arguments in their existing order.
/// This compatibility symbol deliberately stops after host authentication and
/// runtime discovery; callers must use the runtime picker and explicit
/// selection/create operations to bind a backend.
///
/// `auth_method` is `publicKey` or `password`. An empty method retains the
/// legacy public-key default for adapters that predate the method selector;
/// every other value is rejected. For `publicKey`, `private_key` is the
/// complete OpenSSH private-key text and an empty passphrase means no
/// passphrase. For `password`, `password` is required and is passed without
/// trimming; the key and passphrase arguments must be empty.
///
/// Credential text is copied into zeroizing Rust-owned storage before the
/// connection task is spawned. It is never placed in a snapshot or log.
///
/// # Safety
///
/// Every non-empty pointer must point to the stated number of readable UTF-8
/// bytes for the duration of this call. The byte slices are copied before the
/// connection task is spawned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_connect(
    id: u64,
    host: *const u8,
    host_length: usize,
    port: u16,
    username: *const u8,
    username_length: usize,
    private_key: *const u8,
    private_key_length: usize,
    passphrase: *const u8,
    passphrase_length: usize,
    known_hosts_path: *const u8,
    known_hosts_path_length: usize,
    auth_method: *const u8,
    auth_method_length: usize,
    password: *const u8,
    password_length: usize,
) -> i32 {
    // SAFETY: the caller of this legacy ABI supplied the same pointers and
    // lengths that this forwarding call receives. The host-only ABI has the
    // same argument layout and does not inspect any backend/runtime fields.
    unsafe {
        meeterm_connect_host(
            id,
            host,
            host_length,
            port,
            username,
            username_length,
            private_key,
            private_key_length,
            passphrase,
            passphrase_length,
            known_hosts_path,
            known_hosts_path_length,
            auth_method,
            auth_method_length,
            password,
            password_length,
        )
    }
}

/// Authenticate an SSH host and enter the native runtime picker. Unlike
/// the removed direct-connect backend ABI, this operation has no
/// backend/runtime target and never creates or attaches a session before the
/// caller selects a candidate.
/// The credential argument order intentionally matches the existing connect
/// ABI so platform adapters can share their transient decoding path.
///
/// # Safety
/// Every non-empty pointer must point to the stated number of readable UTF-8
/// bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_connect_host(
    id: u64,
    host: *const u8,
    host_length: usize,
    port: u16,
    username: *const u8,
    username_length: usize,
    private_key: *const u8,
    private_key_length: usize,
    passphrase: *const u8,
    passphrase_length: usize,
    known_hosts_path: *const u8,
    known_hosts_path_length: usize,
    auth_method: *const u8,
    auth_method_length: usize,
    password: *const u8,
    password_length: usize,
) -> i32 {
    let Ok(host) = (unsafe { utf8_argument(host, host_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    let Ok(username) = (unsafe { utf8_argument(username, username_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    let Ok(private_key) =
        (unsafe { utf8_argument(private_key, private_key_length) }).map(Zeroizing::new)
    else {
        return ConnectionError::InvalidArgument.code();
    };
    let Ok(passphrase) =
        (unsafe { utf8_argument(passphrase, passphrase_length) }).map(Zeroizing::new)
    else {
        return ConnectionError::InvalidArgument.code();
    };
    let Ok(known_hosts_path) =
        (unsafe { utf8_argument(known_hosts_path, known_hosts_path_length) })
    else {
        return ConnectionError::InvalidArgument.code();
    };
    let Ok(auth_method) = (unsafe { utf8_argument(auth_method, auth_method_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    let Ok(password) = (unsafe { utf8_argument(password, password_length) }).map(Zeroizing::new)
    else {
        return ConnectionError::InvalidArgument.code();
    };
    let credentials = match auth_method.as_str() {
        "" | "publicKey" => {
            if !password.is_empty() {
                return ConnectionError::InvalidArgument.code();
            }
            AuthOptions::PublicKey {
                private_key,
                passphrase: (!passphrase.is_empty()).then_some(passphrase),
            }
        }
        "password" => {
            if !private_key.is_empty() || !passphrase.is_empty() {
                return ConnectionError::InvalidArgument.code();
            }
            AuthOptions::Password { password }
        }
        _ => return ConnectionError::InvalidArgument.code(),
    };
    crate::ssh::connect_host(
        id,
        ConnectOptions {
            host,
            port,
            username,
            credentials,
            known_hosts_path: known_hosts_path.into(),
            backend: Backend::Tmux,
            runtime: None,
        },
    )
    .map(|()| 0)
    .unwrap_or_else(connection_error_code)
}

/// Ask the authenticated host actor to refresh both picker sections.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_list_runtimes(id: u64) -> i32 {
    crate::ssh::list_runtimes(id)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

fn copy_runtime_discovery(id: u64, output: *mut u8, capacity: usize) -> usize {
    let Some(bytes) = runtime_discovery_bytes(id) else {
        return 0;
    };
    if output.is_null() || capacity < bytes.len() {
        return bytes.len();
    }
    if !bytes.is_empty() {
        // SAFETY: callers of the exported wrappers promise writable storage;
        // this branch proves it is large enough for the complete payload.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len()) };
    }
    bytes.len()
}

/// Return the bounded runtime picker payload size. The public bridge shape is
/// `{connectionGeneration, revision, backends}`; it contains opaque candidate IDs and display state,
/// never executable paths, sockets, session directories, or terminal bytes.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_runtime_discovery_size(id: u64) -> usize {
    runtime_discovery_bytes(id).map_or(0, |bytes| bytes.len())
}

/// Copy the bounded runtime picker payload for the iOS C ABI and Android JNI
/// adapter. If `output` is null or too small, no bytes are copied and the
/// required length is returned.
///
/// # Safety
/// When `output` is non-null and `capacity` is at least the returned length,
/// it must point to writable storage for that many bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_runtime_discovery(
    id: u64,
    output: *mut u8,
    capacity: usize,
) -> usize {
    copy_runtime_discovery(id, output, capacity)
}

/// Compatibility aliases used by an earlier native adapter draft. Keep them
/// byte-for-byte identical to the canonical runtime-discovery ABI so there is
/// one public metadata shape and one size bound.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_runtime_snapshot_size(id: u64) -> usize {
    meeterm_runtime_discovery_size(id)
}

/// # Safety
/// See [`meeterm_runtime_discovery`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_runtime_snapshot(
    id: u64,
    output: *mut u8,
    capacity: usize,
) -> usize {
    // SAFETY: this compatibility wrapper has the same caller contract as the
    // canonical byte-copy operation.
    unsafe { meeterm_runtime_discovery(id, output, capacity) }
}

/// Select a candidate ID returned by the current runtime snapshot. Raw
/// session IDs, socket paths, and command strings are not accepted here.
///
/// # Safety
/// For nonzero length, `candidate` must point to that many readable UTF-8
/// bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_select_runtime(
    id: u64,
    candidate: *const u8,
    candidate_length: usize,
) -> i32 {
    if candidate_length > 128 {
        return ConnectionError::InvalidArgument.code();
    }
    let Ok(candidate) = (unsafe { utf8_argument(candidate, candidate_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    crate::ssh::select_runtime(id, &candidate)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Request a fresh runtime list using the authenticated SSH actor.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_refresh_runtimes(id: u64) -> i32 {
    meeterm_list_runtimes(id)
}

/// Explicitly create and select a detached tmux runtime. Herdr creation/start
/// is rejected until the isolated 0.9.0 lifecycle proof is available.
///
/// # Safety
/// Every non-empty pointer must point to the stated number of readable UTF-8
/// bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_create_runtime(
    id: u64,
    backend: *const u8,
    backend_length: usize,
    name: *const u8,
    name_length: usize,
) -> i32 {
    if backend_length > 32 || name_length > 4096 {
        return ConnectionError::InvalidArgument.code();
    }
    let (Ok(backend), Ok(name)) = (unsafe { utf8_argument(backend, backend_length) }, unsafe {
        utf8_argument(name, name_length)
    }) else {
        return ConnectionError::InvalidArgument.code();
    };
    let backend = if backend.is_empty() {
        Backend::Tmux
    } else if let Some(backend) = Backend::parse(&backend) {
        backend
    } else {
        return ConnectionError::InvalidArgument.code();
    };
    crate::ssh::create_runtime(id, backend, &name)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Canonical iOS/Android operation for the explicitly supported tmux create
/// action. Herdr creation has no public ABI here and remains rejected by the
/// native selector.
///
/// # Safety
/// For nonzero length, `name` must point to that many readable UTF-8 bytes for
/// the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_create_tmux_session(
    id: u64,
    name: *const u8,
    name_length: usize,
) -> i32 {
    if name_length > crate::tmux::MAX_SESSION_NAME_BYTES {
        return ConnectionError::InvalidArgument.code();
    }
    let Ok(name) = (unsafe { utf8_argument(name, name_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    crate::ssh::create_runtime(id, Backend::Tmux, &name)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Abort an SSH task while retaining its terminal ID for state polling.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_disconnect(id: u64) -> i32 {
    disconnect_terminal(id)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Retry the Rust-owned connection using process-local credentials.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_reconnect(id: u64) -> i32 {
    crate::ssh::reconnect_terminal(id)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Retry the retained recovery flow for the exact decimal epoch observed by
/// the caller.  This append-only symbol is separate from the legacy reconnect
/// request and never performs optimistic state changes in the adapter.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_retry_recovery(id: u64, expected_epoch: u64) -> i32 {
    crate::ssh::retry_recovery(id, expected_epoch)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Confirm a native recovery token. Tokens are bounded UTF-8 values and may
/// not contain controls or NUL; the Rust recovery core consumes them only
/// after validating the current awaiting-confirmation state.
///
/// # Safety
/// For nonzero length, token must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_confirm_recovery(
    id: u64,
    token: *const u8,
    token_length: usize,
) -> i32 {
    let Ok(token) = (unsafe { recovery_token_argument(token, token_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    crate::ssh::confirm_recovery(id, &token)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Leave the retained runtime and start the authenticated runtime picker for
/// the exact epoch observed by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_change_runtime(id: u64, expected_epoch: u64) -> i32 {
    crate::ssh::change_runtime(id, expected_epoch)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Select an existing tmux pane using its numeric runtime identity.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_select_pane(id: u64, pane_id: u64) -> i32 {
    crate::ssh::select_pane(id, pane_id)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Resolve a borrowed native handle without allocating a terminal.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_terminal_exists(id: u64) -> u8 {
    u8::from(registry::shared_terminal(id).is_ok())
}

/// Fixed-layout control-plane record. This contains no terminal output.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TmuxPaneRecord {
    pub window_id: u64,
    pub pane_id: u64,
    pub terminal_id: u64,
    pub window_name_len: u16,
    pub selected: u8,
    pub active: u8,
    pub reserved: [u8; 4],
    pub window_name: [u8; 256],
    pub pane_name_len: u16,
    pub reserved_name: [u8; 6],
    pub pane_name: [u8; 256],
}

#[unsafe(no_mangle)]
pub extern "C" fn meeterm_pane_record_size() -> usize {
    std::mem::size_of::<TmuxPaneRecord>()
}

/// Copy one coherent topology snapshot, or return the required record count.
/// Returns `usize::MAX` on an unavailable session. No partial copy is made.
///
/// # Safety
/// A non-null `out` must be aligned and writable for `capacity` records.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_session_panes(
    id: u64,
    out: *mut TmuxPaneRecord,
    capacity: usize,
) -> usize {
    let session = match crate::ssh::session_snapshot(id) {
        Ok(session) => session,
        Err(_) => return usize::MAX,
    };
    let count = session.panes.len();
    if out.is_null() || capacity < count {
        return count;
    }
    for (index, pane) in session.panes.iter().enumerate() {
        let mut record = TmuxPaneRecord {
            window_id: pane.window_id,
            pane_id: pane.pane_id,
            terminal_id: pane.terminal_id,
            window_name_len: 0,
            selected: u8::from(pane.selected),
            active: u8::from(pane.active),
            reserved: [0; 4],
            window_name: [0; 256],
            pane_name_len: 0,
            reserved_name: [0; 6],
            pane_name: [0; 256],
        };
        let mut length = pane.window_name.len().min(record.window_name.len());
        while !pane.window_name.is_char_boundary(length) {
            length -= 1;
        }
        record.window_name[..length].copy_from_slice(&pane.window_name.as_bytes()[..length]);
        record.window_name_len = length as u16;
        let mut length = pane.pane_name.len().min(record.pane_name.len());
        while !pane.pane_name.is_char_boundary(length) {
            length -= 1;
        }
        record.pane_name[..length].copy_from_slice(&pane.pane_name.as_bytes()[..length]);
        record.pane_name_len = length as u16;
        // The caller provides space for all records; padding is explicit.
        unsafe {
            out.add(index).write(record);
        }
    }
    count
}

/// Return the byte length of the current backend-independent workspace
/// metadata snapshot, or zero when the session is unavailable or the snapshot
/// exceeds the bounded native bridge limit. The result is only a sizing hint:
/// the snapshot can change between this call and `meeterm_workspace_state`.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_workspace_state_size(id: u64) -> usize {
    let Ok(json) = crate::ssh::workspace_snapshot_json(id) else {
        return 0;
    };
    if json.len() > MAX_WORKSPACE_STATE_BYTES {
        0
    } else {
        json.len()
    }
}

/// Copy the current backend-independent workspace metadata JSON into a native
/// caller-owned buffer. If `output` is null or too small, no bytes are copied
/// and the required length is returned. Returning the required length on a
/// size change lets adapters retry without an unbounded allocation.
///
/// # Safety
/// When `output` is non-null and `capacity` is at least the returned length,
/// it must point to writable storage for that many bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_workspace_state(
    id: u64,
    output: *mut u8,
    capacity: usize,
) -> usize {
    let Ok(json) = crate::ssh::workspace_snapshot_json(id) else {
        return 0;
    };
    let bytes = json.as_bytes();
    if bytes.len() > MAX_WORKSPACE_STATE_BYTES {
        return 0;
    }
    if output.is_null() || capacity < bytes.len() {
        return bytes.len();
    }
    if !bytes.is_empty() {
        // SAFETY: the caller promises writable storage for `capacity` bytes;
        // the branch above proves it contains the complete JSON payload.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len()) };
    }
    bytes.len()
}

/// Return the fixed C snapshot size.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_connection_snapshot_size() -> usize {
    std::mem::size_of::<ConnectionSnapshot>()
}

/// Copy a fixed connection snapshot into a caller-owned native struct.
///
/// # Safety
///
/// `out` must be non-null, correctly aligned, and point to writable storage
/// for one complete `ConnectionSnapshot`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_connection_snapshot(id: u64, out: *mut ConnectionSnapshot) -> i32 {
    if out.is_null() {
        return FFI_ERROR;
    }
    let snapshot = match connection_snapshot(id) {
        Ok(snapshot) => snapshot,
        Err(error) => return connection_error_code(error),
    };
    // The caller promises an aligned, writable `ConnectionSnapshot`.
    unsafe { std::ptr::write(out, snapshot) };
    0
}

/// Answer a pending first-seen host-key prompt.  `accept` must be 0 or 1.
///
/// # Safety
///
/// When `fingerprint_length` is non-zero, `fingerprint` must point to that
/// many readable UTF-8 bytes for the duration of this call. The bytes are
/// copied before returning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_respond_host_key(
    id: u64,
    fingerprint: *const u8,
    fingerprint_length: usize,
    accept: u8,
) -> i32 {
    if accept > 1 {
        return ConnectionError::InvalidArgument.code();
    }
    let Ok(fingerprint) = (unsafe { utf8_argument(fingerprint, fingerprint_length) }) else {
        return ConnectionError::InvalidArgument.code();
    };
    respond_to_host_key(id, &fingerprint, accept == 1)
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Forget a host's persisted trust entry.  The platform passes the same
/// app-private path used by `meeterm_connect`.
///
/// # Safety
///
/// Every non-empty pointer must point to the stated number of readable UTF-8
/// bytes for the duration of this call. The byte slices are copied before
/// returning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn meeterm_forget_host_key(
    host: *const u8,
    host_length: usize,
    port: u16,
    known_hosts_path: *const u8,
    known_hosts_path_length: usize,
) -> i32 {
    let (Ok(host), Ok(known_hosts_path)) = (unsafe { utf8_argument(host, host_length) }, unsafe {
        utf8_argument(known_hosts_path, known_hosts_path_length)
    }) else {
        return ConnectionError::InvalidArgument.code();
    };
    forget_host_key(&host, port, Path::new(&known_hosts_path))
        .map(|()| 0)
        .unwrap_or_else(connection_error_code)
}

/// Return the monotonic native content revision, or zero for an invalid ID.
#[unsafe(no_mangle)]
pub extern "C" fn meeterm_terminal_revision(id: u64) -> u64 {
    terminal_revision(id).unwrap_or(0)
}

#[cfg(test)]
mod session_abi_tests {
    use super::*;

    #[test]
    fn pane_record_matches_c_header_layout() {
        assert_eq!(std::mem::size_of::<TmuxPaneRecord>(), 552);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, pane_name_len), 288);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, pane_name), 296);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, window_name_len), 24);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, selected), 26);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, active), 27);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, window_name), 32);
    }

    #[test]
    fn borrowed_handle_and_empty_session_do_not_allocate_panes() {
        let id = meeterm_create_terminal(80, 24);
        assert_eq!(meeterm_terminal_exists(id), 1);
        assert_eq!(
            unsafe { meeterm_session_panes(id, std::ptr::null_mut(), 0) },
            0
        );
        assert_eq!(meeterm_destroy_terminal(id), 1);
        assert_eq!(meeterm_terminal_exists(id), 0);
        assert_eq!(
            unsafe { meeterm_session_panes(id, std::ptr::null_mut(), 0) },
            usize::MAX
        );
    }

    #[test]
    fn visibility_and_workspace_state_abi_reject_invalid_or_unknown_handles() {
        assert_eq!(
            meeterm_set_terminal_visible(0, 2),
            ConnectionError::InvalidArgument.code()
        );
        assert_eq!(meeterm_workspace_state_size(0), 0);
        // SAFETY: a null output is explicitly accepted for the sizing/error
        // path and no bytes may be copied for an unknown terminal.
        assert_eq!(
            unsafe { meeterm_workspace_state(0, std::ptr::null_mut(), 0) },
            0
        );
    }

    #[test]
    fn runtime_discovery_abi_matches_platform_shape_without_private_paths() {
        let id = meeterm_create_terminal(80, 24);
        assert_ne!(id, 0);
        let required = meeterm_runtime_discovery_size(id);
        assert!(required > 0 && required <= MAX_RUNTIME_DISCOVERY_BYTES);
        assert_eq!(required, meeterm_runtime_snapshot_size(id));
        assert_eq!(
            unsafe { meeterm_runtime_discovery(id, std::ptr::null_mut(), 0) },
            required
        );

        let mut bytes = vec![0_u8; required];
        let copied = unsafe { meeterm_runtime_discovery(id, bytes.as_mut_ptr(), bytes.len()) };
        assert_eq!(copied, required);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["connectionGeneration"], "0");
        assert_eq!(value["revision"], 0);
        let backends = value["backends"].as_array().unwrap();
        assert_eq!(backends.len(), 2);
        assert_eq!(backends[0]["backend"], "tmux");
        assert_eq!(backends[0]["state"], "loading");
        assert_eq!(backends[0]["canCreate"], true);
        assert_eq!(backends[1]["backend"], "herdr");
        assert_eq!(backends[1]["state"], "loading");
        assert_eq!(backends[1]["canCreate"], false);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("socket_path"));
        assert!(!text.contains("session_dir"));
        assert!(!text.contains("executable"));
        assert_eq!(meeterm_destroy_terminal(id), 1);
    }

    #[test]
    fn runtime_candidate_bridge_preserves_selection_and_bounded_error_state() {
        let long_message = "あ".repeat(200);
        let section = RuntimeSection {
            state: RuntimeSectionState::Success,
            candidates: vec![
                RuntimeCandidate {
                    id: "running".into(),
                    backend: Backend::Tmux,
                    name: "meeterm".into(),
                    state: RuntimeState::Running,
                    selectable: true,
                    suggested: true,
                    error_code: None,
                    error_message: None,
                },
                RuntimeCandidate {
                    id: "stopped".into(),
                    backend: Backend::Herdr,
                    name: "default".into(),
                    state: RuntimeState::Stopped,
                    selectable: false,
                    suggested: false,
                    error_code: None,
                    error_message: None,
                },
                RuntimeCandidate {
                    id: "failed-selection".into(),
                    backend: Backend::Tmux,
                    name: "work".into(),
                    state: RuntimeState::Running,
                    selectable: false,
                    suggested: false,
                    error_code: Some("runtime_selection".into()),
                    error_message: Some(long_message),
                },
            ],
            error_code: None,
            error_message: None,
        };
        let value = serde_json::to_value(runtime_backend_bridge(Backend::Tmux, &section, true))
            .expect("runtime bridge JSON");
        let candidates = value["candidates"].as_array().expect("candidate array");

        assert_eq!(candidates[0]["state"], "running");
        assert_eq!(candidates[0]["selectable"], true);
        assert_eq!(candidates[0]["errorCode"], "");
        assert_eq!(candidates[0]["errorMessage"], "");

        assert_eq!(candidates[1]["state"], "stopped");
        assert_eq!(candidates[1]["selectable"], false);

        assert_eq!(candidates[2]["selectable"], false);
        assert_eq!(candidates[2]["errorCode"], "runtime_selection");
        let message = candidates[2]["errorMessage"].as_str().unwrap();
        assert!(message.len() <= crate::ssh::ERROR_MESSAGE_CAPACITY);
        assert!(message.is_char_boundary(message.len()));
        assert_eq!(message.chars().count(), 85);
    }

    #[test]
    fn connect_rejects_unknown_or_mixed_authentication_arguments() {
        let id = meeterm_create_terminal(80, 24);
        assert_ne!(id, 0);
        let host = b"127.0.0.1";
        let username = b"meeterm";
        let known_hosts = b"/tmp/meeterm-known-hosts";
        let private_key = b"private-key";
        let passphrase = b"passphrase";
        let password = b"password";
        let unknown = b"keyboardInteractive";
        let password_method = b"password";
        let public_key_method = b"publicKey";

        // SAFETY: every pointer below remains valid for the duration of the
        // synchronous boundary call.
        let result = unsafe {
            meeterm_connect(
                id,
                host.as_ptr(),
                host.len(),
                22,
                username.as_ptr(),
                username.len(),
                private_key.as_ptr(),
                private_key.len(),
                passphrase.as_ptr(),
                passphrase.len(),
                known_hosts.as_ptr(),
                known_hosts.len(),
                unknown.as_ptr(),
                unknown.len(),
                password.as_ptr(),
                password.len(),
            )
        };
        assert_eq!(result, ConnectionError::InvalidArgument.code());

        // A password selection cannot smuggle key material or silently fall
        // back to public-key authentication.
        let result = unsafe {
            meeterm_connect(
                id,
                host.as_ptr(),
                host.len(),
                22,
                username.as_ptr(),
                username.len(),
                private_key.as_ptr(),
                private_key.len(),
                passphrase.as_ptr(),
                passphrase.len(),
                known_hosts.as_ptr(),
                known_hosts.len(),
                password_method.as_ptr(),
                password_method.len(),
                password.as_ptr(),
                password.len(),
            )
        };
        assert_eq!(result, ConnectionError::InvalidArgument.code());

        // A public-key selection cannot accept a password field.
        let result = unsafe {
            meeterm_connect(
                id,
                host.as_ptr(),
                host.len(),
                22,
                username.as_ptr(),
                username.len(),
                private_key.as_ptr(),
                private_key.len(),
                passphrase.as_ptr(),
                passphrase.len(),
                known_hosts.as_ptr(),
                known_hosts.len(),
                public_key_method.as_ptr(),
                public_key_method.len(),
                password.as_ptr(),
                password.len(),
            )
        };
        assert_eq!(result, ConnectionError::InvalidArgument.code());
        assert_eq!(meeterm_destroy_terminal(id), 1);
    }

    #[test]
    fn operation_epoch_abi_rejects_stale_input_and_recovery_tokens() {
        let id = meeterm_create_terminal(80, 24);
        assert_ne!(id, 0);
        let epoch = meeterm_operation_epoch(id);
        assert_ne!(epoch, 0);

        let input = b"epoch";
        // SAFETY: the byte slice remains valid for the synchronous call.
        assert_eq!(
            unsafe { meeterm_commit_utf8_at_epoch(id, epoch, input.as_ptr(), input.len()) },
            1
        );
        // A delayed callback from the prior terminal epoch is rejected before
        // it can change the native commit count.
        assert_eq!(
            unsafe {
                meeterm_commit_utf8_at_epoch(
                    id,
                    epoch.saturating_add(1),
                    input.as_ptr(),
                    input.len(),
                )
            },
            0
        );
        assert_eq!(meeterm_input_commit_count(id), 1);
        assert_eq!(
            meeterm_resize_terminal_at_epoch(id, epoch.saturating_add(1), 80, 24),
            FFI_ERROR
        );

        let control_token = b"token\n";
        // SAFETY: the token slice remains valid for the synchronous call.
        assert_eq!(
            unsafe { meeterm_confirm_recovery(id, control_token.as_ptr(), control_token.len()) },
            ConnectionError::InvalidArgument.code()
        );
        assert_eq!(
            unsafe { meeterm_confirm_recovery(id, std::ptr::null(), 0) },
            ConnectionError::InvalidArgument.code()
        );
        let oversized_token = vec![b'x'; MAX_RECOVERY_TOKEN_BYTES + 1];
        assert_eq!(
            unsafe {
                meeterm_confirm_recovery(id, oversized_token.as_ptr(), oversized_token.len())
            },
            ConnectionError::InvalidArgument.code()
        );
        assert_eq!(meeterm_destroy_terminal(id), 1);
    }
}
