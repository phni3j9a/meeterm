//! JNI entry points for the Android Expo module.
//!
//! The Android view talks to this library through these methods.  The actual
//! terminal state remains behind the safe Rust registry; JNI only converts
//! primitive values and byte arrays at the boundary.

use jni::errors::Error as JniError;
use jni::objects::{JByteArray, JObject, JObjectArray, JString};
use jni::sys::{jboolean, jint, jlong};
use jni::{Env, EnvUnowned, Outcome};

use crate::registry;
use crate::ssh::{AuthOptions, ConnectOptions, ConnectionError, ConnectionSnapshot};
use crate::terminal::TerminalError;
use zeroize::Zeroizing;

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

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sendKey(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    key: jint,
    modifiers: jint,
) -> jint {
    let (Some(handle), Ok(key), Ok(modifiers)) = (
        handle_from_jlong(handle),
        u32::try_from(key),
        u32::try_from(modifiers),
    ) else {
        return -1;
    };
    crate::ffi::meeterm_send_key(handle, key, modifiers)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_commitModified<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    bytes: JByteArray<'caller>,
    modifiers: jint,
) -> jint {
    let (Some(handle), Ok(modifiers)) = (handle_from_jlong(handle), u32::try_from(modifiers))
    else {
        return -1;
    };
    code_from_outcome(unowned_env.with_env(|env| {
        let bytes = env.convert_byte_array(&bytes)?;
        Ok(unsafe {
            crate::ffi::meeterm_commit_modified_utf8(handle, bytes.as_ptr(), bytes.len(), modifiers)
        })
    }))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_selectStart(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    row: jint,
    column: jint,
) -> jint {
    let (Some(handle), Ok(row), Ok(column)) = (
        handle_from_jlong(handle),
        u32::try_from(row),
        u32::try_from(column),
    ) else {
        return -1;
    };
    crate::ffi::meeterm_select_start(handle, row, column)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_selectUpdate(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    row: jint,
    column: jint,
) -> jint {
    let (Some(handle), Ok(row), Ok(column)) = (
        handle_from_jlong(handle),
        u32::try_from(row),
        u32::try_from(column),
    ) else {
        return -1;
    };
    crate::ffi::meeterm_select_update(handle, row, column)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_clearSelection(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ffi::meeterm_clear_selection(handle)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_selectionText<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) -> JString<'caller> {
    let Some(handle) = handle_from_jlong(handle) else {
        return JString::default();
    };
    let result = unowned_env
        .with_env(|env| -> Result<_, JniError> {
            let text = registry::selection_text(handle).map_err(native_error)?;
            match text {
                Some(text) if text.len() <= 4 * 1024 * 1024 => env.new_string(text),
                _ => Ok(JString::default()),
            }
        })
        .into_outcome();
    match result {
        Outcome::Ok(text) => text,
        Outcome::Err(_) | Outcome::Panic(_) => JString::default(),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_setTheme(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    light: jboolean,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ffi::meeterm_set_theme(handle, u8::from(light))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_setScrollbackLimit(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    lines: jint,
) -> jint {
    let Ok(lines) = u32::try_from(lines) else {
        return -1;
    };
    crate::ffi::meeterm_set_scrollback_limit(lines)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_tmuxCommand<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    operation: jint,
    target: jlong,
    name: JString<'caller>,
) -> jint {
    let (Some(handle), Ok(operation), Ok(target)) = (
        handle_from_jlong(handle),
        u32::try_from(operation),
        u64::try_from(target),
    ) else {
        return -1;
    };
    code_from_outcome(unowned_env.with_env(|env| {
        let name = string_from_java(env, &name)?;
        // The string owns its bytes for the entire FFI call.
        Ok(unsafe {
            crate::ffi::meeterm_tmux_command(handle, operation, target, name.as_ptr(), name.len())
        })
    }))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_setForeground(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    foreground: jboolean,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ffi::meeterm_set_foreground(handle, u8::from(foreground))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_setTerminalVisible(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    visible: jboolean,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ffi::meeterm_set_terminal_visible(handle, u8::from(visible))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_setAutomaticReconnect(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    enabled: jboolean,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ffi::meeterm_set_automatic_reconnect(handle, u8::from(enabled))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_terminalExists(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jboolean {
    handle_from_jlong(handle)
        .is_some_and(|id| registry::shared_terminal(id).is_ok())
        .into()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshReconnect(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ssh::reconnect_terminal(handle)
        .map(|()| 0)
        .unwrap_or_else(|e| e.code())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_tmuxSelectPane(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    pane: jlong,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    let Ok(pane) = u64::try_from(pane) else {
        return -1;
    };
    crate::ssh::select_pane(handle, pane)
        .map(|()| 0)
        .unwrap_or_else(|e| e.code())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_tmuxSessionState<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) -> JObjectArray<'caller> {
    let Some(handle) = handle_from_jlong(handle) else {
        return JObjectArray::default();
    };
    let result = unowned_env
        .with_env(|env| -> Result<_, JniError> {
            let session = crate::ssh::session_snapshot(handle)
                .map_err(|e| JniError::ParseFailed(e.to_string()))?;
            let length = session
                .panes
                .len()
                .checked_mul(7)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| JniError::ParseFailed("Session too large".into()))?;
            let array =
                env.new_object_array(length, jni::jni_str!("java/lang/String"), JObject::null())?;
            for (index, pane) in session.panes.iter().enumerate() {
                let fields = [
                    pane.window_id.to_string(),
                    pane.pane_id.to_string(),
                    pane.terminal_id.to_string(),
                    pane.window_name.clone(),
                    u8::from(pane.selected).to_string(),
                    u8::from(pane.active).to_string(),
                    pane.pane_name.clone(),
                ];
                for (field, value) in fields.iter().enumerate() {
                    let value = env.new_string(value)?;
                    array.set_element(env, index * fields.len() + field, &value)?;
                }
            }
            Ok(array)
        })
        .into_outcome();
    match result {
        Outcome::Ok(array) => array,
        Outcome::Err(_) | Outcome::Panic(_) => JObjectArray::default(),
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

    let outcome = unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let snapshot = registry::snapshot(handle).map_err(native_error)?;
            env.byte_array_from_slice(snapshot.as_bytes())
        })
        .into_outcome();
    // Remote panes can disappear between native frame scheduling and this
    // lookup. A stale borrowed ID means no frame, never a renderer exception.
    match outcome {
        Outcome::Ok(bytes) => bytes,
        Outcome::Err(_) | Outcome::Panic(_) => JByteArray::default(),
    }
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

/// Paste UTF-8 using the mode owned by the shared terminal.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_paste<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    bytes: JByteArray<'caller>,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    let outcome = unowned_env
        .with_env(|env| -> jni::errors::Result<_> {
            let bytes = env.convert_byte_array(&bytes)?;
            Ok(unsafe { crate::ffi::meeterm_paste_utf8(handle, bytes.as_ptr(), bytes.len()) })
        })
        .into_outcome();
    match outcome {
        Outcome::Ok(result) => result,
        Outcome::Err(_) | Outcome::Panic(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_scrollLines(
    _env: EnvUnowned<'_>,
    _this: JObject<'_>,
    handle: jlong,
    lines: jint,
) -> jint {
    let Some(handle) = handle_from_jlong(handle) else {
        return -1;
    };
    crate::ffi::meeterm_scroll_lines(handle, lines)
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
/// The private key is an inline OpenSSH/PEM string for the `publicKey` method.
/// `auth_method` and `password` are appended after the existing
/// `known_hosts_path` argument. An empty method retains the legacy public-key
/// default; only `publicKey` and `password` are otherwise accepted. Password
/// input is passed byte-for-byte, including whitespace. The platform owns the
/// path passed as `known_hosts_path`; Rust owns parsing, trust decisions, and
/// persistence. A negative return value is a stable native error code.
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
    auth_method: JString<'caller>,
    password: JString<'caller>,
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
        // Wrap credentials as soon as they cross JNI so a later conversion
        // error cannot leave earlier secrets in ordinary String storage.
        let private_key = Zeroizing::new(string_from_java(env, &private_key)?);
        let passphrase = Zeroizing::new(string_from_java(env, &passphrase)?);
        let known_hosts_path = string_from_java(env, &known_hosts_path)?;
        let auth_method = string_from_java(env, &auth_method)?;
        let password = Zeroizing::new(string_from_java(env, &password)?);
        let credentials = match auth_method.as_str() {
            "" | "publicKey" => {
                if !password.is_empty() {
                    return Ok(ConnectionError::InvalidArgument.code());
                }
                AuthOptions::PublicKey {
                    private_key,
                    passphrase: (!passphrase.is_empty()).then_some(passphrase),
                }
            }
            "password" => {
                if !private_key.is_empty() || !passphrase.is_empty() {
                    return Ok(ConnectionError::InvalidArgument.code());
                }
                AuthOptions::Password { password }
            }
            _ => {
                return Ok(ConnectionError::InvalidArgument.code());
            }
        };
        let options = ConnectOptions {
            host,
            port,
            username,
            credentials,
            known_hosts_path: known_hosts_path.into(),
            backend: crate::workspace::Backend::Tmux,
            runtime: None,
        };
        Ok(crate::ssh::connect_terminal(handle, options)
            .map(|()| 0)
            .unwrap_or_else(|error| error.code()))
    }))
}

/// Start an SSH connection for an explicit backend/runtime while preserving
/// the legacy `sshConnect` entry point above for existing Android callers.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_sshConnectBackend<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    host: JString<'caller>,
    port: jint,
    username: JString<'caller>,
    private_key: JString<'caller>,
    passphrase: JString<'caller>,
    known_hosts_path: JString<'caller>,
    auth_method: JString<'caller>,
    password: JString<'caller>,
    backend: JString<'caller>,
    runtime: JString<'caller>,
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
        let private_key = Zeroizing::new(string_from_java(env, &private_key)?);
        let passphrase = Zeroizing::new(string_from_java(env, &passphrase)?);
        let known_hosts_path = string_from_java(env, &known_hosts_path)?;
        let auth_method = string_from_java(env, &auth_method)?;
        let password = Zeroizing::new(string_from_java(env, &password)?);
        let backend = string_from_java(env, &backend)?;
        let runtime = string_from_java(env, &runtime)?;
        let credentials = match auth_method.as_str() {
            "" | "publicKey" => {
                if !password.is_empty() {
                    return Ok(ConnectionError::InvalidArgument.code());
                }
                AuthOptions::PublicKey {
                    private_key,
                    passphrase: (!passphrase.is_empty()).then_some(passphrase),
                }
            }
            "password" => {
                if !private_key.is_empty() || !passphrase.is_empty() {
                    return Ok(ConnectionError::InvalidArgument.code());
                }
                AuthOptions::Password { password }
            }
            _ => return Ok(ConnectionError::InvalidArgument.code()),
        };
        let backend = crate::workspace::Backend::parse(&backend)
            .ok_or_else(|| JniError::ParseFailed("invalid backend".into()))?;
        let options = ConnectOptions {
            host,
            port,
            username,
            credentials,
            known_hosts_path: known_hosts_path.into(),
            backend,
            runtime: (!runtime.is_empty()).then_some(runtime),
        };
        Ok(crate::ssh::connect_terminal(handle, options)
            .map(|()| 0)
            .unwrap_or_else(|error| error.code()))
    }))
}

fn workspace_json(handle: u64) -> Option<String> {
    let mut capacity = crate::ffi::meeterm_workspace_state_size(handle);
    // A topology snapshot is low-frequency and bounded. Retry a bounded
    // number of times when the remote topology changes between size/copy.
    for _ in 0..4 {
        if capacity == 0 || capacity > crate::ffi::MAX_WORKSPACE_STATE_BYTES {
            return None;
        }
        let mut bytes = vec![0_u8; capacity];
        let copied =
            unsafe { crate::ffi::meeterm_workspace_state(handle, bytes.as_mut_ptr(), bytes.len()) };
        if copied > bytes.len() {
            capacity = copied;
            continue;
        }
        if copied == 0 || copied > crate::ffi::MAX_WORKSPACE_STATE_BYTES {
            return None;
        }
        bytes.truncate(copied);
        return String::from_utf8(bytes).ok();
    }
    None
}

/// Return the bounded backend-independent workspace metadata JSON. The
/// Android adapter parses this low-frequency object; terminal bytes/cells
/// never cross JNI.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_meeterm_terminal_MeetermNative_workspaceState<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) -> JString<'caller> {
    let Some(handle) = handle_from_jlong(handle) else {
        return JString::default();
    };
    let outcome = unowned_env.with_env(|env| -> Result<JString<'caller>, JniError> {
        match workspace_json(handle) {
            Some(json) => env.new_string(json),
            None => Ok(JString::default()),
        }
    });
    match outcome.into_outcome() {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => JString::default(),
    }
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
