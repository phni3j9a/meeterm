//! Bounded real-OpenSSH coverage for the native SSH and terminal boundary.
//!
//! The test is ignored by default because it needs the disposable server from
//! `scripts/ssh/fixture.py`.  It intentionally uses only the public Rust API
//! and the native snapshot/input functions so it remains close to the mobile
//! integration boundary.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use meeterm_core::{
    ConnectOptions, ConnectionSnapshot, ConnectionState, PaneSnapshot, SessionSnapshot, SpecialKey,
    connect_terminal, connection_snapshot, create_terminal, destroy_terminal, disconnect_terminal,
    meeterm_commit_utf8, meeterm_input_commit_count, meeterm_resize_terminal,
    meeterm_respond_host_key, meeterm_send_special_key, meeterm_snapshot, meeterm_snapshot_size,
    reconnect_terminal, select_pane, send_bytes, session_snapshot,
};

const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SNAPSHOT_HEADER_SIZE: usize = 28;
const SNAPSHOT_CELL_METADATA_SIZE: usize = 28;
const BOLD_FLAG: u16 = 0b10;

const SYNC_MARKER: &str = "MEETERM_TMUX_SYNC_6B39";
const ANSI_MARKER: &str = "MEETERM_TMUX_ANSI_4D12";
const LS_DONE_MARKER: &str = "MEETERM_TMUX_LS_DONE_7E20";
const JAPANESE_DONE_MARKER: &str = "MEETERM_TMUX_JA_DONE_91AC";
const MAIN_PANE_MARKER: &str = "MEETERM_TMUX_MAIN_PANE_0A11";
const SIDE_PANE_MARKER: &str = "MEETERM_TMUX_SIDE_PANE_0A12";
const DURABLE_MARKER: &str = "MEETERM_TMUX_DURABLE_0A13";
const FULLSCREEN_MARKER: &str = "MEETERM_TMUX_FULLSCREEN_0A15";
const JAPANESE_TEXT: &str = "日本語";

