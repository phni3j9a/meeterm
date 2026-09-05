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
