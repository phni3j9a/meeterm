use std::path::Path;
use std::slice;

use crate::input::SpecialKey;
use crate::registry;
use crate::ssh::{
    ConnectOptions, ConnectionError, ConnectionSnapshot, connect_terminal, connection_snapshot,
    disconnect_terminal, forget_host_key, respond_to_host_key, terminal_revision,
};

const FFI_ERROR: i32 = -1;
const FFI_INVALID_KEY: i32 = -2;

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

/// Start a public-key SSH connection.  All string arguments are UTF-8 byte
/// slices; the platform supplies the app-private known-hosts path.
///
/// `passphrase_length == 0` means that the key is unencrypted.  The
/// passphrase is copied into the short-lived connection task and never placed
/// in a snapshot or log.
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
) -> i32 {
    let (Ok(host), Ok(username), Ok(private_key), Ok(passphrase), Ok(known_hosts_path)) = (
        unsafe { utf8_argument(host, host_length) },
        unsafe { utf8_argument(username, username_length) },
        unsafe { utf8_argument(private_key, private_key_length) },
        unsafe { utf8_argument(passphrase, passphrase_length) },
        unsafe { utf8_argument(known_hosts_path, known_hosts_path_length) },
    ) else {
        return ConnectionError::InvalidArgument.code();
    };

    connect_terminal(
        id,
        ConnectOptions {
            host,
            port,
            username,
            private_key,
            passphrase: if passphrase.is_empty() {
                None
            } else {
                Some(passphrase)
            },
            known_hosts_path: known_hosts_path.into(),
        },
    )
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
    pub reserved: [u8; 5],
    pub window_name: [u8; 256],
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
            reserved: [0; 5],
            window_name: [0; 256],
        };
        let mut length = pane.window_name.len().min(record.window_name.len());
        while !pane.window_name.is_char_boundary(length) {
            length -= 1;
        }
        record.window_name[..length].copy_from_slice(&pane.window_name.as_bytes()[..length]);
        record.window_name_len = length as u16;
        // The caller provides space for all records; padding is explicit.
        unsafe {
            out.add(index).write(record);
        }
    }
    count
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
        assert_eq!(std::mem::size_of::<TmuxPaneRecord>(), 288);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, window_name_len), 24);
        assert_eq!(std::mem::offset_of!(TmuxPaneRecord, selected), 26);
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
}