struct FixtureConfig {
    host: String,
    port: u16,
    username: String,
    private_key: String,
    passphrase: String,
    fingerprint: String,
    known_hosts: PathBuf,
    unencrypted_key: PathBuf,
    alternate_host_key: PathBuf,
    tmux_tmpdir: PathBuf,
    tmux_socket: PathBuf,
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
fn real_openssh_tmux_session_loop() {
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
    respond_to_host_key_ffi(id, &fixture.fingerprint, true);
    let ready = wait_for_state(id, ConnectionState::Ready, "encrypted-key authentication");
    assert_eq!(
        connection_string(&ready.algorithm, ready.algorithm_len),
        "ssh-ed25519"
    );

    // Control Mode creates the durable session and the first pane.  The
    // fixture's sshd SetEnv points every shell at the private ordinary tmux
    // socket, so these extra windows/panes cannot touch a developer's own
    // `tmux -t meeterm` session even when the test itself runs inside tmux.
    let initial = wait_for_session(id, 1, "initial meeterm session");
    assert_eq!(
        initial.windows.len(),
        1,
        "a fresh fixture starts one window"
    );
    let initial_pane = initial.panes.first().expect("initial pane").clone();
    assert_ne!(initial_pane.terminal_id, 0);
    let socket = wait_for_remote_tmux(
        &fixture,
        "tmux display-message -p -t '=meeterm' '#{socket_path}'",
        "fixture tmux socket",
        |output| !output.trim().is_empty(),
    );
    assert!(fixture.tmux_socket.starts_with(&fixture.tmux_tmpdir));
    assert_eq!(socket.trim(), fixture.tmux_socket.to_string_lossy());

    run_remote_tmux(
        &fixture,
        "tmux set-hook -t '=meeterm:' 'client-detached[77]' 'display-message user-hook-preserved'; tmux set-hook -t '=meeterm:' 'client-session-changed[77]' 'display-message user-hook-preserved'",
        "install preexisting user hooks",
    );

    run_remote_tmux(
        &fixture,
        &format!(
            "tmux rename-window -t @{} main; \
             tmux split-window -h -t %{} 'exec /bin/sh -i'; \
             tmux new-window -t '=meeterm' -n side 'exec /bin/sh -i'; \
             tmux split-window -h -t '=meeterm:side' 'exec /bin/sh -i'; \
             tmux select-window -t @{}; \
             tmux select-pane -t %{}",
            initial_pane.window_id,
            initial_pane.pane_id,
            initial_pane.window_id,
            initial_pane.pane_id,
        ),
        "create fixture tmux topology",
    );
    let topology = wait_for_session(id, 4, "tmux topology synchronization");
    assert!(topology.windows.len() >= 2);
    assert_eq!(
        topology
            .panes
            .iter()
            .map(|pane| pane.pane_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        topology.panes.len(),
        "tmux pane IDs must be unique runtime identities"
    );
    assert!(topology.panes.iter().all(|pane| pane.terminal_id != 0));

    let main = topology
        .panes
        .iter()
        .find(|pane| pane.pane_id == initial_pane.pane_id)
        .expect("original pane survives topology changes")
        .clone();
    let side = topology
        .panes
        .iter()
        .find(|pane| pane.window_id != main.window_id)
        .expect("second window pane")
        .clone();
    let split = topology
        .panes
        .iter()
        .find(|pane| pane.window_id == main.window_id && pane.pane_id != main.pane_id)
        .expect("split pane")
        .clone();

    // Each pane gets a deterministic shell and its own native terminal. The
    // octal marker encoding keeps command echo from making an assertion pass.
    prepare_pane(&main, "main pane shell");
    prepare_pane(&side, "side pane shell");
    prepare_pane(&split, "split pane shell");
    wait_for_state(
        id,
        ConnectionState::Ready,
        "pane preparation synchronization",
    );

    let sync_command = format!("stty -echo; printf '{}\\n'", printf_octal(SYNC_MARKER));
    assert!(!sync_command.contains(SYNC_MARKER));
    send_line_retry(
        main.terminal_id,
        &sync_command,
        "main echo-disable synchronization",
    );
    wait_for_pane_text(&main, SYNC_MARKER, "main echo-disable synchronization");

    let ansi_command = format!(
        "printf '\\033[1;31m{}\\033[0m\\n'",
        printf_octal(ANSI_MARKER)
    );
    assert!(!ansi_command.contains(ANSI_MARKER));
    send_line_retry(main.terminal_id, &ansi_command, "ANSI styled output");
    wait_for_pane_snapshot(&main, "ANSI styled output", |snapshot| {
        has_red_bold_marker(snapshot, ANSI_MARKER)
    });

    let ls_command = format!("ls -d /tmp; printf '{}\\n'", printf_octal(LS_DONE_MARKER));
    assert!(!ls_command.contains(LS_DONE_MARKER));
    send_line_retry(main.terminal_id, &ls_command, "real ls command");
    let ls_snapshot = wait_for_pane_text(&main, LS_DONE_MARKER, "real ls output completion");
    // The command text contains `/tmp`; exactly one occurrence proves that
    // the value came from ls after echo was disabled.
    assert_eq!(snapshot_text(&ls_snapshot).matches("/tmp").count(), 1);

    // Send committed CJK text through the native UTF-8 path inside a shell
    // printf command. The pane's own input counter and output prove that the
    // commit was accepted once and routed to the correct remote PTY.
    send_line_retry(main.terminal_id, "export LC_ALL=C.UTF-8", "set C.UTF-8");
    send_raw_retry(main.terminal_id, b"printf \"", "start Japanese printf");
    let committed = JAPANESE_TEXT.as_bytes();
    // SAFETY: `committed` remains alive and contains valid UTF-8 for the
    // supplied length while the native function copies it into its queue.
    assert_eq!(commit_utf8_retry(main.terminal_id, committed), 1);
    assert_eq!(meeterm_input_commit_count(main.terminal_id), 1);
    let japanese_suffix = format!("\\n{}\\n\"", printf_octal(JAPANESE_DONE_MARKER));
    assert!(!japanese_suffix.contains(JAPANESE_TEXT));
    assert!(!japanese_suffix.contains(JAPANESE_DONE_MARKER));
    send_raw_retry(
        main.terminal_id,
        japanese_suffix.as_bytes(),
        "finish Japanese printf",
    );
    send_enter_retry(main.terminal_id, "Japanese printf Enter");
    let japanese_snapshot = wait_for_pane_text(
        &main,
        JAPANESE_DONE_MARKER,
        "committed Japanese output completion",
    );
    assert_eq!(
        snapshot_text(&japanese_snapshot)
            .matches(JAPANESE_TEXT)
            .count(),
        1
    );

    // Output is routed by pane ID, so markers written to one pane must not
    // appear in its siblings' Rust-owned terminal snapshots.
    send_line_retry(
        main.terminal_id,
        &format!("printf '{}\\n'", printf_octal(MAIN_PANE_MARKER)),
        "main pane marker",
    );
    let main_marker = wait_for_pane_text(&main, MAIN_PANE_MARKER, "main pane marker");
    assert!(!snapshot_text(&read_snapshot(side.terminal_id)).contains(MAIN_PANE_MARKER));
    send_line_retry(
        side.terminal_id,
        &format!("printf '{}\\n'", printf_octal(SIDE_PANE_MARKER)),
        "side pane marker",
    );
    let _side_marker = wait_for_pane_text(&side, SIDE_PANE_MARKER, "side pane marker");
    assert!(!snapshot_text(&main_marker).contains(SIDE_PANE_MARKER));
    assert!(!snapshot_text(&read_snapshot(split.terminal_id)).contains(SIDE_PANE_MARKER));

    // Selecting a mobile pane must preserve the window/pane model while
    // zooming the selected window. The remote query observes the real tmux
    // state, rather than trusting an optimistic local flag.
    select_pane(id, side.pane_id).expect("select side pane");
    wait_for_selected_pane(id, side.pane_id, "select side pane");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        "tmux zoom",
        |output| output.trim() == "1",
    );

    // Resize the selected pane through the native terminal API and verify the
    // dimensions reported by both Rust's terminal snapshot and tmux metadata.
    resize_and_check_pane(&side, 100, 30, "resize selected pane");
    resize_and_check_pane(&side, 60, 18, "resize selected pane back");

    // The ordinary desktop client is a second consumer of the same session.
    // It attaches using exactly `tmux attach -t meeterm` and detaches cleanly
    // with the standard Ctrl-b d sequence.
    ordinary_desktop_attach(&fixture);

    // An alternate-screen marker exercises the capture/resynchronization
    // boundary used by full-screen TUIs. The escape sequence is emitted by
    // the shell into the PTY: sending it as input would only feed readline
    // and would never switch the remote terminal's active screen.
    let fullscreen_command = format!(
        "printf '\\033[?1049h\\033[2J\\033[H\\033[?7l'; printf '{}\\n'",
        printf_octal(FULLSCREEN_MARKER)
    );
    assert!(!fullscreen_command.contains(FULLSCREEN_MARKER));
    send_line_retry(
        side.terminal_id,
        &fullscreen_command,
        "enter alternate screen",
    );
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t %{0} '#{{alternate_on}}'",
            side.pane_id
        ),
        "remote alternate screen",
        |output| output.trim() == "1",
    );
    wait_for_pane_text(&side, FULLSCREEN_MARKER, "full-screen marker");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t %{} '#{{wrap_flag}}'",
            side.pane_id
        ),
        "wrap disabled before reconnect",
        |output| output.trim() == "0",
    );

    let before_loss = session_snapshot(id).expect("session snapshot before transport loss");
    let before_ids = pane_identity_set(&before_loss);
    // Detaching the native Control Mode client from another ordinary SSH
    // client simulates an abrupt transport loss while leaving tmux alive.
    detach_control_mode_client(&fixture);
    wait_for_transport_loss(id, "abrupt transport loss");
    assert!(send_bytes(side.terminal_id, b"should be rejected").is_err());

    // tmux remains durable while SSH is gone. Inject a shell sentinel via a
    // separate ordinary SSH client before reconnecting the native owner.
    remote_send_keys(&fixture, side.pane_id, DURABLE_MARKER);
    reconnect_terminal(id).expect("start reconnect after transport loss");
    wait_for_ready_without_prompt(id, "pinned reconnect after transport loss");
    let after_loss = wait_for_session(id, before_loss.panes.len(), "resynchronized tmux topology");
    assert_eq!(pane_identity_set(&after_loss), before_ids);
    let reconnected_side = after_loss
        .panes
        .iter()
        .find(|pane| pane.pane_id == side.pane_id)
        .expect("side pane identity survives reconnect")
        .clone();
    assert_eq!(
        reconnected_side.terminal_id, side.terminal_id,
        "native terminal identity survives reconnect"
    );
    wait_for_pane_text(
        &reconnected_side,
        DURABLE_MARKER,
        "durable sentinel after reconnect",
    );
    wait_for_pane_text(
        &reconnected_side,
        FULLSCREEN_MARKER,
        "alternate-screen capture after reconnect",
    );

    // DECAWM remains disabled remotely across transport loss. Writing past
    // the right margin must overwrite the last cell instead of wrapping in
    // the reconstructed native Term. Move the prompt away from that row.
    let columns = read_snapshot(reconnected_side.terminal_id).columns;
    send_line_retry(
        reconnected_side.terminal_id,
        &format!("printf '\\033[10;{}HABCD\\033[11;1H'", columns - 1),
        "post-reconnect no-wrap output",
    );
    wait_for_pane_snapshot(&reconnected_side, "restored no-wrap mode", |snapshot| {
        snapshot
            .cells
            .iter()
            .any(|cell| cell.row == 9 && cell.column == columns - 2 && cell.base == "A")
            && snapshot
                .cells
                .iter()
                .any(|cell| cell.row == 9 && cell.column == columns - 1 && cell.base == "D")
    });

    // A graceful disconnect must clean up zoom state as well.  The remote
    // session and pane identities remain available for a later desktop handoff.
    select_pane(id, reconnected_side.pane_id).expect("reselect side pane");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            reconnected_side.window_id
        ),
        "zoom before graceful disconnect",
        |output| output.trim() == "1",
    );
    disconnect_terminal(id).expect("graceful native disconnect");
    wait_for_state(id, ConnectionState::Disconnected, "graceful disconnect");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            reconnected_side.window_id
        ),
        "zoom cleanup after graceful disconnect",
        |output| output.trim() == "0",
    );
    assert!(send_bytes(reconnected_side.terminal_id, b"input after disconnect").is_err());

    let hooks = run_remote_tmux(
        &fixture,
        "tmux show-hooks -t '=meeterm:'",
        "preserved user hooks",
    );
    let hooks = String::from_utf8_lossy(&hooks.stdout);
    assert!(hooks.contains("client-detached[77]"));
    assert!(hooks.contains("client-session-changed[77]"));
    assert!(
        !hooks.contains("[1000]"),
        "only meeterm's hook slots should be removed"
    );

    // External pane removal invalidates the borrowed native handle and must
    // not leave a dead zoom target that breaks the next mobile selection.
    reconnect_terminal(id).expect("reconnect before pane removal");
    wait_for_ready_without_prompt(id, "reconnect before pane removal");
    run_remote_tmux(
        &fixture,
        &format!("tmux kill-pane -t %{}", side.pane_id),
        "remove selected remote pane",
    );
    let remaining = wait_for_session(id, 3, "removed pane topology");
    assert!(
        !remaining
            .panes
            .iter()
            .any(|pane| pane.pane_id == side.pane_id)
    );
    assert_eq!(
        meeterm_snapshot_size(side.terminal_id),
        0,
        "removed borrowed handle is invalid"
    );
    select_pane(id, main.pane_id).expect("select surviving pane after removal");
    send_line_retry(
        main.terminal_id,
        &format!("printf '{}\\n'", printf_octal("MEETERM_AFTER_REMOVE")),
        "surviving pane input",
    );
    wait_for_pane_text(&main, "MEETERM_AFTER_REMOVE", "surviving pane output");
    disconnect_terminal(id).expect("disconnect surviving session");

    // Once the key is pinned, a wrong passphrase fails before tmux is opened.
    let wrong_id = create_terminal(80, 24).expect("create wrong-passphrase terminal");
    let _wrong_guard = TerminalGuard { id: wrong_id };
    let mut wrong_options = fixture.options();
    wrong_options.passphrase = Some("definitely-wrong-passphrase".to_owned());
    connect_terminal(wrong_id, wrong_options).expect("start wrong-passphrase connection");
    let wrong = wait_for_state(
        wrong_id,
        ConnectionState::Failed,
        "wrong-passphrase rejection",
    );
    assert_eq!(
        connection_string(&wrong.error_code, wrong.error_code_len),
        "key_file"
    );

    // Replace only this fixture's trust record with an unrelated valid key.
    // The live server still presents the expected key, so the native client
    // must report host_key_changed and refuse to continue.
    let changed_id = create_terminal(80, 24).expect("create changed-key terminal");
    let _changed_guard = TerminalGuard { id: changed_id };
    write_alternate_trust_record(&fixture);
    connect_terminal(changed_id, fixture.options()).expect("start changed-key connection");
    let changed = wait_for_state(
        changed_id,
        ConnectionState::Failed,
        "changed host-key rejection",
    );
    assert_eq!(
        connection_string(&changed.error_code, changed.error_code_len),
        "host_key_changed"
    );
    assert_eq!(
        connection_string(&changed.fingerprint, changed.fingerprint_len),
        fixture.fingerprint
    );
    assert_ne!(
        connection_string(&changed.known_fingerprint, changed.known_fingerprint_len),
        fixture.fingerprint
    );
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
            unencrypted_key: PathBuf::from(value("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")),
            alternate_host_key: PathBuf::from(value("MEETERM_SSH_ALTERNATE_HOST_KEY_FILE")),
            tmux_tmpdir: PathBuf::from(value("MEETERM_TMUX_TMPDIR")),
            tmux_socket: PathBuf::from(value("MEETERM_TMUX_SOCKET")),
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

