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
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use meeterm_core::workspace::{
    Backend, RuntimeDiscoverySnapshot, RuntimeSectionState, RuntimeState,
};
use meeterm_core::{
    ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED, ATTACHMENT_FLAG_REMOTE_REMOVED, AttachmentError,
    AttachmentPhase, AttachmentSnapshot, AuthOptions, ConnectOptions, ConnectionSnapshot,
    ConnectionState, MAX_ATTACHMENT_BYTES, PaneSnapshot, SessionSnapshot, SpecialKey,
    attachment_begin, attachment_cancel, attachment_delete_remote, attachment_dispose,
    attachment_insert, attachment_snapshot, close_pane, close_workspace, connect_host,
    connection_snapshot, create_pane, create_runtime, create_terminal, create_workspace,
    destroy_terminal, disconnect_terminal, meeterm_commit_utf8, meeterm_input_commit_count,
    meeterm_resize_terminal, meeterm_respond_host_key, meeterm_send_special_key, meeterm_snapshot,
    meeterm_snapshot_size, reconnect_terminal, refresh_terminal, rename_pane, rename_workspace,
    runtime_discovery_snapshot, select_pane, select_runtime, send_bytes, session_snapshot,
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
const TUI_MARKER: &str = "MEETERM_TMUX_TUI_REDRAW_0A16";
const TUI_INPUT_MARKER: &str = "MEETERM_TMUX_TUI_INPUT_0A17";
const TUI_COLD_MARKER: &str = "MEETERM_TMUX_TUI_COLD_0A18";
const TUI_COLD_INPUT_MARKER: &str = "MEETERM_TMUX_TUI_COLD_INPUT_0A19";
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteWindowLayout {
    window_id: u64,
    saved_layout: String,
    shape: LayoutShape,
    width: u16,
    height: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemotePaneState {
    window_id: u64,
    pane_id: u64,
    index: u32,
    pid: u32,
    active: bool,
    window_active: bool,
    zoomed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteTmuxLayout {
    windows: Vec<RemoteWindowLayout>,
    panes: Vec<RemotePaneState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LayoutShape {
    Leaf(u64),
    Split {
        direction: LayoutSplitDirection,
        children: Vec<LayoutShape>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LayoutSplitDirection {
    Horizontal,
    Vertical,
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py to provide a real local sshd"]
fn real_openssh_existing_tmux_runtime_selection() {
    let fixture = FixtureConfig::from_environment();
    create_fixture_tmux_session(&fixture, "meeterm");
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host_and_select_meeterm(id, &fixture, "existing tmux runtime selection");
    let session = wait_for_session(id, 1, "existing tmux runtime ready");
    assert_eq!(session.windows.len(), 1);
    assert_eq!(session.panes.len(), 1);
}

/// Drive the fixture-owned sshd stop/start boundary: killing the SSH server
/// also kills the remote `tmux -C` client, so `client-detached` fires on the
/// durable server exactly like the mobile transport-loss smoke. This is the
/// hard-loss variant of `detach_control_mode_client`.
fn fixture_control_action(action: &str) {
    let request_path = PathBuf::from(value("MEETERM_SSH_FIXTURE_CONTROL_REQUEST"));
    let status_path = PathBuf::from(value("MEETERM_SSH_FIXTURE_CONTROL_STATUS"));
    let token = format!("transport-loss-rust-{}-{}", std::process::id(), action);
    let expected = format!(
        "{token}\tok\t{}",
        if action == "stop" {
            "stopped"
        } else {
            "started"
        }
    );
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        use std::os::unix::fs::OpenOptionsExt;
        let request = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&request_path);
        match request {
            Ok(mut file) => {
                file.write_all(format!("{token}\t{action}\n").as_bytes())
                    .expect("write fixture control request");
                let _ = file.sync_all();
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                assert!(
                    Instant::now() < deadline,
                    "fixture control request stayed busy"
                );
                sleep(POLL_INTERVAL);
            }
            Err(error) => panic!("fixture control request failed: {error}"),
        }
    }
    loop {
        if let Ok(contents) = fs::read_to_string(&status_path) {
            let fields: Vec<&str> = contents.trim_end_matches('\n').split('\t').collect();
            if fields.first().copied() == Some(token.as_str()) {
                assert_eq!(
                    fields.join("\t"),
                    expected,
                    "fixture control action {action} failed"
                );
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture control action {action} timed out"
        );
        sleep(POLL_INTERVAL);
    }
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py to provide a real local sshd"]
fn real_openssh_tmux_transport_loss_sshd_restart() {
    let fixture = FixtureConfig::from_environment();
    create_fixture_tmux_session(&fixture, "meeterm");
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host_and_select_meeterm(id, &fixture, "sshd-restart recovery selection");
    // A third-party indexed hook on the same reserved name must survive the
    // whole loss/recovery cycle; meeterm only ever removes its own slot.
    run_remote_tmux(
        &fixture,
        "tmux set-hook -t '=meeterm:' 'client-detached[7]' 'display-message third-party'",
        "install third-party client-detached hook",
    );
    let initial = wait_for_session(id, 1, "initial meeterm session");
    let initial_pane = initial.panes.first().expect("initial pane").clone();
    run_remote_tmux(
        &fixture,
        &format!(
            "tmux split-window -h -t %{} 'exec /bin/sh -i'",
            initial_pane.pane_id,
        ),
        "split fixture pane",
    );
    let topology = wait_for_session(id, 2, "split topology synchronization");
    let side = topology
        .panes
        .iter()
        .find(|pane| {
            pane.pane_id != initial_pane.pane_id && pane.window_id == initial_pane.window_id
        })
        .expect("split pane in selected window")
        .clone();

    select_pane(id, side.pane_id).expect("select split pane");
    wait_for_selected_pane(id, side.pane_id, "select split pane");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        "meeterm zoom before transport loss",
        |output| output.trim() == "1",
    );

    // Killing sshd terminates the remote `tmux -C` process; the durable tmux
    // server then fires client-detached, so the indexed recovery hooks run
    // before the replacement actor even connects.
    fixture_control_action("stop");
    wait_for_reconnecting(id, "sshd-stop transport loss");
    fixture_control_action("start");
    let recovered = wait_for_ready_without_prompt(id, "authoritative Ready after sshd restart");
    assert_eq!(
        connection_string(&recovered.error_code, recovered.error_code_len),
        "",
        "recovered connection must not carry a failure code"
    );
    wait_for_selected_pane(id, side.pane_id, "same pane after sshd restart");
    wait_for_remote_tmux(
        &fixture,
        "tmux show-hooks -t '=meeterm:' | grep -F 'client-detached[7] display-message third-party'",
        "third-party hook survives sshd restart recovery",
        |output| !output.is_empty(),
    );
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py to provide a real local sshd"]
fn real_openssh_tmux_session_loop() {
    let fixture = FixtureConfig::from_environment();
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host(id, fixture.options()).expect("start SSH host connection");
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
    let discovery = wait_for_runtime_picker_without_prompt(id, "initial runtime discovery");
    let ready = create_tmux_meeterm_from_picker(
        id,
        &discovery,
        "initial explicit meeterm runtime creation",
    );
    assert_eq!(
        connection_string(&ready.algorithm, ready.algorithm_len),
        "ssh-ed25519"
    );

    // The explicit detached create above establishes the durable session;
    // Control Mode now attaches to it and opens the first pane. The fixture's
    // sshd SetEnv points every shell at the private ordinary tmux socket, so
    // these extra windows/panes cannot touch a developer's own `tmux -t
    // meeterm` session even when the test itself runs inside tmux.
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
    let side_active = topology
        .panes
        .iter()
        .find(|pane| pane.window_id == side.window_id && pane.active)
        .expect("active pane in second window")
        .clone();
    assert_ne!(
        side_active.pane_id, side.pane_id,
        "the non-selected window must exercise a non-first active pane"
    );
    assert!(!side_active.selected);
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
    wait_for_state(id, ConnectionState::Ready, "ready before keyboard resize");
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                assert_eq!(
                    connection_snapshot(id)
                        .expect("connection during resize")
                        .state,
                    ConnectionState::Ready as u32,
                    "routine resize must not tell the UI to unmount its live terminal"
                );
                sleep(Duration::from_millis(1));
            }
        });
        resize_and_check_pane(&side, 100, 30, "resize selected pane");
        resize_and_check_pane(&side, 60, 18, "resize selected pane back");
        observer.join().expect("continuous readiness during resize");
    });

    // The phone's native Ctrl-C key must interrupt a real foreground process,
    // rather than print a label or send the literal characters '^C'.
    send_line_retry(side.terminal_id, "sleep 30", "start interruptible process");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t %{} '#{{pane_current_command}}'",
            side.pane_id
        ),
        "foreground sleep starts",
        |output| output.trim() == "sleep",
    );
    let interrupt_started = Instant::now();
    assert_eq!(
        meeterm_send_special_key(side.terminal_id, SpecialKey::Interrupt as u32),
        1
    );
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t %{} '#{{pane_current_command}}'",
            side.pane_id
        ),
        "foreground sleep interrupted",
        |output| output.trim() != "sleep",
    );
    assert!(interrupt_started.elapsed() < Duration::from_secs(10));

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

    // Exercise a real alternate-screen application when the fixture image
    // provides one. The explicit redraw command must reconstruct the current
    // TUI screen in the native terminal without routing cells through JS.
    let vim_available = ssh_command(&fixture, false)
        .arg("command -v vim")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        vim_available,
        "the OpenSSH fixture must provide vim for the full-screen TUI recovery boundary"
    );
    let tui = ["nvim", "vim"]
        .into_iter()
        .find(|program| {
            ssh_command(&fixture, false)
                .arg(format!("command -v {program}"))
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("vim must be available after the fixture boundary check");
    {
        send_line_retry(
            side.terminal_id,
            "printf '\\033[?1049l'",
            "leave shell alternate screen before TUI",
        );
        wait_for_remote_tmux(
            &fixture,
            &format!(
                "tmux display-message -p -t %{} '#{{alternate_on}}'",
                side.pane_id
            ),
            "leave shell alternate screen before TUI",
            |output| output.trim() == "0",
        );
        let launch = match tui {
            "nvim" => "nvim -u NONE -N",
            "vim" => "vim -Nu NONE -n",
            _ => unreachable!("only nvim or vim can reach the TUI fixture"),
        };
        send_line_retry(side.terminal_id, launch, "launch real full-screen TUI");
        wait_for_remote_tmux(
            &fixture,
            &format!(
                "tmux display-message -p -t %{} '#{{alternate_on}}'",
                side.pane_id
            ),
            "real full-screen TUI enters alternate screen",
            |output| output.trim() == "1",
        );
        // Leave Vim in insert mode while the native transport is interrupted.
        // Reconnect must restore the alternate-screen capture and keep
        // accepting the already active TUI input mode; the second marker is
        // intentionally sent without another `i`.
        send_raw_retry(
            side.terminal_id,
            format!("i{TUI_MARKER}").as_bytes(),
            "vim TUI marker",
        );
        refresh_terminal(id).expect("request native TUI redraw");
        wait_for_pane_text(&side, TUI_MARKER, "native TUI redraw marker");
        detach_control_mode_client(&fixture);
        wait_for_reconnecting(id, "TUI transport loss");
        wait_for_ready_without_prompt(id, "TUI automatic reconnect");
        refresh_terminal(id).expect("request TUI redraw after reconnect");
        wait_for_pane_text(&side, TUI_MARKER, "TUI marker after reconnect");
        send_raw_retry(
            side.terminal_id,
            format!("{TUI_INPUT_MARKER}\x1b").as_bytes(),
            "recovered Vim insert mode",
        );
        wait_for_pane_text(&side, TUI_INPUT_MARKER, "recovered Vim input mode marker");
        send_raw_retry(side.terminal_id, b":q!\r", "exit vim TUI");
        wait_for_remote_tmux(
            &fixture,
            &format!(
                "tmux display-message -p -t %{} '#{{alternate_on}}'",
                side.pane_id
            ),
            "real full-screen TUI exits alternate screen",
            |output| output.trim() == "0",
        );
        // Re-establish the shell-owned alternate screen used by the later
        // transport-loss assertion after the real TUI has exited.
        send_line_retry(
            side.terminal_id,
            &fullscreen_command,
            "restore alternate screen marker",
        );
        wait_for_remote_tmux(
            &fixture,
            &format!(
                "tmux display-message -p -t %{} '#{{alternate_on}}'",
                side.pane_id
            ),
            "restore alternate screen marker",
            |output| output.trim() == "1",
        );
        wait_for_pane_text(&side, FULLSCREEN_MARKER, "restored alternate screen marker");
    }

    let before_loss = session_snapshot(id).expect("session snapshot before transport loss");
    let before_ids = pane_identity_set(&before_loss);
    // Detaching the native Control Mode client from another ordinary SSH
    // client simulates an abrupt transport loss while leaving tmux alive.
    detach_control_mode_client(&fixture);
    wait_for_reconnecting(id, "abrupt transport loss");
    assert!(send_bytes(side.terminal_id, b"should be rejected").is_err());

    // tmux remains durable while SSH is gone. Inject a shell sentinel via a
    // separate ordinary SSH client before reconnecting the native owner.
    remote_send_keys(&fixture, side.pane_id, DURABLE_MARKER);
    wait_for_ready_without_prompt(id, "automatic reconnect after transport loss");
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

    // A graceful disconnect must clean up zoom state as well. Exercise the
    // issue #30 sequence with the authoritative saved layout and separately
    // fetched pane state: same-window pane switch -> another window -> back,
    // then compare the remote state before and after shutdown. The viewport is
    // fixed at 60x18 by the resize checks above, so this case also requires an
    // exact saved-layout and window-dimension match.
    let reconnected_main = wait_for_pane_handle(id, main.pane_id, "refresh main after recovery");
    let reconnected_side_active = wait_for_pane_handle(
        id,
        side_active.pane_id,
        "refresh side active after recovery",
    );
    let baseline_layout = remote_tmux_layout(&fixture, "baseline");
    exercise_zoom_switch_sequence(
        id,
        &fixture,
        &reconnected_main,
        &reconnected_side,
        &reconnected_side_active,
        &baseline_layout,
        "first zoom switch sequence",
    );
    let before_disconnect = remote_tmux_layout(&fixture, "before_disconnect");
    assert_remote_layout_preserved(
        &baseline_layout,
        &before_disconnect,
        "before_disconnect",
        true,
    );
    assert_zoom_state(
        &before_disconnect,
        reconnected_side_active.pane_id,
        "before_disconnect",
    );
    disconnect_terminal(id).expect("graceful native disconnect");
    wait_for_state(id, ConnectionState::Disconnected, "graceful disconnect");
    let after_disconnect = remote_tmux_layout(&fixture, "after_disconnect");
    assert_remote_layout_preserved(
        &baseline_layout,
        &after_disconnect,
        "after_disconnect",
        true,
    );
    assert_no_zoom(&after_disconnect, "after_disconnect");
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

    // Repeat the same ownership path after same-process transport recovery.
    // The recovery hook may restore the ordinary layout during the loss, but
    // the recovered actor must re-establish ownership only for the selected
    // window before the final explicit Disconnect.
    reconnect_and_select_meeterm(
        id,
        &fixture.fingerprint,
        "recover for zoom cleanup sequence",
    );
    let recovery_main = wait_for_pane_handle(id, main.pane_id, "refresh main before recovery path");
    let recovery_side = wait_for_pane_handle(id, side.pane_id, "refresh side before recovery path");
    let recovery_side_active = wait_for_pane_handle(
        id,
        side_active.pane_id,
        "refresh side active before recovery path",
    );
    let recovery_baseline = remote_tmux_layout(&fixture, "recovery_baseline");
    exercise_zoom_switch_sequence(
        id,
        &fixture,
        &recovery_main,
        &recovery_side,
        &recovery_side_active,
        &recovery_baseline,
        "recovery zoom switch before transport loss",
    );
    let selected_before_loss = remote_tmux_layout(&fixture, "before_recovery_disconnect");
    assert_remote_layout_preserved(
        &recovery_baseline,
        &selected_before_loss,
        "before_recovery_disconnect",
        true,
    );
    assert_zoom_state(
        &selected_before_loss,
        recovery_side_active.pane_id,
        "before_recovery_disconnect",
    );
    detach_control_mode_client(&fixture);
    wait_for_reconnecting(id, "zoom cleanup recovery transport loss");
    wait_for_ready_without_prompt(id, "zoom cleanup recovery ready");
    let recovered_main = wait_for_pane_handle(id, main.pane_id, "refresh main after zoom recovery");
    let recovered_side = wait_for_pane_handle(id, side.pane_id, "refresh side after zoom recovery");
    let recovered_side_active = wait_for_pane_handle(
        id,
        side_active.pane_id,
        "refresh side active after zoom recovery",
    );
    exercise_zoom_switch_sequence(
        id,
        &fixture,
        &recovered_main,
        &recovered_side,
        &recovered_side_active,
        &recovery_baseline,
        "recovery zoom switch after transport loss",
    );
    let selected_after_recovery = remote_tmux_layout(&fixture, "after_recovery_selection");
    assert_remote_layout_preserved(
        &recovery_baseline,
        &selected_after_recovery,
        "after_recovery_selection",
        true,
    );
    assert_zoom_state(
        &selected_after_recovery,
        recovered_side_active.pane_id,
        "after_recovery_selection",
    );
    disconnect_terminal(id).expect("disconnect after recovered zoom sequence");
    wait_for_state(
        id,
        ConnectionState::Disconnected,
        "disconnect after recovered zoom sequence",
    );
    let after_recovery_disconnect = remote_tmux_layout(&fixture, "after_recovery_disconnect");
    assert_remote_layout_preserved(
        &recovery_baseline,
        &after_recovery_disconnect,
        "after_recovery_disconnect",
        true,
    );
    assert_no_zoom(&after_recovery_disconnect, "after_recovery_disconnect");

    // Establish the desired reconnect selection while a live controller owns
    // the command stream.  Sending a selection immediately after disconnect
    // would race the cancelled controller's final zoom cleanup.
    let previous_main_terminal = main.terminal_id;
    reconnect_and_select_meeterm(
        id,
        &fixture.fingerprint,
        "prepare main selection before zoom regression",
    );
    let main = wait_for_pane_handle(
        id,
        main.pane_id,
        "refresh main handle after manual runtime selection",
    );
    assert_eq!(
        main.terminal_id, previous_main_terminal,
        "the first tmux pane reuses the connection owner's reset native terminal"
    );
    select_pane(id, main.pane_id).expect("select main before zoom regression");
    wait_for_selected_pane(id, main.pane_id, "select main before zoom regression");
    disconnect_terminal(id).expect("disconnect before preexisting zoom regression");
    wait_for_state(
        id,
        ConnectionState::Disconnected,
        "disconnect before preexisting zoom regression",
    );

    // A desktop client may have zoomed a window before meeterm connects.  The
    // mobile controller must observe and preserve that ownership boundary:
    // selecting the pre-zoomed pane is allowed, but disconnect must not undo
    // the desktop layout.  Clear the zoom before resuming the rest of this
    // fixture so its later assertions still exercise meeterm-owned cleanup.
    run_remote_tmux(
        &fixture,
        &format!(
            "tmux select-window -t @{}; tmux select-pane -t %{}; tmux resize-pane -Z -t %{}",
            side.window_id, side_active.pane_id, side_active.pane_id
        ),
        "create preexisting desktop zoom",
    );
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        "preexisting desktop zoom",
        |output| output.trim() == "1",
    );
    reconnect_and_select_meeterm(
        id,
        &fixture.fingerprint,
        "reconnect with preexisting desktop zoom",
    );
    // Select the other pane in the already-zoomed window.  tmux would
    // otherwise drop the zoom as a side effect of select-pane; the native
    // controller must restore the zoom while leaving cleanup unowned.
    select_pane(id, side.pane_id).expect("select pane in preexisting zoomed window");
    wait_for_selected_pane(id, side.pane_id, "select pane in preexisting zoomed window");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        "preserve preexisting desktop zoom after pane selection",
        |output| output.trim() == "1",
    );
    disconnect_terminal(id).expect("disconnect after preexisting zoom selection");
    wait_for_state(
        id,
        ConnectionState::Disconnected,
        "disconnect after preexisting zoom selection",
    );
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        "preserve preexisting desktop zoom after disconnect",
        |output| output.trim() == "1",
    );
    run_remote_tmux(
        &fixture,
        &format!("tmux resize-pane -Z -t %{}", side.pane_id),
        "clear preexisting desktop zoom",
    );
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        "clear preexisting desktop zoom",
        |output| output.trim() == "0",
    );
    reconnect_and_select_meeterm(id, &fixture.fingerprint, "resume meeterm-owned zoom checks");

    // External pane removal invalidates the borrowed native handle and must
    // not leave a dead zoom target that breaks the next mobile selection.
    reconnect_and_select_meeterm(id, &fixture.fingerprint, "reconnect before pane removal");
    let side = wait_for_pane_handle(
        id,
        side.pane_id,
        "refresh side handle after manual runtime selection",
    );
    let main = wait_for_pane_handle(
        id,
        main.pane_id,
        "refresh main handle after manual runtime selection",
    );
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

    // CRUD uses numeric tmux identities and one quoted argument for each
    // user-visible name. Creation selects the newly created mobile target so
    // the app can open it immediately while the ordinary tmux layout remains
    // durable on the remote server.
    let before_crud = session_snapshot(id).expect("CRUD baseline snapshot");
    let workspace_name = "daily ; # $HOME \\ \" 日本語";
    create_workspace(id, workspace_name).expect("create workspace");
    let after_workspace = wait_for_snapshot(id, "workspace creation", |snapshot| {
        snapshot.windows.len() == before_crud.windows.len() + 1
    });
    let created_window = after_workspace
        .windows
        .iter()
        .find(|window| {
            !before_crud
                .windows
                .iter()
                .any(|old| old.window_id == window.window_id)
        })
        .expect("created workspace identity")
        .clone();
    assert_eq!(created_window.name, workspace_name);
    assert!(created_window.selected);

    let renamed_workspace = "renamed workspace ; $HOME";
    rename_workspace(id, created_window.window_id, renamed_workspace).expect("rename workspace");
    let renamed = wait_for_snapshot(id, "workspace rename", |snapshot| {
        snapshot.windows.iter().any(|window| {
            window.window_id == created_window.window_id && window.name == renamed_workspace
        })
    });
    let created_window = renamed
        .windows
        .iter()
        .find(|window| window.window_id == created_window.window_id)
        .expect("renamed workspace")
        .clone();

    let before_pane_count = renamed.panes.len();
    create_pane(id, created_window.window_id).expect("create pane");
    let after_pane = wait_for_snapshot(id, "pane creation", |snapshot| {
        snapshot.panes.len() == before_pane_count + 1
            && snapshot
                .panes
                .iter()
                .any(|pane| pane.window_id == created_window.window_id && pane.selected)
    });
    let created_pane = after_pane
        .panes
        .iter()
        .find(|pane| {
            pane.window_id == created_window.window_id
                && !created_window
                    .panes
                    .iter()
                    .any(|old| old.pane_id == pane.pane_id)
        })
        .expect("created pane identity")
        .clone();
    let pane_name = "editor pane ; # $HOME \\ \" 日本語";
    rename_pane(id, created_pane.pane_id, pane_name).expect("rename pane");
    let renamed_pane = wait_for_snapshot(id, "pane rename", |snapshot| {
        snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == created_pane.pane_id && pane.pane_name == pane_name)
    });
    assert_eq!(
        renamed_pane
            .panes
            .iter()
            .find(|pane| pane.pane_id == created_pane.pane_id)
            .expect("renamed pane")
            .pane_name,
        pane_name
    );
    close_pane(id, created_pane.pane_id).expect("close pane");
    let after_close_pane = wait_for_snapshot(id, "pane close", |snapshot| {
        !snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == created_pane.pane_id)
    });
    assert_eq!(after_close_pane.panes.len(), before_pane_count);
    close_workspace(id, created_window.window_id).expect("close workspace");
    let after_close_workspace = wait_for_snapshot(id, "workspace close", |snapshot| {
        !snapshot
            .windows
            .iter()
            .any(|window| window.window_id == created_window.window_id)
    });
    assert_eq!(
        after_close_workspace.windows.len(),
        before_crud.windows.len()
    );

    // Keep a real Vim process alive while the original native owner is
    // destroyed. A new owner must attach to the same durable tmux pane,
    // recapture the alternate screen into a fresh terminal registry, and
    // continue in Vim's existing insert mode without another `i` command.
    select_pane(id, main.pane_id).expect("select shell pane for cold TUI recovery");
    wait_for_selected_pane(id, main.pane_id, "select shell pane for cold TUI recovery");
    send_line_retry(
        main.terminal_id,
        "vim -Nu NONE -n",
        "launch Vim for cold TUI recovery",
    );
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t %{} '#{{alternate_on}}'",
            main.pane_id
        ),
        "cold Vim enters alternate screen",
        |output| output.trim() == "1",
    );
    send_raw_retry(
        main.terminal_id,
        format!("i{TUI_COLD_MARKER}").as_bytes(),
        "cold Vim initial marker",
    );
    wait_for_pane_text(&main, TUI_COLD_MARKER, "cold Vim initial marker");

    disconnect_terminal(id).expect("disconnect original owner with Vim alive");
    wait_for_state(
        id,
        ConnectionState::Disconnected,
        "disconnect original owner with Vim alive",
    );
    assert!(
        destroy_terminal(id),
        "destroy original terminal registry owner"
    );

    let cold_id = create_terminal(80, 24).expect("create cold TUI recovery owner");
    let _cold_guard = TerminalGuard { id: cold_id };
    connect_host_and_select_meeterm(cold_id, &fixture, "cold TUI recovery owner picker");
    let cold_session = wait_for_snapshot(cold_id, "cold TUI recovery session", |snapshot| {
        snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == main.pane_id)
    });
    let cold_pane = cold_session
        .panes
        .iter()
        .find(|pane| pane.pane_id == main.pane_id)
        .expect("cold owner finds Vim pane identity")
        .clone();
    refresh_terminal(cold_id).expect("recapture Vim screen in fresh owner");
    wait_for_pane_text(&cold_pane, TUI_COLD_MARKER, "cold Vim screen recapture");
    send_raw_retry(
        cold_pane.terminal_id,
        format!("{TUI_COLD_INPUT_MARKER}\x1b").as_bytes(),
        "cold Vim recovered insert mode",
    );
    wait_for_pane_text(
        &cold_pane,
        TUI_COLD_INPUT_MARKER,
        "cold Vim recovered insert mode",
    );
    send_raw_retry(cold_pane.terminal_id, b":q!\r", "exit cold Vim TUI");
    wait_for_remote_tmux(
        &fixture,
        &format!(
            "tmux display-message -p -t %{} '#{{alternate_on}}'",
            main.pane_id
        ),
        "cold Vim exits alternate screen",
        |output| output.trim() == "0",
    );
    // Explicitly closing the final pane must converge to Disconnected. It is
    // a user requested end of the managed session, so reconnect must not
    // silently recreate an empty `meeterm` workspace.
    let remaining = session_snapshot(cold_id).expect("cold session after Vim exit");
    let final_pane = remaining
        .panes
        .last()
        .expect("cold session retains a pane after Vim exit")
        .clone();
    for pane in remaining
        .panes
        .iter()
        .filter(|pane| pane.pane_id != final_pane.pane_id)
    {
        close_pane(cold_id, pane.pane_id).expect("close non-final cold pane");
        wait_for_snapshot(cold_id, "close non-final cold pane", |snapshot| {
            !snapshot
                .panes
                .iter()
                .any(|candidate| candidate.pane_id == pane.pane_id)
        });
    }
    close_pane(cold_id, final_pane.pane_id).expect("close final cold pane");
    wait_for_state(
        cold_id,
        ConnectionState::Disconnected,
        "close final cold pane",
    );

    // The same final-window path is exercised after a manual reconnect. The
    // runtime is absent, so the picker must show empty tmux discovery and the
    // test explicitly creates meeterm again before attaching.
    reconnect_and_create_meeterm(
        cold_id,
        &fixture.fingerprint,
        "recreate cold owner for final workspace close",
    );
    let recreated = wait_for_session(cold_id, 1, "recreated one-pane session");
    let final_window = recreated
        .windows
        .first()
        .expect("recreated final workspace")
        .window_id;
    close_workspace(cold_id, final_window).expect("close final cold workspace");
    wait_for_state(
        cold_id,
        ConnectionState::Disconnected,
        "close final cold workspace",
    );

    // Once the key is pinned, a wrong passphrase fails before tmux is opened.
    let wrong_id = create_terminal(80, 24).expect("create wrong-passphrase terminal");
    let _wrong_guard = TerminalGuard { id: wrong_id };
    let wrong_options = fixture.options_with_passphrase("definitely-wrong-passphrase");
    connect_host(wrong_id, wrong_options).expect("start wrong-passphrase connection");
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
    connect_host(changed_id, fixture.options()).expect("start changed-key connection");
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

