use std::slice;

use crate::input::SpecialKey;
use crate::registry;

const FFI_ERROR: i32 = -1;
const FFI_INVALID_KEY: i32 = -2;

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
