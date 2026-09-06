//! Bounded real-OpenSSH coverage for the native SSH and terminal boundary.
//!
//! The test is ignored by default because it needs the disposable server from
//! `scripts/ssh/fixture.py`.  It intentionally uses only the public Rust API
//! and the native snapshot/input functions so it remains close to the mobile
//! integration boundary.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use meeterm_core::{
    ConnectOptions, ConnectionSnapshot, ConnectionState, SpecialKey, connect_terminal,
    connection_snapshot, create_terminal, destroy_terminal, disconnect_terminal,
    meeterm_commit_utf8, meeterm_input_commit_count, meeterm_resize_terminal,
    meeterm_send_special_key, meeterm_snapshot, meeterm_snapshot_size, respond_to_host_key,
    send_bytes,
};

const WAIT_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SNAPSHOT_HEADER_SIZE: usize = 28;
const SNAPSHOT_CELL_METADATA_SIZE: usize = 28;
const BOLD_FLAG: u16 = 0b10;

const SYNC_MARKER: &str = "MEETERM_SSH_SYNC_6B39";
const ANSI_MARKER: &str = "MEETERM_SSH_ANSI_4D12";
const LS_DONE_MARKER: &str = "MEETERM_SSH_LS_DONE_7E20";
const JAPANESE_DONE_MARKER: &str = "MEETERM_SSH_JA_DONE_91AC";
const IDLE_DONE_MARKER: &str = "MEETERM_SSH_IDLE_DONE_2F58";
const JAPANESE_TEXT: &str = "日本語";

struct FixtureConfig {
    host: String,
    port: u16,
    username: String,
    private_key: String,
    passphrase: String,
    fingerprint: String,
    known_hosts: PathBuf,
}

struct TerminalGuard {
    id: u64,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disconnect_terminal(self.id);
        let _ = destroy_terminal(self.id);
    }
}

#[derive(Debug)]
struct DecodedSnapshot {
    columns: u32,
    rows: u32,
    cells: Vec<DecodedCell>,
}