/// Exercise the password-only path against the disposable Docker OpenSSH
/// fixture. The fixture publishes only endpoint, trust-store, and password
/// environment variables; no key material is needed by this test.
#[test]
#[ignore = "requires the disposable password-enabled OpenSSH fixture"]
fn real_openssh_password_auth_reconnect_and_host_key_gate() {
    let fixture = PasswordFixtureConfig::from_environment();
    let id = create_terminal(80, 24).expect("create password SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host(id, fixture.options()).expect("start password host connection");
    let discovery = wait_for_runtime_picker(id, &fixture.fingerprint, "password authentication");
    let ready = create_tmux_meeterm_from_picker(id, &discovery, "password authentication");
    assert_eq!(
        connection_string(&ready.algorithm, ready.algorithm_len),
        "ssh-ed25519"
    );

    let initial = wait_for_session(id, 1, "password meeterm session");
    let pane = initial
        .panes
        .first()
        .expect("password session pane")
        .clone();
    prepare_pane(&pane, "password pane shell");
    let marker = "MEETERM_PASSWORD_AUTH_OK_5C2A";
    send_line_retry(
        pane.terminal_id,
        &format!("printf '{}\\n'", printf_octal(marker)),
        "password authentication marker",
    );
    wait_for_pane_text(&pane, marker, "password authentication marker");

    // A normal disconnect preserves the parsed password profile. Manual
    // reconnect must use password authentication again, rediscover the
    // picker, and retain the same tmux pane. The first pane reuses the
    // connection owner's native ID after its terminal state is reset.
    disconnect_terminal(id).expect("disconnect password connection");
    wait_for_state(id, ConnectionState::Disconnected, "password disconnect");
    reconnect_and_select_meeterm(id, &fixture.fingerprint, "password reconnect");
    let reconnected = wait_for_session(id, 1, "password reconnect session");
    let reconnected_pane = reconnected
        .panes
        .iter()
        .find(|candidate| candidate.pane_id == pane.pane_id)
        .expect("password reconnect retains pane identity")
        .clone();
    assert_eq!(
        reconnected_pane.terminal_id, pane.terminal_id,
        "password reconnect reuses the reset connection-owner terminal ID"
    );
    let reconnect_marker = "MEETERM_PASSWORD_RECONNECT_OK_6D3B";
    send_line_retry(
        reconnected_pane.terminal_id,
        &format!("printf '{}\\n'", printf_octal(reconnect_marker)),
        "password reconnect marker",
    );
    wait_for_pane_text(
        &reconnected_pane,
        reconnect_marker,
        "password reconnect marker",
    );

    // A rejected password must not fall through to public-key or any other
    // method. The host key is already pinned, so this reaches authentication.
    let wrong_id = create_terminal(80, 24).expect("create wrong-password terminal");
    let _wrong_guard = TerminalGuard { id: wrong_id };
    connect_host(
        wrong_id,
        fixture.options_with_password("wrong password that must be rejected"),
    )
    .expect("start wrong-password connection");
    let wrong = wait_for_state(wrong_id, ConnectionState::Failed, "wrong password");
    assert_eq!(
        connection_string(&wrong.error_code, wrong.error_code_len),
        "auth_failed"
    );
    assert_eq!(
        connection_string(&wrong.error_message, wrong.error_message_len),
        "SSH authentication failed."
    );

    // Use an independent trust path so the long-lived fixture remains usable
    // by other smoke tests. A changed host identity must fail before the
    // deliberately wrong password can be attempted.
    let changed_trust = std::env::temp_dir().join(format!(
        "meeterm-password-changed-trust-{}",
        std::process::id()
    ));
    write_alternate_password_trust_record(&fixture, &changed_trust);
    let changed_id = create_terminal(80, 24).expect("create changed-key terminal");
    let _changed_guard = TerminalGuard { id: changed_id };
    connect_host(
        changed_id,
        fixture.options_with_password_and_trust(
            "wrong password must never be reached",
            changed_trust.clone(),
        ),
    )
    .expect("start changed-key connection");
    let changed = wait_for_state(
        changed_id,
        ConnectionState::Failed,
        "changed host-key rejection before password auth",
    );
    assert_eq!(
        connection_string(&changed.error_code, changed.error_code_len),
        "host_key_changed"
    );
    assert_eq!(
        connection_string(&changed.fingerprint, changed.fingerprint_len),
        fixture.fingerprint
    );
    let _ = fs::remove_file(changed_trust);
}

/// Poll one attachment until it reaches `phase`, or panic on a terminal
/// phase that cannot reach it. Returns the last observed snapshot.
fn wait_for_attachment_phase(
    attachment_id: u64,
    phase: AttachmentPhase,
    label: &str,
) -> AttachmentSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = attachment_snapshot(attachment_id).expect("attachment snapshot");
        if snapshot.phase == phase as u32 {
            return snapshot;
        }
        let terminal = snapshot.phase == AttachmentPhase::Failed as u32
            || snapshot.phase == AttachmentPhase::Cancelled as u32
            || (snapshot.phase == AttachmentPhase::Inserted as u32
                && phase != AttachmentPhase::Inserted);
        assert!(
            !terminal,
            "{label}: attachment reached terminal phase {} with code={} message={}",
            snapshot.phase,
            connection_string(&snapshot.error_code, snapshot.error_code_len),
            connection_string(&snapshot.error_message, snapshot.error_message_len),
        );
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {label}: phase={}, bytes={}/{}, code={} message={}",
                snapshot.phase,
                snapshot.bytes_uploaded,
                snapshot.size_bytes,
                connection_string(&snapshot.error_code, snapshot.error_code_len),
                connection_string(&snapshot.error_message, snapshot.error_message_len),
            );
        }
        sleep(POLL_INTERVAL);
    }
}

