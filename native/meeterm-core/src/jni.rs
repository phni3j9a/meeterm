//! JNI entry points for the Android Expo module.
//!
//! The Android view talks to this library through these methods.  The actual
//! terminal state remains behind the safe Rust registry; JNI only converts
//! primitive values and byte arrays at the boundary.

use jni::errors::{Error as JniError, ThrowRuntimeExAndDefault};
use jni::objects::{JByteArray, JObject, JObjectArray, JString};
use jni::sys::{jboolean, jint, jlong};
use jni::{Env, EnvUnowned, Outcome};

use crate::registry;
use crate::ssh::{ConnectOptions, ConnectionSnapshot};
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

fn string_from_java(env: &Env<'_>, value: &JString<'_>) -> Result<String, JniError> {
    value.try_to_string(env)
}

fn snapshot_string(bytes: &[u8], length: u16) -> String {
    let length = usize::from(length).min(bytes.len());
    String::from_utf8_lossy(&bytes[..length]).into_owned()
}

fn code_from_outcome(outcome: jni::EnvOutcome<'_, jint, JniError>) -> jint {
    match outcome.into_outcome() {
        Outcome::Ok(code) => code,
        // Control-plane methods use a sentinel instead of allowing malformed
        // Java arguments or a caught panic to escape as a RuntimeException.
        Outcome::Err(_) | Outcome::Panic(_) => -1,
    }
}

fn snapshot_array<'local>(
    env: &mut Env<'local>,
    snapshot: ConnectionSnapshot,
) -> Result<JObjectArray<'local>, JniError> {
    let array = env.new_object_array(8, jni::jni_str!("java/lang/String"), JObject::null())?;
    let values = [
        snapshot.state.to_string(),
        snapshot_string(&snapshot.host, snapshot.host_len),
        snapshot.port.to_string(),
        snapshot_string(&snapshot.fingerprint, snapshot.fingerprint_len),
        snapshot_string(&snapshot.algorithm, snapshot.algorithm_len),
        snapshot_string(&snapshot.known_fingerprint, snapshot.known_fingerprint_len),
        snapshot_string(&snapshot.error_code, snapshot.error_code_len),
        snapshot_string(&snapshot.error_message, snapshot.error_message_len),
    ];
    for (index, value) in values.iter().enumerate() {
        let java_value = env.new_string(value)?;
        array.set_element(env, index, &java_value)?;
    }
    Ok(array)
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

    let outcome = unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let bytes = env.convert_byte_array(&bytes)?;
            // Input rejection is a normal transient condition while an SSH
            // connection opens or closes. Return the same zero sentinel as
            // the C ABI instead of throwing through the IME callback.
            Ok(registry::commit_utf8(handle, &bytes)
                .ok()
                .and_then(|count| jlong::try_from(count).ok())
                .unwrap_or(0))
        })
        .into_outcome();
    match outcome {
        Outcome::Ok(count) => count,
        Outcome::Err(_) | Outcome::Panic(_) => 0,
    }
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

/// Start an SSH connection for an existing terminal.
///
/// The private key is an inline OpenSSH/PEM string.  The platform owns the
/// path passed as `known_hosts_path`; Rust owns parsing, trust decisions, and
/// persistence.  A negative return value is a stable native error code.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshConnect<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    host: JString<'caller>,
    port: jint,
    username: JString<'caller>,
    private_key: JString<'caller>,
    passphrase: JString<'caller>,
    known_hosts_path: JString<'caller>,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -2;
    };
    let Some(port) = u16::try_from(port).ok() else {
        return -1;
    };

    code_from_outcome(unowned_env.with_env(|env| {
        let host = string_from_java(env, &host)?;
        let username = string_from_java(env, &username)?;
        let private_key = string_from_java(env, &private_key)?;
        let passphrase = string_from_java(env, &passphrase)?;
        let known_hosts_path = string_from_java(env, &known_hosts_path)?;
        let options = ConnectOptions {
            host,
            port,
            username,
            private_key,
            passphrase: (!passphrase.is_empty()).then_some(passphrase),
            known_hosts_path: known_hosts_path.into(),
        };
        Ok(crate::ssh::connect_terminal(handle, options)
            .map(|()| 0)
            .unwrap_or_else(|error| error.code()))
    }))
}

/// Cancel the SSH lifecycle for a terminal.  The terminal remains registered
/// so the native state can be polled while it transitions to Disconnected.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshDisconnect(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -2;
    };
    crate::ssh::disconnect_terminal(handle)
        .map(|()| 0)
        .map_err(|error| error.code())
        .unwrap_or(-1)
}

/// Return the fixed eight-field connection state array used by the Android
/// adapter.  Field order is state, host, port, fingerprint, algorithm,
/// knownFingerprint, errorCode, and errorMessage.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshConnectionState<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) -> JObjectArray<'caller> {
    let Some(handle) = handle_from_jlong(handle) else {
        return JObjectArray::default();
    };

    let outcome = unowned_env
        .with_env(|env| {
            let snapshot = crate::ssh::connection_snapshot(handle)
                .map_err(|error| JniError::ParseFailed(error.to_string()))?;
            snapshot_array(env, snapshot)
        })
        .into_outcome();

    match outcome {
        Outcome::Ok(array) => array,
        Outcome::Err(_) | Outcome::Panic(_) => JObjectArray::default(),
    }
}

/// Respond to the current host-key prompt.  The fingerprint must match the
/// pending prompt exactly; Rust persists an accepted key before continuing.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshRespondHostKey<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    fingerprint: JString<'caller>,
    accept: jboolean,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -2;
    };
    code_from_outcome(unowned_env.with_env(|env| {
        let fingerprint = string_from_java(env, &fingerprint)?;
        Ok(
            crate::ssh::respond_to_host_key(handle, &fingerprint, accept)
                .map(|()| 0)
                .unwrap_or_else(|error| error.code()),
        )
    }))
}

/// Remove the exact trusted host/port record from the app-private trust file.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshForgetHostKey<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    host: JString<'caller>,
    port: jint,
    known_hosts_path: JString<'caller>,
) -> jint {
    let Some(port) = u16::try_from(port).ok() else {
        return -1;
    };
    code_from_outcome(unowned_env.with_env(|env| {
        let host = string_from_java(env, &host)?;
        let known_hosts_path = string_from_java(env, &known_hosts_path)?;
        Ok(
            crate::ssh::forget_host_key(&host, port, known_hosts_path.as_ref())
                .map(|()| 0)
                .unwrap_or_else(|error| error.code()),
        )
    }))
}

/// Return the terminal content revision used by the adapter's low-frequency
/// polling loop.  Terminal bytes and render frames never cross JNI.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_terminalRevision(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jlong {
    let Some(handle) = handle_from_jlong(handle) else {
        return 0;
    };
    crate::ssh::terminal_revision(handle)
        .ok()
        .and_then(|revision| jlong::try_from(revision).ok())
        .unwrap_or(0)
}