#[derive(Debug)]
struct DecodedCell {
    row: u32,
    column: u32,
    width: u8,
    flags: u16,
    foreground: [u8; 4],
    base: String,
    combining: String,
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py to provide a real local sshd"]
fn real_openssh_shell() {
    let fixture = FixtureConfig::from_environment();
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_terminal(id, fixture.options()).expect("start SSH connection");
    let host_prompt = wait_for_state(id, ConnectionState::HostKeyPending, "host-key prompt");
    assert_eq!(
        connection_string(&host_prompt.fingerprint, host_prompt.fingerprint_len),
        fixture.fingerprint
    );
    assert_eq!(
        connection_string(&host_prompt.algorithm, host_prompt.algorithm_len),
        "ssh-ed25519"
    );
    respond_to_host_key(id, &fixture.fingerprint, true).expect("accept expected host key");
    let ready = wait_for_state(id, ConnectionState::Ready, "encrypted-key authentication");
    assert_eq!(
        connection_string(&ready.algorithm, ready.algorithm_len),
        "ssh-ed25519"
    );

    // The fixture account may use zsh, whose line editor can redraw input
    // after the tty echo flag is cleared.  Replace it with a deterministic
    // interactive POSIX shell before checking any output.
    send_line(id, "exec /bin/sh -i");
    sleep(Duration::from_millis(100));

    // The command line is echoed before `stty -echo` takes effect.  Encode
    // every expected marker as octal so a match can only come from the remote
    // printf output, never from the echoed command itself.
    let sync_command = format!("stty -echo; printf '{}\\n'", printf_octal(SYNC_MARKER));
    assert!(!sync_command.contains(SYNC_MARKER));
    send_line(id, &sync_command);
    wait_for_text(id, SYNC_MARKER, "echo-disable synchronization");

    let ansi_command = format!(
        "printf '\\033[1;31m{}\\033[0m\\n'",
        printf_octal(ANSI_MARKER)
    );
    assert!(!ansi_command.contains(ANSI_MARKER));
    send_line(id, &ansi_command);
    wait_for_snapshot(id, "ANSI styled output", |snapshot| {
        has_red_bold_marker(snapshot, ANSI_MARKER)
    });

    let ls_command = format!("ls -d /tmp; printf '{}\\n'", printf_octal(LS_DONE_MARKER));
    assert!(!ls_command.contains(LS_DONE_MARKER));
    send_line(id, &ls_command);
    let ls_snapshot = wait_for_text(id, LS_DONE_MARKER, "real ls output completion");
    // The command text contains `/tmp`; exactly one occurrence proves that
    // the value came from ls after echo was disabled.
    assert_eq!(snapshot_text(&ls_snapshot).matches("/tmp").count(), 1);

    // Send the committed CJK text through the native UTF-8 path inside a
    // shell printf command.  Echo is disabled, so the server's one output is
    // an unambiguous round trip rather than a local line-discipline echo.
    send_line(id, "export LC_ALL=C.UTF-8");
    send_raw(id, b"printf \"");
    let committed = JAPANESE_TEXT.as_bytes();
    // SAFETY: `committed` remains alive and contains valid UTF-8 for the
    // supplied length while the native function copies it into its queue.
    assert_eq!(
        unsafe { meeterm_commit_utf8(id, committed.as_ptr(), committed.len()) },
        1
    );
    assert_eq!(meeterm_input_commit_count(id), 1);
    let japanese_suffix = format!("\\n{}\\n\"", printf_octal(JAPANESE_DONE_MARKER));
    assert!(!japanese_suffix.contains(JAPANESE_TEXT));
    assert!(!japanese_suffix.contains(JAPANESE_DONE_MARKER));
    send_raw(id, japanese_suffix.as_bytes());
    assert_eq!(meeterm_send_special_key(id, SpecialKey::Enter as u32), 1);
    let japanese_snapshot = wait_for_text(
        id,
        JAPANESE_DONE_MARKER,
        "committed Japanese output completion",
    );
    assert_eq!(
        snapshot_text(&japanese_snapshot)
            .matches(JAPANESE_TEXT)
            .count(),
        1
    );

    resize_and_check_remote_size(id, 100, 30, "30 100");
    resize_and_check_remote_size(id, 60, 18, "18 60");

    // A shell that closes normally must leave the native lifecycle in its
    // stable disconnected state.  The guard still owns cleanup if this wait
    // fails and the process is interrupted.
    send_line(id, "exit");
    wait_for_state(id, ConnectionState::Disconnected, "normal shell exit");

    // The accepted host key is persisted in the fixture trust store.  A new
    // connection reaches Ready directly; seeing HostKeyPending here would
    // mean the pin was lost or silently bypassed.
    connect_terminal(id, fixture.options()).expect("start pinned reconnect");
    wait_for_ready_without_prompt(id, "pinned reconnect");

    // Keep a healthy Ready shell idle longer than the transport reader's
    // stage timeout.  The marker is octal encoded so even an accidental tty
    // echo cannot make the assertion pass before the sleep completes.
    send_line(id, "exec /bin/sh -i");
    sleep(Duration::from_millis(100));
    let reconnect_sync = format!("stty -echo; printf '{}\\n'", printf_octal(SYNC_MARKER));
    assert!(!reconnect_sync.contains(SYNC_MARKER));
    send_line(id, &reconnect_sync);
    wait_for_text(id, SYNC_MARKER, "pinned reconnect echo synchronization");

    let idle_command = format!("sleep 32; printf '{}\\n'", printf_octal(IDLE_DONE_MARKER));
    assert!(!idle_command.contains(IDLE_DONE_MARKER));
    send_line(id, &idle_command);
    wait_for_text_with_timeout(
        id,
        IDLE_DONE_MARKER,
        "healthy idle Ready shell",
        Duration::from_secs(45),
    );
    let idle_state = connection_snapshot(id).expect("connection snapshot after idle shell");
    assert_eq!(idle_state.state, ConnectionState::Ready as u32);

    // Explicit native disconnect must leave the handle observable as
    // Disconnected and reject further input while retaining the terminal ID.
    disconnect_terminal(id).expect("explicit native disconnect");
    wait_for_state(id, ConnectionState::Disconnected, "explicit disconnect");
    assert!(send_bytes(id, b"input after disconnect").is_err());
    assert!(meeterm_send_special_key(id, SpecialKey::Enter as u32) < 0);
}

impl FixtureConfig {
    fn from_environment() -> Self {
        let port = value("MEETERM_SSH_PORT")
            .parse::<u16>()
            .expect("MEETERM_SSH_PORT must be a u16");
        assert!(port > 1024);
        Self {
            host: value("MEETERM_SSH_HOST"),
            port,
            username: value("MEETERM_SSH_USERNAME"),
            // ConnectOptions deliberately receives the PEM text as a String
            // because the core decodes it before opening the SSH session.
            private_key: fs::read_to_string(value("MEETERM_SSH_PRIVATE_KEY_FILE"))
                .expect("fixture private key file must be readable"),
            passphrase: value("MEETERM_SSH_PASSPHRASE"),
            fingerprint: value("MEETERM_SSH_FINGERPRINT"),
            known_hosts: PathBuf::from(value("MEETERM_SSH_KNOWN_HOSTS_FILE")),
        }
    }