/// Generated-name grammar: `meeterm-<8 digits>-<6 digits>-<16 lower hex>`
/// plus an optional `.<lower-alnum>` extension.
fn generated_name_valid(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("meeterm-") else {
        return false;
    };
    let mut parts = rest.splitn(3, '-');
    let date = parts.next().unwrap_or_default();
    let time = parts.next().unwrap_or_default();
    let tail = parts.next().unwrap_or_default();
    let (random, extension) = tail
        .split_once('.')
        .map_or((tail, None), |(random, ext)| (random, Some(ext)));
    date.len() == 8
        && date.bytes().all(|byte| byte.is_ascii_digit())
        && time.len() == 6
        && time.bytes().all(|byte| byte.is_ascii_digit())
        && random.len() == 16
        && random
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && extension.is_none_or(|ext| {
            !ext.is_empty()
                && ext.len() <= 8
                && ext
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Poll one attachment until `flag` appears in the snapshot flags.
fn wait_for_attachment_flag(attachment_id: u64, flag: u32, label: &str) -> AttachmentSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = attachment_snapshot(attachment_id).expect("attachment snapshot");
        if snapshot.flags & flag == flag {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: flag {flag:#x} never appeared; phase={} code={} message={}",
            snapshot.phase,
            connection_string(&snapshot.error_code, snapshot.error_code_len),
            connection_string(&snapshot.error_message, snapshot.error_message_len),
        );
        sleep(POLL_INTERVAL);
    }
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py --sftp for a real local sshd with SFTP"]
fn real_openssh_sftp_attachment_upload_and_insert() {
    assert_eq!(
        env::var("MEETERM_SSH_SFTP").ok().as_deref(),
        Some("1"),
        "attachment test requires the fixture started with --sftp"
    );
    let fixture = FixtureConfig::from_environment();
    create_fixture_tmux_session(&fixture, "meeterm");
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host_and_select_meeterm(id, &fixture, "sftp attachment selection");
    let initial = wait_for_session(id, 1, "attachment session");
    let pane = initial.panes.first().expect("attachment pane").clone();
    select_pane(id, pane.pane_id).expect("select attachment pane");
    wait_for_selected_pane(id, pane.pane_id, "select attachment pane");
    prepare_pane(&pane, "attachment pane shell");

    // The picked file must stay readable and unchanged for the operation's
    // lifetime; its name must never leak into the remote path. The file is
    // named `.jpg` but carries PNG magic — the remote extension must come
    // from the data, not the picked name.
    let scratch = scratch_dir("meeterm-att-src");
    // Backstop remote cleanup: records the shared default attachments
    // directory now so a panic can still collect only this run's names.
    let mut remote_guard = RemoteAttachmentGuard::new(&fixture);
    // Legs that only exercise upload mechanics — not the default-dir
    // layout itself — target a dedicated remote dir under /tmp so the
    // shared real-$HOME directory is touched as little as possible.
    let remote_scratch = format!("/tmp/meeterm-att-remote-{}", std::process::id());
    fs::create_dir_all(&remote_scratch).expect("create dedicated remote dir");
    remote_guard.track(remote_scratch.clone());
    let local = scratch.join("picked image.jpg");
    let mut payload: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    payload.extend((0..96 * 1024_u32).map(|index| (index % 251) as u8));
    fs::write(&local, &payload).expect("write picked image");
    let local_path = local.to_str().expect("UTF-8 local path");

    let attachment_id = attachment_begin(
        id,
        local_path,
        "picked image.jpg",
        payload.len() as u64,
        None,
    )
    .expect("begin SFTP attachment");
    // One live operation at a time: a second begin is rejected while this
    // one is in flight.
    assert!(
        attachment_begin(id, local_path, "second.jpg", payload.len() as u64, None).is_err(),
        "a second live attachment must be rejected"
    );
    let uploaded =
        wait_for_attachment_phase(attachment_id, AttachmentPhase::Uploaded, "sftp upload");
    assert_eq!(uploaded.bytes_uploaded, payload.len() as u64);
    let remote_path = connection_string(&uploaded.remote_path, uploaded.remote_path_len);
    assert!(
        remote_path.contains("/.local/share/meeterm/attachments/meeterm-"),
        "unexpected remote attachment path: {remote_path}"
    );
    assert!(
        !remote_path.contains("picked"),
        "picked filename leaked into remote path: {remote_path}"
    );
    // The basename is generated: meeterm-<YYYYMMDD>-<HHMMSS>-<16 hex>,
    // extension decided by magic bytes (PNG data under a .jpg name).
    let basename = remote_path.rsplit('/').next().expect("remote basename");
    assert!(
        generated_name_valid(basename),
        "remote basename outside generated grammar: {basename}"
    );
    assert!(
        basename.ends_with(".png"),
        "extension must come from magic bytes, not the picked name: {basename}"
    );

    // Byte equality and restrictive permissions on the remote side.
    let remote_sha = run_remote_tmux(
        &fixture,
        &format!("sha256sum '{remote_path}' | cut -d' ' -f1"),
        "remote attachment sha256",
    );
    let local_sha = Command::new("sha256sum")
        .arg(&local)
        .output()
        .expect("local sha256sum");
    assert!(local_sha.status.success(), "local sha256sum failed");
    assert_eq!(
        String::from_utf8_lossy(&remote_sha.stdout)
            .split_whitespace()
            .next(),
        String::from_utf8_lossy(&local_sha.stdout)
            .split_whitespace()
            .next(),
        "remote attachment bytes differ from the picked image"
    );
    let modes = run_remote_tmux(
        &fixture,
        &format!("stat -c '%a' '{remote_path}'; stat -c '%a' \"$(dirname '{remote_path}')\""),
        "remote attachment permissions",
    );
    let modes_text = String::from_utf8_lossy(&modes.stdout);
    let mut modes = modes_text.lines();
    assert_eq!(modes.next().map(str::trim), Some("600"), "file mode");
    assert_eq!(modes.next().map(str::trim), Some("700"), "directory mode");

    // The destination fence rejects insertion after the user selected a
    // different pane; it records a pending reason instead of retargeting.
    run_remote_tmux(
        &fixture,
        &format!(
            "tmux split-window -h -t %{} 'exec /bin/sh -i'",
            pane.pane_id
        ),
        "split pane for stale-destination check",
    );
    let topology = wait_for_session(id, 2, "split topology for attachment fence");
    let other = topology
        .panes
        .iter()
        .find(|candidate| candidate.pane_id != pane.pane_id)
        .expect("second pane after split")
        .clone();
    select_pane(id, other.pane_id).expect("select other pane");
    wait_for_selected_pane(id, other.pane_id, "select other pane");
    let rejected = attachment_insert(id, attachment_id);
    assert!(
        rejected.is_err(),
        "insert after destination change must be rejected"
    );
    let blocked = attachment_snapshot(attachment_id).expect("blocked attachment snapshot");
    assert_eq!(blocked.phase, AttachmentPhase::Uploaded as u32);
    assert_eq!(
        connection_string(&blocked.error_code, blocked.error_code_len),
        "destination_changed"
    );

    // Reselecting the fenced pane restores the explicit insert path.
    select_pane(id, pane.pane_id).expect("reselect attachment pane");
    wait_for_selected_pane(id, pane.pane_id, "reselect attachment pane");
    attachment_insert(id, attachment_id).expect("insert remote path");
    let inserted =
        wait_for_attachment_phase(attachment_id, AttachmentPhase::Inserted, "path insert");
    assert_eq!(
        inserted.flags & ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED,
        ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED
    );
    // Exactly one single-quoted line lands in the pane input; Enter is
    // never sent, so the shell echoes the line without executing it. The
    // generated path is longer than the 80-column pane: it soft-wraps, so
    // the checks run against the wrap-joined viewport text.
    let quoted = format!("'{remote_path}'");
    let shown = wait_for_pane_snapshot(&pane, "quoted remote path echo", |snapshot| {
        snapshot_text(snapshot).replace('\n', "").contains(&quoted)
    });
    let text = snapshot_text(&shown).replace('\n', "");
    assert!(
        !text.contains("Permission denied") && !text.contains("not found"),
        "the inserted path must not have been executed:\n{text}"
    );
    assert!(
        text.trim_end().ends_with(&quoted),
        "insert must append one path line without Enter, got: {text:?}"
    );

    // Explicit remote deletion removes only the generated names; verified
    // removal is reported by the flag while the inserted phase is kept.
    // The call is owner-bound: another terminal id cannot delete.
    assert!(
        attachment_delete_remote(id + 10_000, attachment_id).is_err(),
        "delete from a foreign terminal id must be rejected"
    );
    attachment_delete_remote(id, attachment_id).expect("queue remote delete");
    let removed = wait_for_attachment_flag(
        attachment_id,
        ATTACHMENT_FLAG_REMOTE_REMOVED,
        "remote delete",
    );
    assert_eq!(removed.phase, AttachmentPhase::Inserted as u32);
    // The file itself must be gone; the shared attachments directory is
    // removed only when empty, which is not guaranteed on this host.
    let gone = run_remote_tmux(
        &fixture,
        &format!(
            "test -e '{remote_path}' && echo FILE_PRESENT || echo FILE_GONE; \
             find \"$HOME/.local/share/meeterm/attachments\" \
                -name '.meeterm-partial-{basename}' 2>/dev/null | wc -l"
        ),
        "remote file deleted",
    );
    let gone_text = String::from_utf8_lossy(&gone.stdout);
    assert!(
        gone_text.contains("FILE_GONE"),
        "remote file still present after removal: {gone_text}"
    );
    assert_eq!(
        gone_text.lines().last().map(str::trim),
        Some("0"),
        "remote partial leaked"
    );
    // Idempotent: a second remove while already flagged returns success.
    attachment_delete_remote(id, attachment_id).expect("idempotent remote delete");
    attachment_dispose(attachment_id).expect("dispose first attachment");

    // An explicit remote directory is validated component-by-component:
    // a symlink inside the chain is refused rather than followed.
    remote_guard.track("/tmp/meeterm-att-target");
    remote_guard.track("/tmp/meeterm-att-link");
    run_remote_tmux(
        &fixture,
        "rm -rf /tmp/meeterm-att-target /tmp/meeterm-att-link; \
         mkdir -p /tmp/meeterm-att-target && \
         ln -s /tmp/meeterm-att-target /tmp/meeterm-att-link",
        "create remote symlink dir",
    );
    let symlink_id = attachment_begin(
        id,
        local_path,
        "picked image.jpg",
        payload.len() as u64,
        Some("/tmp/meeterm-att-link/inside"),
    )
    .expect("begin symlinked-dir attachment");
    let symlinked =
        wait_for_attachment_phase(symlink_id, AttachmentPhase::Failed, "symlink dir rejection");
    assert_eq!(
        connection_string(&symlinked.error_code, symlinked.error_code_len),
        "remote_unsafe_path"
    );
    attachment_dispose(symlink_id).expect("dispose symlink attachment");
    run_remote_tmux(
        &fixture,
        "rm -f /tmp/meeterm-att-link; rm -rf /tmp/meeterm-att-target",
        "remote symlink cleanup",
    );

    // `~/` expands against the SFTP realpath(".") result — never a
    // client-side home guess. The target dir must already exist; we never
    // create or chmod an explicit directory.
    remote_guard.track("$HOME/meeterm-att-tilde");
    run_remote_tmux(
        &fixture,
        "mkdir -p \"$HOME/meeterm-att-tilde\" && chmod 0700 \"$HOME/meeterm-att-tilde\"",
        "create remote tilde dir",
    );
    let tilde_id = attachment_begin(
        id,
        local_path,
        "picked image.jpg",
        payload.len() as u64,
        Some("~/meeterm-att-tilde"),
    )
    .expect("begin tilde-dir attachment");
    let tilde_uploaded =
        wait_for_attachment_phase(tilde_id, AttachmentPhase::Uploaded, "tilde-dir upload");
    let tilde_path = connection_string(&tilde_uploaded.remote_path, tilde_uploaded.remote_path_len);
    let tilde_base = tilde_path.rsplit('/').next().expect("tilde basename");
    assert!(
        tilde_path.contains("/meeterm-att-tilde/meeterm-") && generated_name_valid(tilde_base),
        "tilde-expanded remote path: {tilde_path}"
    );
    assert!(
        !tilde_path.starts_with("~/"),
        "remote path must be absolute after server-side expansion: {tilde_path}"
    );
    attachment_delete_remote(id, tilde_id).expect("queue tilde remote delete");
    wait_for_attachment_flag(
        tilde_id,
        ATTACHMENT_FLAG_REMOTE_REMOVED,
        "tilde remote delete",
    );
    attachment_dispose(tilde_id).expect("dispose tilde attachment");
    run_remote_tmux(
        &fixture,
        "rm -rf \"$HOME/meeterm-att-tilde\"",
        "remote tilde cleanup",
    );

    // Path characters that could break the single-quoted insertion line
    // are rejected at transfer validation, before a line can exist.
    for (index, dir) in [
        "/tmp/meeterm-att-'quote",
        "/tmp/meeterm-att-..\nnewline",
        "/tmp/meeterm-att-\u{1}control",
        "/tmp/meeterm-att/../escape",
    ]
    .iter()
    .enumerate()
    {
        let unsafe_id = attachment_begin(
            id,
            local_path,
            "picked image.jpg",
            payload.len() as u64,
            Some(dir),
        )
        .unwrap_or_else(|_| panic!("begin unsafe-dir attachment #{index}"));
        let failed = wait_for_attachment_phase(
            unsafe_id,
            AttachmentPhase::Failed,
            "unsafe remote dir rejection",
        );
        assert_eq!(
            connection_string(&failed.error_code, failed.error_code_len),
            "remote_unsafe_path",
            "remote dir {dir:?} must fail unsafe"
        );
        attachment_dispose(unsafe_id).expect("dispose unsafe-dir attachment");
    }

    // A read-only explicit directory fails the exclusive-create writability
    // probe with remote_permission_denied.
    remote_guard.track("$HOME/meeterm-att-ro");
    run_remote_tmux(
        &fixture,
        "mkdir -p \"$HOME/meeterm-att-ro\" && chmod 0555 \"$HOME/meeterm-att-ro\"",
        "create read-only remote dir",
    );
    let denied_id = attachment_begin(
        id,
        local_path,
        "picked image.jpg",
        payload.len() as u64,
        Some("~/meeterm-att-ro"),
    )
    .expect("begin read-only-dir attachment");
    let denied = wait_for_attachment_phase(
        denied_id,
        AttachmentPhase::Failed,
        "read-only dir rejection",
    );
    assert_eq!(
        connection_string(&denied.error_code, denied.error_code_len),
        "remote_permission_denied",
        "read-only explicit dir must fail remote_permission_denied: {}",
        connection_string(&denied.error_message, denied.error_message_len),
    );
    attachment_dispose(denied_id).expect("dispose denied attachment");
    run_remote_tmux(
        &fixture,
        "chmod 0700 \"$HOME/meeterm-att-ro\" && rm -rf \"$HOME/meeterm-att-ro\"",
        "remote read-only cleanup",
    );

    // A sparse file over the product cap is rejected at begin without
    // touching the remote side.
    let oversized = scratch.join("picked oversized.bin");
    fs::File::create(&oversized)
        .expect("create oversized picked image")
        .set_len(MAX_ATTACHMENT_BYTES + 1)
        .expect("size oversized picked image");
    let oversized_path = oversized.to_str().expect("UTF-8 oversized path");
    assert!(
        matches!(
            attachment_begin(
                id,
                oversized_path,
                "picked oversized.bin",
                MAX_ATTACHMENT_BYTES + 1,
                None,
            ),
            Err(AttachmentError::SourceTooLarge)
        ),
        "over-cap picked file must fail source_too_large"
    );

    // The detached SFTP task must not starve the interactive loop: pane
    // input echoes back within the bounded deadline while a large upload
    // is still streaming on the same connection. The payload uses the
    // product maximum so the transfer outlasts the echo round trip; a
    // sparse file is enough — only the byte stream matters here.
    send_raw_retry(pane.terminal_id, b"\x03", "clear inserted input line");
    let responsive = scratch.join("picked large.png");
    fs::File::create(&responsive)
        .expect("create large picked image")
        .set_len(MAX_ATTACHMENT_BYTES)
        .expect("size large picked image");
    let responsive_path = responsive.to_str().expect("UTF-8 large path");
    let responsive_id = attachment_begin(
        id,
        responsive_path,
        "picked large.png",
        MAX_ATTACHMENT_BYTES,
        Some(remote_scratch.as_str()),
    )
    .expect("begin large attachment");
    let live_marker = format!("MEETERM_ATT_LIVE_{}", std::process::id());
    send_line_retry(
        pane.terminal_id,
        &format!("printf '{}\\n'", printf_octal(&live_marker)),
        "pane input during large upload",
    );
    wait_for_pane_text(&pane, &live_marker, "pane responsive during large upload");
    let during = attachment_snapshot(responsive_id).expect("mid-upload snapshot");
    assert_ne!(
        during.phase,
        AttachmentPhase::Uploaded as u32,
        "max-size upload finished before the input echo — cannot prove overlap"
    );
    wait_for_attachment_phase(responsive_id, AttachmentPhase::Uploaded, "large upload");
    attachment_delete_remote(id, responsive_id).expect("queue large remote delete");
    wait_for_attachment_flag(
        responsive_id,
        ATTACHMENT_FLAG_REMOTE_REMOVED,
        "large remote delete",
    );
    attachment_dispose(responsive_id).expect("dispose large attachment");

    // Cancelling a second operation discards its delayed completion; the
    // remote partial namespace is cleaned either way.
    let big = scratch.join("picked second.png");
    let big_payload = vec![0xA5_u8; 8 * 1024 * 1024];
    fs::write(&big, &big_payload).expect("write second picked image");
    let big_path = big.to_str().expect("UTF-8 second path");
    let cancelled_id = attachment_begin(
        id,
        big_path,
        "picked second.png",
        big_payload.len() as u64,
        Some(remote_scratch.as_str()),
    )
    .expect("begin second attachment");
    attachment_cancel(cancelled_id).expect("cancel second attachment");
    let cancelled = wait_for_attachment_phase(cancelled_id, AttachmentPhase::Cancelled, "cancel");
    assert_eq!(cancelled.phase, AttachmentPhase::Cancelled as u32);
    attachment_dispose(cancelled_id).expect("dispose cancelled attachment");
    assert!(attachment_snapshot(cancelled_id).is_err());
    // Allow any in-flight partial cleanup to settle, then verify.
    sleep(Duration::from_millis(1500));
    let leftovers = run_remote_tmux(
        &fixture,
        "find \"$HOME/.local/share/meeterm\" \
            -name '.meeterm-partial-meeterm-*' 2>/dev/null | wc -l",
        "no leftover remote partial",
    );
    assert_eq!(
        String::from_utf8_lossy(&leftovers.stdout).trim(),
        "0",
        "remote partial file leaked"
    );
    // Fixture hygiene: remove only the names this test generated — the
    // shared attachments directory may hold other workers' files.
    run_remote_tmux(
        &fixture,
        &format!(
            "find \"$HOME/.local/share/meeterm/attachments\" -maxdepth 1 \
                \\( -name '{basename}' \
                -o -name '.meeterm-partial-{basename}' \\) \
                -delete 2>/dev/null || true"
        ),
        "fixture attachment cleanup",
    );
    send_raw_retry(pane.terminal_id, b"\x03", "discard inserted input line");
    // ScratchDirGuard and RemoteAttachmentGuard collect the rest.
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py without --sftp for the negative path"]
fn real_openssh_no_sftp_attachment_fails_visibly() {
    assert_eq!(
        env::var("MEETERM_SSH_SFTP").ok().as_deref(),
        Some("0"),
        "negative attachment test requires the fixture started without --sftp"
    );
    let fixture = FixtureConfig::from_environment();
    create_fixture_tmux_session(&fixture, "meeterm");
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host_and_select_meeterm(id, &fixture, "no-sftp attachment selection");
    let initial = wait_for_session(id, 1, "attachment session");
    let pane = initial.panes.first().expect("attachment pane").clone();
    select_pane(id, pane.pane_id).expect("select attachment pane");
    wait_for_selected_pane(id, pane.pane_id, "select attachment pane");

    let scratch = scratch_dir("meeterm-att-neg");
    let local = scratch.join("picked.jpg");
    fs::write(&local, b"negative-path-image").expect("write picked image");
    let local_path = local.to_str().expect("UTF-8 local path");

    let attachment_id = attachment_begin(id, local_path, "picked.jpg", 19, None)
        .expect("begin attachment without SFTP");
    let failed =
        wait_for_attachment_phase(attachment_id, AttachmentPhase::Failed, "sftp-unavailable");
    assert_eq!(
        connection_string(&failed.error_code, failed.error_code_len),
        "sftp_unavailable"
    );

    // A failed attachment must not take the interactive connection down:
    // the pane still accepts ordinary input after the failure.
    let connection = connection_snapshot(id).expect("connection after failed attachment");
    assert_eq!(connection.state, ConnectionState::Ready as u32);
    prepare_pane(&pane, "no-sftp pane shell");
    let marker = "MEETERM_NO_SFTP_ALIVE_3E71";
    send_line_retry(
        pane.terminal_id,
        &format!("printf '{}\\n'", printf_octal(marker)),
        "pane alive after failed attachment",
    );
    wait_for_pane_text(&pane, marker, "pane alive after failed attachment");
    attachment_dispose(attachment_id).expect("dispose failed attachment");
}

#[test]
#[ignore = "requires python3 scripts/ssh/fixture.py --sftp --sftp-delay 45 for a stalled SFTP init"]
fn real_openssh_delayed_sftp_attachment_times_out() {
    assert_eq!(
        env::var("MEETERM_SSH_SFTP").ok().as_deref(),
        Some("1"),
        "timeout attachment test requires the fixture started with --sftp"
    );
    let delay: f64 = env::var("MEETERM_SSH_SFTP_DELAY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0);
    assert!(
        delay >= 40.0,
        "timeout attachment test requires --sftp-delay >= 40 (REQUEST_TIMEOUT is 30s), got {delay}"
    );
    let fixture = FixtureConfig::from_environment();
    create_fixture_tmux_session(&fixture, "meeterm");
    let id = create_terminal(80, 24).expect("create SSH terminal");
    let _guard = TerminalGuard { id };

    connect_host_and_select_meeterm(id, &fixture, "delayed-sftp attachment selection");
    let initial = wait_for_session(id, 1, "attachment session");
    let pane = initial.panes.first().expect("attachment pane").clone();
    select_pane(id, pane.pane_id).expect("select attachment pane");
    wait_for_selected_pane(id, pane.pane_id, "select attachment pane");

    let scratch = scratch_dir("meeterm-att-timeout");
    let local = scratch.join("picked.png");
    let payload: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];
    fs::write(&local, &payload).expect("write picked image");
    let local_path = local.to_str().expect("UTF-8 local path");

    // The fixture's subsystem wrapper sleeps before exec'ing sftp-server,
    // so SSH_FXP_INIT outlives the per-request timeout. The operation must
    // surface a retryable Pending(timeout), not crash the connection.
    let attachment_id = attachment_begin(id, local_path, "picked.png", payload.len() as u64, None)
        .expect("begin delayed-sftp attachment");
    // The init timeout lands at ~30s — beyond the shared WAIT_TIMEOUT —
    // so poll with a longer, still-bounded deadline.
    let deadline = Instant::now() + Duration::from_secs(75);
    let stalled = loop {
        let snapshot = attachment_snapshot(attachment_id).expect("attachment snapshot");
        if snapshot.phase == AttachmentPhase::Pending as u32 {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for stalled SFTP init: phase={}, code={} message={}",
            snapshot.phase,
            connection_string(&snapshot.error_code, snapshot.error_code_len),
            connection_string(&snapshot.error_message, snapshot.error_message_len),
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(
        connection_string(&stalled.error_code, stalled.error_code_len),
        "timeout",
        "stalled SFTP init must surface a timeout reason: {}",
        connection_string(&stalled.error_message, stalled.error_message_len),
    );

    // The stalled operation is recoverable: cancel discards it and the
    // interactive pane keeps answering input.
    attachment_cancel(attachment_id).expect("cancel stalled attachment");
    wait_for_attachment_phase(attachment_id, AttachmentPhase::Cancelled, "cancel stalled");
    attachment_dispose(attachment_id).expect("dispose stalled attachment");
    let connection = connection_snapshot(id).expect("connection after stalled attachment");
    assert_eq!(connection.state, ConnectionState::Ready as u32);
    prepare_pane(&pane, "timeout pane shell");
    let marker = "MEETERM_TIMEOUT_ALIVE_9C42";
    send_line_retry(
        pane.terminal_id,
        &format!("printf '{}\\n'", printf_octal(marker)),
        "pane alive after stalled attachment",
    );
    wait_for_pane_text(&pane, marker, "pane alive after stalled attachment");
}

struct PasswordFixtureConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    fingerprint: String,
    known_hosts: PathBuf,
}

impl PasswordFixtureConfig {
    fn from_environment() -> Self {
        assert_eq!(
            value("MEETERM_SSH_AUTH"),
            "password",
            "password test requires MEETERM_SSH_AUTH=password"
        );
        let port = value("MEETERM_SSH_PORT")
            .parse::<u16>()
            .expect("MEETERM_SSH_PORT must be a u16");
        assert!(port > 1024);
        Self {
            host: value("MEETERM_SSH_HOST"),
            port,
            username: value("MEETERM_SSH_USERNAME"),
            password: value("MEETERM_SSH_PASSWORD"),
            fingerprint: value("MEETERM_SSH_FINGERPRINT"),
            known_hosts: PathBuf::from(value("MEETERM_SSH_KNOWN_HOSTS_FILE")),
        }
    }

    fn options(&self) -> ConnectOptions {
        self.options_with_password_and_trust(&self.password, self.known_hosts.clone())
    }

    fn options_with_password(&self, password: &str) -> ConnectOptions {
        self.options_with_password_and_trust(password, self.known_hosts.clone())
    }

    fn options_with_password_and_trust(
        &self,
        password: &str,
        known_hosts: PathBuf,
    ) -> ConnectOptions {
        ConnectOptions {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            credentials: AuthOptions::password(password.to_owned()),
            known_hosts_path: known_hosts,
            backend: meeterm_core::workspace::Backend::Tmux,
            runtime: None,
        }
    }
}

fn write_alternate_password_trust_record(fixture: &PasswordFixtureConfig, path: &std::path::Path) {
    // This is a fixed valid Ed25519 public key that differs from the random
    // key generated by the disposable fixture. It is used only to exercise
    // the changed-host-key branch; no private key exists for it.
    const ALTERNATE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ meeterm-test-alternate";
    let record = format!("[{}]:{} {}\n", fixture.host, fixture.port, ALTERNATE_KEY);
    fs::write(path, record).expect("write alternate password trust store");
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
            // ConnectOptions receives the PEM text transiently because the
            // core decodes it before opening the SSH session.
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
        self.options_with_passphrase(&self.passphrase)
    }

    fn options_with_passphrase(&self, passphrase: &str) -> ConnectOptions {
        ConnectOptions {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            credentials: AuthOptions::public_key(
                self.private_key.clone(),
                Some(passphrase.to_owned()),
            ),
            known_hosts_path: self.known_hosts.clone(),
            backend: meeterm_core::workspace::Backend::Tmux,
            runtime: None,
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

/// Non-panicking remote command used only by cleanup guards: failures
/// during unwinding must not turn a test panic into an abort.
fn run_remote_quiet(fixture: &FixtureConfig, command: &str) -> Option<Output> {
    ssh_command(fixture, false).arg(command).output().ok()
}

/// The picked-file scratch directory, removed on success, failure and
/// panic alike.
struct ScratchDirGuard {
    path: PathBuf,
}

fn scratch_dir(prefix: &str) -> ScratchDirGuard {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    fs::create_dir_all(&path).expect("create attachment scratch directory");
    ScratchDirGuard { path }
}

impl std::ops::Deref for ScratchDirGuard {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for ScratchDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Backstop cleanup for remote state this test may create on the fixture
/// host. The fixture sshd acts on the real account `$HOME`, so the
/// app-private default directory is shared with anything else the account
/// runs: the guard snapshots its entries at construction and on drop
/// removes only *new* generated names plus the remote paths the test
/// explicitly registered, never a pre-existing file.
struct RemoteAttachmentGuard<'a> {
    fixture: &'a FixtureConfig,
    remote_paths: Vec<String>,
    baseline: Vec<String>,
}

const REMOTE_DEFAULT_DIR: &str = "$HOME/.local/share/meeterm/attachments";

impl<'a> RemoteAttachmentGuard<'a> {
    fn new(fixture: &'a FixtureConfig) -> Self {
        let baseline = remote_attachment_entries(fixture);
        Self {
            fixture,
            remote_paths: Vec::new(),
            baseline,
        }
    }

    /// Register a remote file or directory this test created; it is
    /// `rm -rf`'d on drop. `$HOME`-relative paths are expanded by the
    /// remote shell.
    fn track(&mut self, remote_path: impl Into<String>) {
        self.remote_paths.push(remote_path.into());
    }
}

fn remote_attachment_entries(fixture: &FixtureConfig) -> Vec<String> {
    let command = format!("ls -A1 {REMOTE_DEFAULT_DIR} 2>/dev/null || true");
    let Some(output) = run_remote_quiet(fixture, &command) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn remote_partial_name_valid(name: &str) -> bool {
    name.strip_prefix(".meeterm-partial-")
        .is_some_and(generated_name_valid)
}

impl Drop for RemoteAttachmentGuard<'_> {
    fn drop(&mut self) {
        let mut script = String::new();
        // Sweep generated names that appeared after the guard was
        // created: published files are normally removed by
        // attachment_delete_remote, but a panic can strand a partial.
        for name in remote_attachment_entries(self.fixture) {
            let generated = generated_name_valid(&name) || remote_partial_name_valid(&name);
            if generated && !self.baseline.iter().any(|seen| seen == &name) {
                script.push_str(&format!("rm -f -- '{REMOTE_DEFAULT_DIR}/{name}'; "));
            }
        }
        for path in &self.remote_paths {
            script.push_str(&format!("rm -rf -- \"{path}\"; "));
        }
        // Remove the app-private dirs only when empty (no-op otherwise,
        // and never a force on a shared directory).
        script.push_str(&format!(
            "rmdir -- {REMOTE_DEFAULT_DIR} \"$HOME/.local/share/meeterm\" 2>/dev/null || true"
        ));
        let _ = run_remote_quiet(self.fixture, &script);
    }
}

fn create_fixture_tmux_session(fixture: &FixtureConfig, name: &str) {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(&fixture.tmux_socket)
        .args([
            "new-session",
            "-d",
            "-s",
            name,
            "-n",
            "smoke",
            "/bin/sh",
            "-i",
        ])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("TMUX_TMPDIR", &fixture.tmux_tmpdir)
        .output()
        .expect("create fixture tmux session");
    assert!(
        output.status.success(),
        "create fixture tmux session failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
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
    let mut input = child.stdin.take().expect("desktop attach stdin");
    wait_for_desktop_tmux_client(fixture, &mut child);
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

fn wait_for_desktop_tmux_client(fixture: &FixtureConfig, child: &mut Child) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let command = "tmux list-clients -F '#{client_session}|#{client_control_mode}|#{client_width}|#{client_height}'";
    loop {
        if let Some(status) = child.try_wait().expect("poll ordinary desktop attach") {
            panic!("ordinary desktop attach exited before readiness: {status}");
        }

        let output = ssh_command(fixture, false)
            .arg(command)
            .output()
            .expect("query ordinary desktop tmux client");
        let ready = output.status.success()
            && String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                let mut fields = line.trim().split('|');
                let session = fields.next();
                let control_mode = fields.next();
                let width = fields.next().and_then(|value| value.parse::<u16>().ok());
                let height = fields.next().and_then(|value| value.parse::<u16>().ok());
                session == Some("meeterm")
                    && control_mode == Some("0")
                    && width.is_some_and(|width| width > 0)
                    && height.is_some_and(|height| height > 0)
            });
        if ready {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for ordinary desktop tmux client: status={}, stdout={:?}, stderr={:?}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
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

fn wait_for_snapshot(
    id: u64,
    label: &str,
    mut predicate: impl FnMut(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = session_snapshot(id).expect("tmux session snapshot");
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
                "timed out waiting for {label}: state={}, errorCode={}, windows={:?}",
                state_name(connection.state),
                connection_string(&connection.error_code, connection.error_code_len),
                snapshot
                    .windows
                    .iter()
                    .map(|window| (window.window_id, window.name.clone()))
                    .collect::<Vec<_>>()
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_runtime_picker(
    id: u64,
    expected_fingerprint: &str,
    label: &str,
) -> RuntimeDiscoverySnapshot {
    wait_for_runtime_picker_with_optional_host_key(id, Some(expected_fingerprint), label)
}

fn wait_for_runtime_picker_without_prompt(id: u64, label: &str) -> RuntimeDiscoverySnapshot {
    wait_for_runtime_picker_with_optional_host_key(id, None, label)
}

fn wait_for_runtime_picker_with_optional_host_key(
    id: u64,
    expected_fingerprint: Option<&str>,
    label: &str,
) -> RuntimeDiscoverySnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut answered = expected_fingerprint.is_none();
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        if snapshot.state == ConnectionState::HostKeyPending as u32 && !answered {
            let expected_fingerprint =
                expected_fingerprint.expect("host-key prompt must have an expected fingerprint");
            let fingerprint = connection_string(&snapshot.fingerprint, snapshot.fingerprint_len);
            assert_eq!(
                fingerprint, expected_fingerprint,
                "{label} host-key fingerprint"
            );
            respond_to_host_key_ffi(id, expected_fingerprint, true);
            answered = true;
        }
        if snapshot.state == ConnectionState::AwaitingRuntimeSelection as u32 {
            return runtime_discovery_snapshot(id).expect("runtime discovery snapshot");
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
                "timed out waiting for {label}: state={}",
                state_name(snapshot.state)
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn assert_empty_tmux_and_independent_herdr(discovery: &RuntimeDiscoverySnapshot, label: &str) {
    assert_eq!(
        discovery.tmux.state,
        RuntimeSectionState::Empty,
        "{label} must report an empty tmux section: code={:?}, message={:?}",
        discovery.tmux.error_code,
        discovery.tmux.error_message
    );
    assert!(
        discovery.tmux.candidates.is_empty(),
        "{label} empty tmux section must not contain candidates"
    );
    assert!(
        discovery.tmux.error_code.is_none() && discovery.tmux.error_message.is_none(),
        "{label} empty tmux discovery must not contain an error"
    );

    assert_ne!(
        discovery.herdr.state,
        RuntimeSectionState::Loading,
        "{label} must publish an independent Herdr result"
    );
    assert!(
        discovery
            .herdr
            .candidates
            .iter()
            .all(|candidate| candidate.backend == Backend::Herdr),
        "{label} Herdr candidates must remain in the Herdr section"
    );
    match &discovery.herdr.state {
        RuntimeSectionState::Loading => unreachable!("loading was rejected above"),
        RuntimeSectionState::Success => assert!(
            !discovery.herdr.candidates.is_empty(),
            "{label} successful Herdr discovery must contain candidates"
        ),
        RuntimeSectionState::Empty => assert!(
            discovery.herdr.candidates.is_empty(),
            "{label} empty Herdr discovery must not contain candidates"
        ),
        RuntimeSectionState::Error => {
            assert!(
                discovery.herdr.candidates.is_empty(),
                "{label} failed Herdr discovery must not contain candidates"
            );
            assert!(
                discovery.herdr.error_code.is_some() && discovery.herdr.error_message.is_some(),
                "{label} failed Herdr discovery must include bounded error details"
            );
        }
    }
}

fn create_tmux_meeterm_from_picker(
    id: u64,
    discovery: &RuntimeDiscoverySnapshot,
    label: &str,
) -> ConnectionSnapshot {
    assert_empty_tmux_and_independent_herdr(discovery, label);
    create_runtime(id, Backend::Tmux, "meeterm").expect("create explicit tmux meeterm runtime");
    wait_for_ready_without_prompt(id, label)
}

fn select_tmux_meeterm(id: u64, expected_fingerprint: &str, label: &str) {
    let discovery = wait_for_runtime_picker(id, expected_fingerprint, label);
    let candidates = discovery
        .tmux
        .candidates
        .iter()
        .filter(|candidate| candidate.name == "meeterm")
        .collect::<Vec<_>>();
    assert_eq!(
        candidates.len(),
        1,
        "{label} must expose one meeterm candidate"
    );
    let candidate = candidates[0];
    assert_eq!(candidate.backend, Backend::Tmux);
    assert_eq!(candidate.state, RuntimeState::Running);
    assert!(
        candidate.selectable,
        "{label} meeterm candidate is not selectable"
    );
    let candidate_id = candidate.id.clone();
    select_runtime(id, &candidate_id).expect("select tmux meeterm runtime");
    wait_for_ready_without_prompt(id, label);
}

fn connect_host_and_select_meeterm(id: u64, fixture: &FixtureConfig, label: &str) {
    connect_host(id, fixture.options()).expect("start SSH host connection");
    select_tmux_meeterm(id, &fixture.fingerprint, label);
}

fn reconnect_and_select_meeterm(id: u64, expected_fingerprint: &str, label: &str) {
    reconnect_terminal(id).expect("start manual SSH reconnect");
    select_tmux_meeterm(id, expected_fingerprint, label);
}

fn reconnect_and_create_meeterm(
    id: u64,
    expected_fingerprint: &str,
    label: &str,
) -> ConnectionSnapshot {
    reconnect_terminal(id).expect("start manual SSH reconnect for explicit create");
    let discovery = wait_for_runtime_picker(id, expected_fingerprint, label);
    create_tmux_meeterm_from_picker(id, &discovery, label)
}

fn wait_for_pane_handle(id: u64, pane_id: u64, label: &str) -> PaneSnapshot {
    wait_for_snapshot(id, label, |snapshot| {
        snapshot.panes.iter().any(|pane| pane.pane_id == pane_id)
    })
    .panes
    .into_iter()
    .find(|pane| pane.pane_id == pane_id)
    .expect("pane handle after runtime selection")
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
            let connection = connection_snapshot(id).expect("connection snapshot");
            panic!(
                "timed out waiting for {label}: selected={:?}, wanted=%{pane_id}, state={}, errorCode={}, errorMessage={}",
                snapshot.selected_pane,
                state_name(connection.state),
                connection_string(&connection.error_code, connection.error_code_len),
                connection_string(&connection.error_message, connection.error_message_len),
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

fn remote_tmux_layout(fixture: &FixtureConfig, label: &str) -> RemoteTmuxLayout {
    // Keep these observations read-only. `window_layout` is the saved layout
    // even while a pane is zoomed; never replace it with a visible-pane size or
    // unzoom/rezoom as part of measurement.
    let windows_output = run_remote_tmux(
        fixture,
        "tmux list-windows -t '=meeterm:' -F '#{window_id}|#{window_layout}|#{window_width}|#{window_height}'",
        &format!("{label}: windows"),
    );
    let panes_output = run_remote_tmux(
        fixture,
        "tmux list-panes -s -t '=meeterm:' -F '#{window_id}|#{pane_id}|#{pane_index}|#{pane_pid}|#{pane_active}|#{window_active}|#{window_zoomed_flag}'",
        &format!("{label}: panes"),
    );
    let layout = RemoteTmuxLayout {
        windows: parse_remote_windows(&windows_output.stdout, label),
        panes: parse_remote_panes(&panes_output.stdout, label),
    };
    validate_remote_tmux_layout(&layout, label);
    layout
}

fn parse_remote_windows(bytes: &[u8], label: &str) -> Vec<RemoteWindowLayout> {
    let output = std::str::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("{label}: window query was not UTF-8: {error}"));
    let mut windows = output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields = line.split('|').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                4,
                "{label}: window record must have four pipe-delimited fields: {line:?}"
            );
            let saved_layout = fields[1].to_owned();
            RemoteWindowLayout {
                window_id: parse_prefixed_number(fields[0], '@', label, "window ID"),
                shape: parse_saved_layout(&saved_layout, label),
                saved_layout,
                width: parse_number(fields[2], label, "window width")
                    .try_into()
                    .unwrap_or_else(|_| panic!("{label}: window width does not fit u16")),
                height: parse_number(fields[3], label, "window height")
                    .try_into()
                    .unwrap_or_else(|_| panic!("{label}: window height does not fit u16")),
            }
        })
        .collect::<Vec<_>>();
    assert!(!windows.is_empty(), "{label}: tmux returned no windows");
    windows.sort_by_key(|window| window.window_id);
    windows
}

fn parse_remote_panes(bytes: &[u8], label: &str) -> Vec<RemotePaneState> {
    let output = std::str::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("{label}: pane query was not UTF-8: {error}"));
    let mut panes = output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields = line.split('|').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                7,
                "{label}: pane record must have seven pipe-delimited fields: {line:?}"
            );
            RemotePaneState {
                window_id: parse_prefixed_number(fields[0], '@', label, "pane window ID"),
                pane_id: parse_prefixed_number(fields[1], '%', label, "pane ID"),
                index: parse_number(fields[2], label, "pane index")
                    .try_into()
                    .unwrap_or_else(|_| panic!("{label}: pane index does not fit u32")),
                pid: parse_number(fields[3], label, "pane PID")
                    .try_into()
                    .unwrap_or_else(|_| panic!("{label}: pane PID does not fit u32")),
                active: parse_binary_flag(fields[4], label, "pane active"),
                window_active: parse_binary_flag(fields[5], label, "window active"),
                zoomed: parse_binary_flag(fields[6], label, "window zoom flag"),
            }
        })
        .collect::<Vec<_>>();
    assert!(!panes.is_empty(), "{label}: tmux returned no panes");
    panes.sort_by_key(|pane| (pane.window_id, pane.index, pane.pane_id));
    panes
}

fn parse_prefixed_number(value: &str, prefix: char, label: &str, field: &str) -> u64 {
    let digits = value
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("{label}: {field} must start with {prefix:?}: {value:?}"));
    parse_number(digits, label, field)
}

fn parse_number(value: &str, label: &str, field: &str) -> u64 {
    assert!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "{label}: invalid {field}: {value:?}"
    );
    value
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("{label}: {field} is out of range: {value:?}"))
}

fn parse_binary_flag(value: &str, label: &str, field: &str) -> bool {
    match value {
        "0" => false,
        "1" => true,
        _ => panic!("{label}: invalid {field}: {value:?}"),
    }
}

fn validate_remote_tmux_layout(layout: &RemoteTmuxLayout, label: &str) {
    let window_ids = layout
        .windows
        .iter()
        .map(|window| window.window_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        window_ids.len(),
        layout.windows.len(),
        "{label}: duplicate window IDs"
    );

    let pane_ids = layout
        .panes
        .iter()
        .map(|pane| pane.pane_id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        pane_ids.len(),
        layout.panes.len(),
        "{label}: duplicate pane IDs"
    );

    for pane in &layout.panes {
        assert!(
            window_ids.contains(&pane.window_id),
            "{label}: pane %{0} refers to missing window @{1}",
            pane.pane_id,
            pane.window_id
        );
    }

    for window in &layout.windows {
        let mut saved_panes = Vec::new();
        collect_layout_pane_ids(&window.shape, &mut saved_panes);
        let mut observed_panes = layout
            .panes
            .iter()
            .filter(|pane| pane.window_id == window.window_id)
            .map(|pane| pane.pane_id)
            .collect::<Vec<_>>();
        saved_panes.sort_unstable();
        observed_panes.sort_unstable();
        assert_eq!(
            saved_panes, observed_panes,
            "{label}: saved layout pane IDs do not match separately fetched panes in window @{}",
            window.window_id
        );
    }
}

fn collect_layout_pane_ids(shape: &LayoutShape, pane_ids: &mut Vec<u64>) {
    match shape {
        LayoutShape::Leaf(pane_id) => pane_ids.push(*pane_id),
        LayoutShape::Split { children, .. } => {
            for child in children {
                collect_layout_pane_ids(child, pane_ids);
            }
        }
    }
}

fn layout_shape_signature(layout: &RemoteTmuxLayout) -> Vec<(u64, LayoutShape)> {
    layout
        .windows
        .iter()
        .map(|window| (window.window_id, window.shape.clone()))
        .collect()
}

fn pane_identity_signature(layout: &RemoteTmuxLayout) -> Vec<(u64, u64, u32, u32)> {
    let mut identity = layout
        .panes
        .iter()
        .map(|pane| (pane.window_id, pane.pane_id, pane.index, pane.pid))
        .collect::<Vec<_>>();
    identity.sort_unstable();
    identity
}

fn window_dimensions_signature(layout: &RemoteTmuxLayout) -> Vec<(u64, u16, u16)> {
    layout
        .windows
        .iter()
        .map(|window| (window.window_id, window.width, window.height))
        .collect()
}

fn saved_layout_signature(layout: &RemoteTmuxLayout) -> Vec<(u64, &str)> {
    layout
        .windows
        .iter()
        .map(|window| (window.window_id, window.saved_layout.as_str()))
        .collect()
}

fn assert_remote_layout_preserved(
    before: &RemoteTmuxLayout,
    after: &RemoteTmuxLayout,
    stage: &str,
    fixed_viewport: bool,
) {
    assert_eq!(
        pane_identity_signature(before),
        pane_identity_signature(after),
        "{stage}: window/pane/index/process identity changed"
    );
    assert_eq!(
        layout_shape_signature(before),
        layout_shape_signature(after),
        "{stage}: saved split direction/nesting/order/pane placement changed"
    );
    if fixed_viewport {
        assert_eq!(
            window_dimensions_signature(before),
            window_dimensions_signature(after),
            "{stage}: fixed-viewport window dimensions changed"
        );
        assert_eq!(
            saved_layout_signature(before),
            saved_layout_signature(after),
            "{stage}: exact saved layout changed at a fixed viewport"
        );
    }
}

fn assert_zoom_state(layout: &RemoteTmuxLayout, selected_pane: u64, stage: &str) {
    let selected = layout
        .panes
        .iter()
        .find(|pane| pane.pane_id == selected_pane)
        .unwrap_or_else(|| panic!("{stage}: selected pane %{selected_pane} is missing"));
    assert!(
        selected.active,
        "{stage}: selected pane %{selected_pane} is not active in its window"
    );
    assert!(
        selected.window_active,
        "{stage}: selected pane %{selected_pane} is not in the active window"
    );
    let mut zoomed_windows = layout
        .panes
        .iter()
        .filter(|pane| pane.zoomed)
        .map(|pane| pane.window_id)
        .collect::<Vec<_>>();
    zoomed_windows.sort_unstable();
    zoomed_windows.dedup();
    assert_eq!(
        zoomed_windows,
        vec![selected.window_id],
        "{stage}: zoom flags do not identify only the selected window"
    );
}

fn assert_no_zoom(layout: &RemoteTmuxLayout, stage: &str) {
    assert!(
        layout.panes.iter().all(|pane| !pane.zoomed),
        "{stage}: one or more independently fetched window zoom flags remain set"
    );
}

fn assert_zoom_selection_stage(
    fixture: &FixtureConfig,
    baseline: &RemoteTmuxLayout,
    selected_pane: u64,
    fixed_viewport: bool,
    stage: &str,
) {
    let observed = remote_tmux_layout(fixture, stage);
    assert_remote_layout_preserved(baseline, &observed, stage, fixed_viewport);
    assert_zoom_state(&observed, selected_pane, stage);
}

struct LayoutParser<'a> {
    input: &'a [u8],
    position: usize,
    stage: &'a str,
}

fn parse_saved_layout(layout: &str, stage: &str) -> LayoutShape {
    let (checksum, body) = layout.split_once(',').unwrap_or_else(|| {
        panic!("{stage}: saved layout is missing checksum separator: {layout:?}")
    });
    assert_eq!(
        checksum.len(),
        4,
        "{stage}: saved layout checksum must have four hex digits: {layout:?}"
    );
    assert!(
        checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{stage}: saved layout checksum is not hexadecimal: {layout:?}"
    );
    assert!(!body.is_empty(), "{stage}: saved layout body is empty");
    let mut parser = LayoutParser {
        input: body.as_bytes(),
        position: 0,
        stage,
    };
    let shape = parser.parse_node();
    assert_eq!(
        parser.position,
        parser.input.len(),
        "{stage}: saved layout has trailing data at byte {}: {layout:?}",
        parser.position
    );
    shape
}

impl LayoutParser<'_> {
    fn parse_node(&mut self) -> LayoutShape {
        let width = self.parse_number_until(b'x', "layout width");
        let height = self.parse_number_until(b',', "layout height");
        let _x = self.parse_number_until(b',', "layout x offset");
        let _y = self.parse_number();
        assert!(
            width > 0 && height > 0,
            "{}: saved layout node has zero dimensions",
            self.stage
        );
        match self.peek() {
            Some(b'[') => self.parse_split(b'[', b']', LayoutSplitDirection::Vertical),
            Some(b'{') => self.parse_split(b'{', b'}', LayoutSplitDirection::Horizontal),
            Some(b',') => {
                self.position += 1;
                let pane_id = self.parse_number();
                assert!(
                    matches!(self.peek(), None | Some(b',') | Some(b']') | Some(b'}')),
                    "{}: invalid saved layout leaf delimiter",
                    self.stage
                );
                LayoutShape::Leaf(pane_id)
            }
            other => panic!(
                "{}: saved layout node must end in a split or pane ID, got {other:?}",
                self.stage
            ),
        }
    }

    fn parse_split(&mut self, open: u8, close: u8, direction: LayoutSplitDirection) -> LayoutShape {
        assert_eq!(
            self.peek(),
            Some(open),
            "{}: missing split opener",
            self.stage
        );
        self.position += 1;
        let mut children = Vec::new();
        loop {
            assert_ne!(
                self.peek(),
                Some(close),
                "{}: saved layout split has no child",
                self.stage
            );
            children.push(self.parse_node());
            match self.peek() {
                Some(b',') => {
                    self.position += 1;
                    assert_ne!(
                        self.peek(),
                        Some(close),
                        "{}: saved layout split has a trailing separator",
                        self.stage
                    );
                }
                Some(value) if value == close => {
                    self.position += 1;
                    break;
                }
                other => panic!(
                    "{}: saved layout split has invalid child delimiter {other:?}",
                    self.stage
                ),
            }
        }
        assert!(
            children.len() >= 2,
            "{}: saved layout split must have at least two children",
            self.stage
        );
        LayoutShape::Split {
            direction,
            children,
        }
    }

    fn parse_number_until(&mut self, terminator: u8, field: &str) -> u32 {
        let value = self.parse_number();
        assert_eq!(
            self.peek(),
            Some(terminator),
            "{}: saved layout {field} is missing delimiter {:?}",
            self.stage,
            terminator as char
        );
        self.position += 1;
        value
            .try_into()
            .unwrap_or_else(|_| panic!("{}: saved layout {field} does not fit u32", self.stage))
    }

    fn parse_number(&mut self) -> u64 {
        let start = self.position;
        while matches!(self.input.get(self.position), Some(byte) if byte.is_ascii_digit()) {
            self.position += 1;
        }
        assert_ne!(
            start, self.position,
            "{}: saved layout expected decimal digits at byte {}",
            self.stage, start
        );
        std::str::from_utf8(&self.input[start..self.position])
            .expect("ASCII layout digits")
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("{}: saved layout number is out of range", self.stage))
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }
}