fn respond_to_host_key_ffi(id: u64, fingerprint: &str, accept: bool) {
    let result = unsafe {
        meeterm_respond_host_key(
            id,
            fingerprint.as_ptr(),
            fingerprint.len(),
            u8::from(accept),
        )
    };
    assert_eq!(
        result, 0,
        "host-key response should be accepted by native core"
    );
}

fn ssh_command(fixture: &FixtureConfig, allocate_tty: bool) -> Command {
    let mut command = Command::new("ssh");
    let destination = format!("{}@{}", fixture.username, fixture.host);
    let port = fixture.port.to_string();
    let known_hosts = format!("UserKnownHostsFile={}", fixture.known_hosts.display());
    if allocate_tty {
        command.arg("-tt").env("TERM", "xterm-256color");
    }
    command
        .args([
            "-F",
            "/dev/null",
            "-p",
            port.as_str(),
            "-i",
            fixture.unencrypted_key.to_str().expect("UTF-8 key path"),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "BatchMode=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            known_hosts.as_str(),
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "LogLevel=ERROR",
        ])
        .arg(destination)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("TMUX_TMPDIR", &fixture.tmux_tmpdir);
    command
}

fn run_remote_tmux(fixture: &FixtureConfig, command: &str, label: &str) -> Output {
    let output = ssh_command(fixture, false)
        .arg(command)
        .output()
        .unwrap_or_else(|error| panic!("{label}: start ssh: {error}"));
    assert!(
        output.status.success(),
        "{label}: ssh exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn wait_for_remote_tmux<F>(
    fixture: &FixtureConfig,
    command: &str,
    label: &str,
    mut predicate: F,
) -> String
where
    F: FnMut(&str) -> bool,
{
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut last_output: String;
    loop {
        let output = ssh_command(fixture, false)
            .arg(command)
            .output()
            .unwrap_or_else(|error| panic!("{label}: start ssh: {error}"));
        last_output = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() && predicate(last_output.trim()) {
            return last_output;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: status={}, stdout={last_output:?}, stderr={:?}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn ordinary_desktop_attach(fixture: &FixtureConfig) {
    let mut child = ssh_command(fixture, true)
        .arg("tmux attach-session -t meeterm")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start ordinary desktop tmux attach");
    sleep(Duration::from_millis(250));
    let mut input = child.stdin.take().expect("desktop attach stdin");
    input
        .write_all(b"\x02d")
        .expect("send ordinary tmux detach keys");
    drop(input);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("wait for desktop tmux attach") {
            assert!(status.success(), "ordinary tmux attach failed: {status}");
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("ordinary tmux attach did not detach with Ctrl-b d");
        }
        sleep(POLL_INTERVAL);
    }
}

fn prepare_pane(pane: &PaneSnapshot, label: &str) {
    send_line_retry(pane.terminal_id, "exec /bin/sh -i", label);
    sleep(Duration::from_millis(100));
}

fn send_raw_retry(id: u64, bytes: &[u8], label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        match send_bytes(id, bytes) {
            Ok(length) if length == bytes.len() => return,
            Ok(length) => panic!("{label}: accepted {length}/{} bytes", bytes.len()),
            Err(_error) if Instant::now() < deadline => sleep(POLL_INTERVAL),
            Err(error) => {
                let connection = connection_snapshot(id).ok();
                let state = connection
                    .as_ref()
                    .map(|snapshot| state_name(snapshot.state))
                    .unwrap_or("unknown");
                panic!("{label}: native input rejected: {error} (connection state={state})");
            }
        }
    }
}

fn send_enter_retry(id: u64, label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        match meeterm_send_special_key(id, SpecialKey::Enter as u32) {
            1 => return,
            _ if Instant::now() < deadline => sleep(POLL_INTERVAL),
            result => panic!("{label}: native Enter rejected with {result}"),
        }
    }
}

fn send_line_retry(id: u64, line: &str, label: &str) {
    send_raw_retry(id, line.as_bytes(), label);
    send_enter_retry(id, label);
}

fn commit_utf8_retry(id: u64, bytes: &[u8]) -> u64 {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        // SAFETY: `bytes` remains alive and contains valid UTF-8 for the
        // supplied length while the native function copies it into its queue.
        let result = unsafe { meeterm_commit_utf8(id, bytes.as_ptr(), bytes.len()) };
        if result != 0 {
            return result;
        }
        if Instant::now() >= deadline {
            panic!("native UTF-8 commit was not accepted");
        }
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_session(id: u64, expected_panes: usize, label: &str) -> SessionSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = session_snapshot(id).expect("tmux session snapshot");
        if snapshot.panes.len() == expected_panes && !snapshot.windows.is_empty() {
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
                "timed out waiting for {label}: state={}, panes={}, windows={}",
                state_name(connection.state),
                snapshot.panes.len(),
                snapshot.windows.len()
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_selected_pane(id: u64, pane_id: u64, label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = session_snapshot(id).expect("tmux session snapshot");
        if snapshot.selected_pane == Some(pane_id)
            && snapshot
                .panes
                .iter()
                .any(|pane| pane.pane_id == pane_id && pane.selected)
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: selected={:?}, wanted=%{pane_id}",
                snapshot.selected_pane
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn pane_identity_set(snapshot: &SessionSnapshot) -> std::collections::HashSet<(u64, u64)> {
    snapshot
        .panes
        .iter()
        .map(|pane| (pane.window_id, pane.pane_id))
        .collect()
}

fn wait_for_pane_snapshot<F>(pane: &PaneSnapshot, label: &str, mut predicate: F) -> DecodedSnapshot
where
    F: FnMut(&DecodedSnapshot) -> bool,
{
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = read_snapshot(pane.terminal_id);
        if predicate(&snapshot) {
            return snapshot;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {label}");
        }
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_pane_text(pane: &PaneSnapshot, expected: &str, label: &str) -> DecodedSnapshot {
    wait_for_pane_snapshot(pane, label, |snapshot| {
        snapshot_text(snapshot).contains(expected)
    })
}

fn resize_and_check_pane(pane: &PaneSnapshot, columns: u16, rows: u16, label: &str) {
    assert_eq!(meeterm_resize_terminal(pane.terminal_id, columns, rows), 0);
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = read_snapshot(pane.terminal_id);
        if snapshot.columns == u32::from(columns) && snapshot.rows == u32::from(rows) {
            break;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: got {}x{}, wanted {}x{}",
                snapshot.columns, snapshot.rows, columns, rows
            );
        }
        sleep(POLL_INTERVAL);
    }
    let marker = format!("MEETERM_TMUX_RESIZE_{columns}_{rows}");
    send_line_retry(
        pane.terminal_id,
        &format!(
            "sleep 0.2; stty size; printf '{}\\n'",
            printf_octal(&marker)
        ),
        label,
    );
    let snapshot = wait_for_pane_text(pane, &marker, label);
    assert!(
        snapshot_text(&snapshot).contains(&format!("{rows} {columns}")),
        "{label}: remote stty did not report {rows} {columns}: {}",
        snapshot_text(&snapshot)
    );
}

fn remote_send_keys(fixture: &FixtureConfig, pane_id: u64, marker: &str) {
    let command = format!(
        "tmux send-keys -t %{pane_id} -l {}; tmux send-keys -t %{pane_id} Enter",
        shell_quote(&format!("printf '{}\\n'", printf_octal(marker)))
    );
    run_remote_tmux(fixture, &command, "inject durable tmux sentinel");
}

fn detach_control_mode_client(fixture: &FixtureConfig) {
    let clients = run_remote_tmux(
        fixture,
        "tmux list-clients -F '#{client_name}|#{client_control_mode}'",
        "list tmux clients before transport loss",
    );
    let client = String::from_utf8_lossy(&clients.stdout)
        .lines()
        .filter_map(|line| line.split_once('|'))
        .find_map(|(name, control_mode)| {
            (control_mode.trim() == "1").then(|| name.trim().to_owned())
        })
        .expect("native Control Mode client must be present before transport loss");
    run_remote_tmux(
        fixture,
        &format!("tmux detach-client -t {}", shell_quote(&client)),
        "abrupt tmux client detach",
    );
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn write_alternate_trust_record(fixture: &FixtureConfig) {
    let alternate = fs::read_to_string(&fixture.alternate_host_key)
        .expect("alternate fixture host key must be readable");
    let fields = alternate.split_whitespace().collect::<Vec<_>>();
    assert!(
        fields.len() >= 2,
        "alternate host key has invalid OpenSSH form"
    );
    let record = format!(
        "[{}]:{} {} {}\n",
        fixture.host, fixture.port, fields[0], fields[1]
    );
    fs::write(&fixture.known_hosts, record).expect("replace fixture trust record");
}

fn wait_for_transport_loss(id: u64, label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        match snapshot.state {
            value if value == ConnectionState::Disconnected as u32 => return,
            value if value == ConnectionState::Failed as u32 => {
                let code = connection_string(&snapshot.error_code, snapshot.error_code_len);
                assert!(
                    code == "transport" || code == "remote_closed",
                    "{label} failed with unexpected error code {code}"
                );
                return;
            }
            _ if Instant::now() < deadline => sleep(POLL_INTERVAL),
            _ => panic!(
                "timed out waiting for {label}: state={}",
                state_name(snapshot.state)
            ),
        }
    }
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
        value if value == ConnectionState::AttachingTmux as u32 => "AttachingTmux",
        value if value == ConnectionState::Synchronizing as u32 => "Synchronizing",
        value if value == ConnectionState::Reconnecting as u32 => "Reconnecting",
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