    fn options(&self) -> ConnectOptions {
        ConnectOptions {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            private_key: self.private_key.clone(),
            passphrase: Some(self.passphrase.clone()),
            known_hosts_path: self.known_hosts.clone(),
        }
    }
}

fn value(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!("missing {name}; run this ignored test through scripts/ssh/fixture.py")
    })
}

fn send_raw(id: u64, bytes: &[u8]) {
    assert_eq!(
        send_bytes(id, bytes).expect("enqueue native SSH input"),
        bytes.len()
    );
}

fn send_line(id: u64, line: &str) {
    send_raw(id, line.as_bytes());
    assert_eq!(
        meeterm_send_special_key(id, SpecialKey::Enter as u32),
        1,
        "native Enter should be accepted"
    );
}

fn wait_for_state(id: u64, expected: ConnectionState, label: &str) -> ConnectionSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        if snapshot.state == expected as u32 {
            return snapshot;
        }
        if snapshot.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: state={}, errorCode={}",
                state_name(snapshot.state),
                connection_string(&snapshot.error_code, snapshot.error_code_len)
            );
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: state={}, errorCode={}",
                state_name(snapshot.state),
                connection_string(&snapshot.error_code, snapshot.error_code_len)
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_ready_without_prompt(id: u64, label: &str) -> ConnectionSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        if snapshot.state == ConnectionState::Ready as u32 {
            return snapshot;
        }
        if snapshot.state == ConnectionState::HostKeyPending as u32 {
            panic!("{label} unexpectedly requested host-key confirmation");
        }
        if snapshot.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: state={}, errorCode={}",
                state_name(snapshot.state),
                connection_string(&snapshot.error_code, snapshot.error_code_len)
            );
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: state={}, errorCode={}",
                state_name(snapshot.state),
                connection_string(&snapshot.error_code, snapshot.error_code_len)
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_text(id: u64, expected: &str, label: &str) -> DecodedSnapshot {
    wait_for_snapshot(id, label, |snapshot| {
        snapshot_text(snapshot).contains(expected)
    })
}

fn wait_for_text_with_timeout(
    id: u64,
    expected: &str,
    label: &str,
    timeout: Duration,
) -> DecodedSnapshot {
    wait_for_snapshot_with_timeout(id, label, timeout, |snapshot| {
        snapshot_text(snapshot).contains(expected)
    })
}

fn wait_for_snapshot<F>(id: u64, label: &str, predicate: F) -> DecodedSnapshot
where
    F: FnMut(&DecodedSnapshot) -> bool,
{
    wait_for_snapshot_with_timeout(id, label, WAIT_TIMEOUT, predicate)
}