fn exercise_zoom_switch_sequence(
    id: u64,
    fixture: &FixtureConfig,
    main: &PaneSnapshot,
    side: &PaneSnapshot,
    side_active: &PaneSnapshot,
    baseline: &RemoteTmuxLayout,
    label: &str,
) {
    select_pane(id, side.pane_id).expect("select zoom sequence side pane");
    wait_for_selected_pane(id, side.pane_id, "select zoom sequence side pane");
    wait_for_remote_tmux(
        fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        &format!("{label}: initial mobile zoom"),
        |output| output.trim() == "1",
    );
    assert_zoom_selection_stage(
        fixture,
        baseline,
        side.pane_id,
        true,
        &format!("{label}: selection_side_complete"),
    );

    // A pane switch inside the owned window must retain ownership and zoom.
    select_pane(id, side_active.pane_id).expect("select same-window zoom sequence pane");
    wait_for_selected_pane(
        id,
        side_active.pane_id,
        "select same-window zoom sequence pane",
    );
    wait_for_remote_tmux(
        fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        &format!("{label}: same-window zoom ownership"),
        |output| output.trim() == "1",
    );
    assert_zoom_selection_stage(
        fixture,
        baseline,
        side_active.pane_id,
        true,
        &format!("{label}: selection_same_window_complete"),
    );

    // Switching windows transfers mobile ownership without changing the
    // underlying split shape; the old window must return to desktop layout.
    select_pane(id, main.pane_id).expect("select other-window zoom sequence pane");
    wait_for_selected_pane(id, main.pane_id, "select other-window zoom sequence pane");
    wait_for_remote_tmux(
        fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            main.window_id
        ),
        &format!("{label}: other-window zoom ownership"),
        |output| output.trim() == "1",
    );
    wait_for_remote_tmux(
        fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        &format!("{label}: old window restored"),
        |output| output.trim() == "0",
    );
    assert_zoom_selection_stage(
        fixture,
        baseline,
        main.pane_id,
        true,
        &format!("{label}: selection_other_window_complete"),
    );

    select_pane(id, side_active.pane_id).expect("select return zoom sequence pane");
    wait_for_selected_pane(id, side_active.pane_id, "select return zoom sequence pane");
    wait_for_remote_tmux(
        fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            side.window_id
        ),
        &format!("{label}: returned window zoom ownership"),
        |output| output.trim() == "1",
    );
    wait_for_remote_tmux(
        fixture,
        &format!(
            "tmux display-message -p -t @{} '#{{window_zoomed_flag}}'",
            main.window_id
        ),
        &format!("{label}: returned old window restored"),
        |output| output.trim() == "0",
    );
    assert_zoom_selection_stage(
        fixture,
        baseline,
        side_active.pane_id,
        true,
        &format!("{label}: selection_return_complete"),
    );
}

