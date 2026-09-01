//! JNI entry points for the Android Expo module.
//!
//! The Android view talks to this library through these methods.  The actual
//! terminal state remains behind the safe Rust registry; JNI only converts
//! primitive values and byte arrays at the boundary.

use jni::EnvUnowned;
use jni::errors::{Error as JniError, ThrowRuntimeExAndDefault};
use jni::objects::{JByteArray, JObject};
use jni::sys::{jint, jlong};

use crate::registry;
use crate::terminal::TerminalError;

fn native_error(error: TerminalError) -> JniError {
    JniError::ParseFailed(error.to_string())
}

fn handle_from_jlong(handle: jlong) -> Option<u64> {
    u64::try_from(handle).ok().filter(|handle| *handle != 0)
}

fn dimensions_from_jint(columns: jint, rows: jint) -> Option<(u16, u16)> {
    Some((u16::try_from(columns).ok()?, u16::try_from(rows).ok()?))
}

/// Create or restore a registry-backed terminal.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_create(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    columns: jint,
    rows: jint,
) -> jlong {
    let Some((columns, rows)) = dimensions_from_jint(columns, rows) else {
        return 0;
    };
    registry::create_terminal(columns, rows)
        .ok()
        .and_then(|handle| jlong::try_from(handle).ok())
        .unwrap_or(0)
}

/// Return the current native-only little-endian snapshot as a Java byte array.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_snapshot<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) -> JByteArray<'caller> {
    let Some(handle) = handle_from_jlong(handle) else {
        return JByteArray::default();
    };

    unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let snapshot = registry::snapshot(handle).map_err(native_error)?;
            env.byte_array_from_slice(snapshot.as_bytes())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Resize a registry-backed terminal. Zero indicates success.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_resize(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    columns: jint,
    rows: jint,
) -> jint {
    let (Some(handle), Some((columns, rows))) = (
        handle_from_jlong(handle),
        dimensions_from_jint(columns, rows),
    ) else {
        return -1;
    };

    registry::resize_terminal(handle, columns, rows)
        .map(|()| 0)
        .unwrap_or(-1)
}

/// Commit one already encoded UTF-8 byte array and return its native count.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_commit<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    bytes: JByteArray<'caller>,
) -> jlong {
    let Some(handle) = handle_from_jlong(handle) else {
        return 0;
    };

    unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let bytes = env.convert_byte_array(&bytes)?;
            registry::commit_utf8(handle, &bytes)
                .map(jlong::try_from)
                .map_err(native_error)?
                .map_err(|_| JniError::ParseFailed("commit count overflow".to_owned()))
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// Send one explicit terminal special key.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sendSpecial(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    key: jint,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    let key = u32::try_from(key).unwrap_or(u32::MAX);

    crate::ffi::meeterm_send_special_key(handle, key)
}

/// Return the number of successful native commit operations.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_inputCommitCount(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jlong {
    let Some(handle) = handle_from_jlong(handle) else {
        return 0;
    };
    registry::input_commit_count(handle)
        .ok()
        .and_then(|count| jlong::try_from(count).ok())
        .unwrap_or(0)
}

/// Explicitly remove a terminal; the Android view does not call this during
/// normal unmount/recreation.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_destroy(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return 0;
    };
    jint::from(registry::destroy_terminal(handle))
}