fn wait_for_snapshot_with_timeout<F>(
    id: u64,
    label: &str,
    timeout: Duration,
    mut predicate: F,
) -> DecodedSnapshot
where
    F: FnMut(&DecodedSnapshot) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = read_snapshot(id);
        if predicate(&snapshot) {
            return snapshot;
        }
        let connection = connection_snapshot(id).expect("connection snapshot");
        if connection.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: state={}, errorCode={}",
                state_name(connection.state),
                connection_string(&connection.error_code, connection.error_code_len)
            );
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: state={}, errorCode={}",
                state_name(connection.state),
                connection_string(&connection.error_code, connection.error_code_len)
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn resize_and_check_remote_size(id: u64, columns: u16, rows: u16, expected: &str) {
    assert_eq!(meeterm_resize_terminal(id, columns, rows), 0);
    let native_dimensions = wait_for_snapshot(id, "native resize", |snapshot| {
        snapshot.columns == u32::from(columns) && snapshot.rows == u32::from(rows)
    });
    assert_eq!(native_dimensions.columns, u32::from(columns));
    assert_eq!(native_dimensions.rows, u32::from(rows));

    // Give the transport select loop a chance to issue window_change before
    // the remote shell evaluates stty.  The command itself remains bounded.
    send_line(id, "sleep 0.2; stty size");
    wait_for_text(id, expected, "remote PTY resize");
}

fn printf_octal(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect()
}

fn state_name(state: u32) -> &'static str {
    match state {
        value if value == ConnectionState::Disconnected as u32 => "Disconnected",
        value if value == ConnectionState::Connecting as u32 => "Connecting",
        value if value == ConnectionState::HostKeyPending as u32 => "HostKeyPending",
        value if value == ConnectionState::Authenticating as u32 => "Authenticating",
        value if value == ConnectionState::OpeningPty as u32 => "OpeningPty",
        value if value == ConnectionState::Ready as u32 => "Ready",
        value if value == ConnectionState::Closing as u32 => "Closing",
        value if value == ConnectionState::Failed as u32 => "Failed",
        _ => "Unknown",
    }
}

fn connection_string(bytes: &[u8], length: u16) -> String {
    let length = usize::from(length);
    assert!(length <= bytes.len());
    std::str::from_utf8(&bytes[..length])
        .expect("connection snapshot field must be UTF-8")
        .to_owned()
}

fn read_snapshot(id: u64) -> DecodedSnapshot {
    let mut capacity = meeterm_snapshot_size(id);
    assert!(
        capacity >= SNAPSHOT_HEADER_SIZE,
        "native snapshot is unavailable"
    );
    for _ in 0..8 {
        let mut bytes = vec![0_u8; capacity];
        // SAFETY: the vector has exactly the capacity returned by the native
        // size call and remains writable for this copy.
        let copied = unsafe { meeterm_snapshot(id, bytes.as_mut_ptr(), bytes.len()) };
        if copied > 0 && copied <= bytes.len() {
            bytes.truncate(copied);
            return decode_snapshot(&bytes);
        }
        capacity = copied.max(meeterm_snapshot_size(id));
        assert!(
            capacity >= SNAPSHOT_HEADER_SIZE,
            "native snapshot disappeared"
        );
    }
    panic!("native snapshot changed too quickly to copy");
}