#[test]
fn tmux_saved_layout_shape_comparison_ignores_resize_but_detects_structure_and_ratio_changes() {
    let baseline = "abcd,80x24,0,0{39x24,0,0,0,40x24,40,0,1}";
    let resized = "beef,60x18,0,0{29x18,0,0,0,30x18,30,0,1}";
    let swapped = "cafe,80x24,0,0{39x24,0,0,1,40x24,40,0,0}";
    let vertical = "d00d,80x24,0,0[39x12,0,0,0,40x11,0,13,1]";
    let same_size_ratio_change = "face,80x24,0,0{30x24,0,0,0,49x24,31,0,1}";
    let nested = "1234,120x24,0,0{59x24,0,0{29x24,0,0,0,29x24,30,0,1},60x24,60,0,2}";
    let flattened = "5678,120x24,0,0{29x24,0,0,0,29x24,30,0,1,60x24,60,0,2}";

    assert_eq!(
        parse_saved_layout(baseline, "layout comparison baseline"),
        parse_saved_layout(resized, "layout comparison resized"),
        "a viewport resize must not change the saved split relation"
    );
    assert_ne!(
        parse_saved_layout(baseline, "layout comparison baseline"),
        parse_saved_layout(swapped, "layout comparison pane placement"),
        "a pane placement change must be detected"
    );
    assert_ne!(
        parse_saved_layout(baseline, "layout comparison baseline"),
        parse_saved_layout(vertical, "layout comparison direction"),
        "a split direction change must be detected"
    );
    assert_ne!(
        parse_saved_layout(nested, "layout comparison nested"),
        parse_saved_layout(flattened, "layout comparison flattened"),
        "a split nesting change must be detected"
    );
    assert_eq!(
        parse_saved_layout(baseline, "layout comparison baseline"),
        parse_saved_layout(same_size_ratio_change, "layout comparison ratio"),
        "the relation-only comparison must ignore dimensions"
    );
    assert_ne!(
        baseline, same_size_ratio_change,
        "the fixed-viewport exact saved-layout comparison must detect a ratio change"
    );
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

fn wait_for_reconnecting(id: u64, label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        if snapshot.state == ConnectionState::Reconnecting as u32 {
            return;
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
                "timed out waiting for {label}: state={}",
                state_name(snapshot.state)
            );
        }
        sleep(POLL_INTERVAL);
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
                "timed out waiting for {label}: state={}, errorCode={}, discovery={:?}",
                state_name(snapshot.state),
                connection_string(&snapshot.error_code, snapshot.error_code_len),
                runtime_discovery_snapshot(id).ok()
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
                "timed out waiting for {label}: state={}, errorCode={}, {}",
                state_name(snapshot.state),
                connection_string(&snapshot.error_code, snapshot.error_code_len),
                runtime_discovery_diagnostic(id)
            );
        }
        sleep(POLL_INTERVAL);
    }
}

fn runtime_discovery_diagnostic(id: u64) -> String {
    let Ok(discovery) = runtime_discovery_snapshot(id) else {
        return "runtimeDiscovery=unavailable".to_owned();
    };
    format!(
        "runtimeDiscoveryRevision={}, tmuxState={:?}, tmuxCandidates={}, tmuxErrorCode={:?}, herdrState={:?}, herdrCandidates={}, herdrErrorCode={:?}",
        discovery.discovery_revision,
        discovery.tmux.state,
        discovery.tmux.candidates.len(),
        discovery.tmux.error_code,
        discovery.herdr.state,
        discovery.herdr.candidates.len(),
        discovery.herdr.error_code,
    )
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
        value if value == ConnectionState::DiscoveringRuntimes as u32 => "DiscoveringRuntimes",
        value if value == ConnectionState::AwaitingRuntimeSelection as u32 => {
            "AwaitingRuntimeSelection"
        }
        value if value == ConnectionState::AttachingRuntime as u32 => "AttachingRuntime",
        value if value == ConnectionState::CreatingRuntime as u32 => "CreatingRuntime",
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