fn decode_snapshot(bytes: &[u8]) -> DecodedSnapshot {
    assert!(bytes.len() >= SNAPSHOT_HEADER_SIZE);
    assert_eq!(&bytes[..4], b"MTRM");
    assert_eq!(u16_at(bytes, 4), 1, "unsupported snapshot version");
    assert_eq!(usize::from(u16_at(bytes, 6)), SNAPSHOT_HEADER_SIZE);
    let columns = u32_at(bytes, 8);
    let rows = u32_at(bytes, 12);
    assert!(columns > 0 && rows > 0);
    let cell_count = usize::try_from(u32_at(bytes, 24)).expect("cell count fits usize");
    assert!(cell_count <= columns as usize * rows as usize);

    let mut offset = SNAPSHOT_HEADER_SIZE;
    let mut cells = Vec::with_capacity(cell_count);
    for _ in 0..cell_count {
        let metadata_end = offset
            .checked_add(SNAPSHOT_CELL_METADATA_SIZE)
            .expect("snapshot metadata offset overflow");
        assert!(metadata_end <= bytes.len());
        let row = u32_at(bytes, offset);
        let column = u32_at(bytes, offset + 4);
        let width = bytes[offset + 8];
        assert!(width == 1 || width == 2);
        assert_eq!(bytes[offset + 9], 0);
        let flags = u16_at(bytes, offset + 10);
        let foreground: [u8; 4] = bytes[offset + 12..offset + 16]
            .try_into()
            .expect("foreground has four bytes");
        let base_len = usize::try_from(u32_at(bytes, offset + 20)).expect("base length fits usize");
        let combining_len =
            usize::try_from(u32_at(bytes, offset + 24)).expect("combining length fits usize");
        let base_start = metadata_end;
        let base_end = base_start
            .checked_add(base_len)
            .expect("snapshot base offset overflow");
        let combining_end = base_end
            .checked_add(combining_len)
            .expect("snapshot combining offset overflow");
        assert!(combining_end <= bytes.len());
        let base = std::str::from_utf8(&bytes[base_start..base_end])
            .expect("snapshot base must be UTF-8")
            .to_owned();
        let combining = std::str::from_utf8(&bytes[base_end..combining_end])
            .expect("snapshot combining text must be UTF-8")
            .to_owned();
        cells.push(DecodedCell {
            row,
            column,
            width,
            flags,
            foreground,
            base,
            combining,
        });
        offset = combining_end;
    }
    assert_eq!(offset, bytes.len());
    DecodedSnapshot {
        columns,
        rows,
        cells,
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    let end = offset.checked_add(2).expect("snapshot offset overflow");
    assert!(end <= bytes.len());
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    let end = offset.checked_add(4).expect("snapshot offset overflow");
    assert!(end <= bytes.len());
    u32::from_le_bytes(bytes[offset..end].try_into().expect("u32 has four bytes"))
}

fn snapshot_text(snapshot: &DecodedSnapshot) -> String {
    let mut rows: Vec<Vec<&DecodedCell>> = (0..snapshot.rows).map(|_| Vec::new()).collect();
    for cell in &snapshot.cells {
        if let Some(row) = rows.get_mut(cell.row as usize) {
            row.push(cell);
        }
    }

    let mut text = String::new();
    for (row_index, row) in rows.iter_mut().enumerate() {
        row.sort_by_key(|cell| cell.column);
        let mut next_column = 0_u32;
        for cell in row {
            while next_column < cell.column {
                text.push(' ');
                next_column += 1;
            }
            text.push_str(&cell.base);
            text.push_str(&cell.combining);
            next_column = cell.column + u32::from(cell.width);
        }
        if row_index + 1 < snapshot.rows as usize {
            text.push('\n');
        }
    }
    text
}

fn has_red_bold_marker(snapshot: &DecodedSnapshot, marker: &str) -> bool {
    let cells: HashMap<(u32, u32), &DecodedCell> = snapshot
        .cells
        .iter()
        .map(|cell| ((cell.row, cell.column), cell))
        .collect();
    let expected: Vec<char> = marker.chars().collect();
    let red = [205, 0, 0, 255];

    for start in &snapshot.cells {
        if start.base != expected.first().copied().unwrap_or_default().to_string() {
            continue;
        }
        let mut row = start.row;
        let mut column = start.column;
        let mut styled = true;
        for character in &expected {
            let Some(cell) = cells.get(&(row, column)) else {
                styled = false;
                break;
            };
            if cell.base != character.to_string()
                || cell.foreground != red
                || cell.flags & BOLD_FLAG == 0
            {
                styled = false;
                break;
            }
            column += u32::from(cell.width);
            if column >= snapshot.columns {
                row += column / snapshot.columns;
                column %= snapshot.columns;
            }
        }
        if styled {
            return true;
        }
    }
    false
}
