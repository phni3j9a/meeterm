//! Bounded live coverage for the production Herdr backend.
//!
//! The test is ignored by default. The Python driver starts only disposable
//! Herdr 0.9.0 processes; this file starts a test-only russh endpoint which
//! implements the exact SSH exec and direct-streamlocal operations used by
//! production. It does not use OpenSSH streamlocal forwarding, whose local
//! privilege rules reject this unprivileged fixture.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use meeterm_core::workspace::Backend;
use meeterm_core::{
    AuthOptions, ConnectOptions, ConnectionSnapshot, ConnectionState, SessionSnapshot, SpecialKey,
    close_group, close_pane, close_workspace, connect_terminal, connection_snapshot, create_group,
    create_pane, create_terminal, create_workspace, destroy_terminal, disconnect_terminal,
    meeterm_commit_utf8, meeterm_paste_utf8, meeterm_resize_terminal, meeterm_respond_host_key,
    meeterm_scroll_lines, meeterm_send_special_key, meeterm_set_terminal_visible, meeterm_snapshot,
    meeterm_snapshot_size, reconnect_terminal, rename_group, rename_pane, rename_workspace,
    select_group, select_pane, session_snapshot, set_foreground, terminal_revision,
    workspace_snapshot_json,
};
use russh::keys;
use russh::server::{self, Auth, ChannelOpenHandle, Handler, Msg, Server as RusshServer, Session};
use russh::{Channel, ChannelId};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, UnixStream};

const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SNAPSHOT_HEADER_SIZE: usize = 28;
const SNAPSHOT_CELL_METADATA_SIZE: usize = 28;
const MAX_EXEC_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct SessionManifest {
    socket: String,
    pane_id: String,
    terminal_id: String,
}

#[derive(Clone, Debug, Deserialize)]
struct FixtureManifest {
    binary: String,
    root: String,
    host_key: String,
    client_key: String,
    environment: HashMap<String, String>,
    sessions: HashMap<String, SessionManifest>,
}

struct Driver {
    base: PathBuf,
    child: Option<Child>,
    manifest: FixtureManifest,
}

impl Driver {
    fn start() -> Self {
        // AF_UNIX sun_path is limited to roughly 108 bytes.  Keep the random
        // private fixture root short enough for Herdr's named-runtime socket.
        let base = unique_directory("mh");
        let root = base.join("fixture");
        let manifest_path = base.join("manifest.json");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/herdr/native_integration.py");
        let mut command = Command::new(env::var_os("PYTHON").unwrap_or_else(|| "python3".into()));
        command
            .arg(script)
            .arg("--serve")
            .arg("--root")
            .arg(&root)
            .arg("--manifest")
            .arg(&manifest_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(base.join("driver.log")).expect("create driver log"));
        if let Some(binary) = env::var_os("MEETERM_HERDR_BINARY") {
            command.arg("--herdr").arg(binary);
        }
        let mut child = command.spawn().expect("start isolated Herdr driver");
        let stdout = child.stdout.take().expect("driver stdout");
        let (line_sender, line_receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let result = reader.read_line(&mut line).map(|_| line);
            let _ = line_sender.send(result);
        });
        let line = line_receiver
            .recv_timeout(WAIT_TIMEOUT)
            .expect("bounded wait for Herdr driver")
            .expect("read Herdr driver readiness");
        assert!(
            line.starts_with("READY "),
            "Herdr driver did not become ready: {line:?}"
        );
        let manifest = read_json_file::<FixtureManifest>(&manifest_path);
        assert_eq!(
            manifest.sessions.len(),
            2,
            "default and named fixture runtimes"
        );
        Self {
            base,
            child: Some(child),
            manifest,
        }
    }

    fn cli(&self, session: &str, args: &[&str]) -> Value {
        let output = Command::new(&self.manifest.binary)
            .env_clear()
            .envs(&self.manifest.environment)
            .arg("--session")
            .arg(session)
            .args(args)
            .output()
            .expect("run isolated Herdr CLI");
        assert!(
            output.status.success(),
            "fixture CLI failed for {}: exit {:?}",
            args.first().unwrap_or(&""),
            output.status.code()
        );
        serde_json::from_slice(&output.stdout).expect("fixture CLI JSON response")
    }

    fn api(&self, runtime: &str, method: &str, params: Value) -> Value {
        let mut socket =
            std::os::unix::net::UnixStream::connect(&self.manifest.sessions[runtime].socket)
                .expect("fixture API socket");
        socket.set_read_timeout(Some(WAIT_TIMEOUT)).unwrap();
        socket.set_write_timeout(Some(WAIT_TIMEOUT)).unwrap();
        let mut request =
            serde_json::to_vec(&json!({"id":"fixture-external", "method":method, "params":params}))
                .unwrap();
        request.push(b'\n');
        socket.write_all(&request).unwrap();
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], "fixture-external");
        assert!(
            response.get("error").is_none(),
            "fixture external API {method} rejected: {}",
            response["error"]["code"]
        );
        response["result"].clone()
    }

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"stop\n");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(_) => break,
            }
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn unique_directory(prefix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let base = env::temp_dir().join(format!("{prefix}-{}-{stamp}", std::process::id()));
    fs::create_dir(&base).expect("create private fixture directory");
    base
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    let bytes = fs::read(path).expect("read fixture manifest");
    serde_json::from_slice(&bytes).expect("decode fixture manifest")
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

#[derive(Clone)]
struct RusshState {
    binary: PathBuf,
    environment: HashMap<String, String>,
    sockets: HashSet<PathBuf>,
    runtimes: HashSet<String>,
    targets: HashSet<String>,
    clients: Arc<std::sync::Mutex<Vec<server::Handle>>>,
}

struct FixtureSsh {
    handle: server::RunningServerHandle,
    join: Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
    address: SocketAddr,
    clients: Arc<std::sync::Mutex<Vec<server::Handle>>>,
}

impl FixtureSsh {
    fn start(manifest: &FixtureManifest) -> Self {
        let host_key = keys::load_secret_key(&manifest.host_key, None).expect("fixture host key");
        let clients = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = Arc::new(RusshState {
            clients: Arc::clone(&clients),
            binary: PathBuf::from(&manifest.binary),
            environment: manifest.environment.clone(),
            sockets: manifest
                .sessions
                .values()
                .map(|session| PathBuf::from(&session.socket))
                .collect(),
            runtimes: manifest.sessions.keys().cloned().collect(),
            targets: manifest
                .sessions
                .values()
                .map(|session| session.terminal_id.clone())
                .collect(),
        });
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("russh test runtime");
            runtime.block_on(async move {
                let listener = TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("bind russh fixture");
                let address = listener.local_addr().expect("russh fixture address");
                let config = Arc::new(server::Config {
                    keys: vec![host_key],
                    auth_rejection_time: Duration::from_millis(0),
                    auth_rejection_time_initial: Some(Duration::from_millis(0)),
                    ..Default::default()
                });
                let mut server = FixtureServer {
                    state,
                    channels: HashMap::new(),
                    registered: false,
                };
                let running = server.run_on_socket(config, &listener);
                let handle = running.handle();
                ready_sender
                    .send((address, handle))
                    .expect("publish russh fixture");
                let _ = running.await;
            });
        });
        let (address, handle) = ready_receiver
            .recv_timeout(WAIT_TIMEOUT)
            .expect("russh fixture startup");
        Self {
            handle,
            join: Arc::new(std::sync::Mutex::new(Some(join))),
            address,
            clients,
        }
    }

    fn lose_connections(&self) {
        let clients = std::mem::take(&mut *self.clients.lock().unwrap());
        assert!(
            !clients.is_empty(),
            "at least one live SSH connection was registered"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for client in clients {
                let _ = client
                    .disconnect(
                        russh::Disconnect::ByApplication,
                        "fixture connection loss".into(),
                        "en".into(),
                    )
                    .await;
            }
        });
    }
}

impl Drop for FixtureSsh {
    fn drop(&mut self) {
        self.handle.shutdown("test complete".to_owned());
        if let Ok(mut join) = self.join.lock()
            && let Some(join) = join.take()
        {
            let _ = join.join();
        }
    }
}

struct FixtureServer {
    state: Arc<RusshState>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    registered: bool,
}

impl RusshServer for FixtureServer {
    type Handler = Self;

    fn new_client(&mut self, _peer: Option<SocketAddr>) -> Self::Handler {
        Self {
            state: Arc::clone(&self.state),
            channels: HashMap::new(),
            registered: false,
        }
    }
}

impl Handler for FixtureServer {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.registered {
            self.state.clients.lock().unwrap().push(session.handle());
            self.registered = true;
        }
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        let Some(parsed) = parse_exec_command(&command, &self.state) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let channel_object = self.channels.remove(&channel);
        session.channel_success(channel)?;
        let handle = session.handle();
        match parsed {
            ExecCommand::Status { runtime } => {
                let state = Arc::clone(&self.state);
                tokio::spawn(async move {
                    let result = tokio::process::Command::new(&state.binary)
                        .env_clear()
                        .envs(&state.environment)
                        .args(["--session", &runtime, "status", "--json"])
                        .output()
                        .await;
                    if let Ok(output) = result {
                        let status = output.status.code().unwrap_or(1).max(0) as u32;
                        if output.stdout.len() <= MAX_EXEC_OUTPUT {
                            let _ = handle.data(channel, output.stdout).await;
                        }
                        let _ = handle.exit_status_request(channel, status).await;
                        let _ = handle.eof(channel).await;
                        let _ = handle.close(channel).await;
                    } else {
                        let _ = handle.exit_status_request(channel, 1).await;
                        let _ = handle.close(channel).await;
                    }
                });
            }
            ExecCommand::Control {
                runtime,
                target,
                cols,
                rows,
            } => {
                let Some(channel_object) = channel_object else {
                    let _ = handle.close(channel).await;
                    return Ok(());
                };
                let state = Arc::clone(&self.state);
                tokio::spawn(async move {
                    let mut command = tokio::process::Command::new(&state.binary);
                    command
                        .env_clear()
                        .envs(&state.environment)
                        .args([
                            "--session".to_owned(),
                            runtime.clone(),
                            "terminal".to_owned(),
                            "session".to_owned(),
                            "control".to_owned(),
                            target.clone(),
                            "--cols".to_owned(),
                            cols.to_string(),
                            "--rows".to_owned(),
                            rows.to_string(),
                        ])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .kill_on_drop(true);
                    let mut child = match command.spawn() {
                        Ok(child) => child,
                        Err(_) => {
                            let _ = handle.close(channel).await;
                            return;
                        }
                    };
                    let Some(mut child_stdin) = child.stdin.take() else {
                        let _ = handle.close(channel).await;
                        return;
                    };
                    let Some(mut child_stdout) = child.stdout.take() else {
                        let _ = handle.close(channel).await;
                        return;
                    };
                    let channel_id = channel_object.id();
                    let channel = channel_object.into_stream();
                    let (mut ssh_read, mut ssh_write) = tokio::io::split(channel);
                    let input = tokio::spawn(async move {
                        tokio::io::copy(&mut ssh_read, &mut child_stdin).await
                    });
                    let output = tokio::spawn(async move {
                        tokio::io::copy(&mut child_stdout, &mut ssh_write).await
                    });
                    let status = child.wait().await;
                    input.abort();
                    let _ = tokio::time::timeout(Duration::from_secs(1), output).await;
                    let code = status.ok().and_then(|status| status.code()).unwrap_or(1) as u32;
                    let _ = handle.exit_status_request(channel_id, code).await;
                    let _ = handle.eof(channel_id).await;
                    let _ = handle.close(channel_id).await;
                });
            }
        }
        Ok(())
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.state.sockets.contains(Path::new(socket_path)) {
            return Ok(());
        }
        let Ok(unix) = UnixStream::connect(socket_path).await else {
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut ssh = channel.into_stream();
            let mut unix = unix;
            let _ = copy_bidirectional(&mut ssh, &mut unix).await;
        });
        Ok(())
    }
}

enum ExecCommand {
    Status {
        runtime: String,
    },
    Control {
        runtime: String,
        target: String,
        cols: u16,
        rows: u16,
    },
}

fn parse_exec_command(command: &str, state: &RusshState) -> Option<ExecCommand> {
    let words = command.split_whitespace().collect::<Vec<_>>();
    if words.len() < 5 || words[0] != "herdr" || words[1] != "--session" {
        return None;
    }
    let runtime = words[2].to_owned();
    if !state.runtimes.contains(&runtime) {
        return None;
    }
    if words[3..] == ["status", "--json"] {
        return Some(ExecCommand::Status { runtime });
    }
    if words.len() != 11 || words[3..6] != ["terminal", "session", "control"] {
        return None;
    }
    let target = words[6].to_owned();
    // The fixture starts with one root pane, but production exercises real
    // tab/pane creation before opening a controller for the new terminal.
    // Keep rejecting arbitrary target strings while accepting dynamically
    // created Herdr terminal IDs; Herdr itself remains the owner check.
    if (!state.targets.contains(&target) && !is_herdr_terminal_id(&target))
        || words[7] != "--cols"
        || words[9] != "--rows"
    {
        return None;
    }
    let cols = words[8].parse().ok()?;
    let rows = words[10].parse().ok()?;
    if !(1..=4096).contains(&cols) || !(1..=4096).contains(&rows) {
        return None;
    }
    Some(ExecCommand::Control {
        runtime,
        target,
        cols,
        rows,
    })
}

fn is_herdr_terminal_id(target: &str) -> bool {
    target.len() <= 128
        && target.strip_prefix("term_").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn options(manifest: &FixtureManifest, ssh: &FixtureSsh, runtime: Option<&str>) -> ConnectOptions {
    let client_key = fs::read_to_string(&manifest.client_key).expect("fixture client key");
    let host_key = keys::load_secret_key(&manifest.host_key, None).expect("fixture host key");
    let public = host_key
        .public_key()
        .to_openssh()
        .expect("fixture public host key");
    let known_hosts = PathBuf::from(&manifest.root).join("known_hosts");
    fs::write(
        &known_hosts,
        format!("[127.0.0.1]:{} {public}\n", ssh.address.port()),
    )
    .expect("write isolated known-hosts file");
    ConnectOptions {
        host: "127.0.0.1".to_owned(),
        port: ssh.address.port(),
        username: "fixture".to_owned(),
        credentials: AuthOptions::public_key(client_key, None),
        known_hosts_path: known_hosts,
        backend: Backend::Herdr,
        runtime: runtime.map(str::to_owned),
    }
}

// The complete marker must not occur in the command's terminal echo.
fn marker_command(marker: &str) -> String {
    let (left, right) = marker.split_at(marker.len() / 2);
    format!("printf '%s%s\\n' '{left}' '{right}'\n")
}

fn commit_marker(id: u64, marker: &str, label: &str) {
    let command = marker_command(marker);
    assert!(
        unsafe { meeterm_commit_utf8(id, command.as_ptr(), command.len()) } > 0,
        "{label} input rejected"
    );
    wait_text(id, marker, label);
}

fn entity_id(value: &Value) -> u64 {
    value
        .as_str()
        .expect("entity ID must be an opaque string")
        .parse()
        .expect("native entity ID")
}

fn wait_state(id: u64, wanted: ConnectionState, label: &str) -> ConnectionSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        if snapshot.state == wanted as u32 {
            return snapshot;
        }
        if snapshot.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: {} {}",
                field(&snapshot.error_code, snapshot.error_code_len),
                field(&snapshot.error_message, snapshot.error_message_len)
            );
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_ready_with_host_key(id: u64, label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut answered = false;
    loop {
        let snapshot = connection_snapshot(id).expect("connection snapshot");
        if snapshot.state == ConnectionState::HostKeyPending as u32 && !answered {
            let fingerprint = field(&snapshot.fingerprint, snapshot.fingerprint_len);
            let bytes = fingerprint.as_bytes();
            let result = unsafe { meeterm_respond_host_key(id, bytes.as_ptr(), bytes.len(), 1) };
            assert_eq!(result, 0, "host-key response");
            answered = true;
        }
        if snapshot.state == ConnectionState::Ready as u32 {
            return;
        }
        if snapshot.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: {} {}",
                field(&snapshot.error_code, snapshot.error_code_len),
                field(&snapshot.error_message, snapshot.error_message_len)
            );
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(POLL_INTERVAL);
    }
}

fn field(bytes: &[u8], length: u16) -> String {
    String::from_utf8(bytes[..usize::from(length)].to_vec()).expect("UTF-8 connection field")
}

fn wait_session(id: u64, label: &str) -> SessionSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = session_snapshot(id).expect("session snapshot");
        if !snapshot.panes.is_empty() && snapshot.selected_pane.is_some() {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_json<F: FnMut(&Value) -> bool>(id: u64, label: &str, mut predicate: F) -> Value {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let state = connection_snapshot(id).expect("connection snapshot");
        if state.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: {} {}",
                field(&state.error_code, state.error_code_len),
                field(&state.error_message, state.error_message_len)
            );
        }
        let value: Value =
            serde_json::from_str(&workspace_snapshot_json(id).expect("workspace JSON"))
                .expect("workspace snapshot JSON");
        if predicate(&value) {
            return value;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {label}; last workspace snapshot: {value}");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_revision(owner: u64, id: u64, previous: u64, label: &str) -> u64 {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let state = connection_snapshot(owner).expect("connection snapshot");
        if state.state == ConnectionState::Failed as u32 {
            panic!(
                "{label} failed: {} {}",
                field(&state.error_code, state.error_code_len),
                field(&state.error_message, state.error_message_len)
            );
        }
        let revision = terminal_revision(id).expect("terminal revision");
        if revision > previous {
            return revision;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label}; state={} revision={revision}/{previous} workspace={}",
            state.state,
            workspace_snapshot_json(owner).unwrap_or_default()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_snapshot(id: u64) -> SnapshotBytes {
    let mut capacity = meeterm_snapshot_size(id);
    assert!(
        capacity >= SNAPSHOT_HEADER_SIZE,
        "native snapshot unavailable"
    );
    for _ in 0..8 {
        let mut bytes = vec![0_u8; capacity];
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

#[derive(Debug)]
struct SnapshotBytes {
    columns: u32,
    rows: u32,
    text: String,
}

fn decode_snapshot(bytes: &[u8]) -> SnapshotBytes {
    assert!(bytes.len() >= SNAPSHOT_HEADER_SIZE);
    assert_eq!(&bytes[..4], b"MTRM");
    let columns = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let rows = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let count = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
    let mut cells = Vec::with_capacity(count);
    let mut offset = SNAPSHOT_HEADER_SIZE;
    for _ in 0..count {
        assert!(offset + SNAPSHOT_CELL_METADATA_SIZE <= bytes.len());
        let row = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let column = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let width = bytes[offset + 8];
        let base_len =
            u32::from_le_bytes(bytes[offset + 20..offset + 24].try_into().unwrap()) as usize;
        let combining_len =
            u32::from_le_bytes(bytes[offset + 24..offset + 28].try_into().unwrap()) as usize;
        let start = offset + SNAPSHOT_CELL_METADATA_SIZE;
        let base_end = start + base_len;
        let end = base_end + combining_len;
        assert!(end <= bytes.len());
        let mut text = String::from_utf8(bytes[start..base_end].to_vec()).expect("snapshot UTF-8");
        text.push_str(std::str::from_utf8(&bytes[base_end..end]).expect("combining UTF-8"));
        cells.push((row, column, width, text));
        offset = end;
    }
    assert_eq!(offset, bytes.len());
    let mut lines = (0..rows)
        .map(|_| Vec::<(u32, u8, String)>::new())
        .collect::<Vec<_>>();
    for cell in cells {
        if let Some(line) = lines.get_mut(cell.0 as usize) {
            line.push((cell.1, cell.2, cell.3));
        }
    }
    let mut text = String::new();
    let line_count = lines.len();
    for (index, line) in lines.iter_mut().enumerate() {
        line.sort_by_key(|cell| cell.0);
        let mut next = 0;
        for (column, width, value) in line {
            while next < *column {
                text.push(' ');
                next += 1;
            }
            text.push_str(value);
            next = column.saturating_add(u32::from(*width));
        }
        if index + 1 < line_count {
            text.push('\n');
        }
    }
    SnapshotBytes {
        columns,
        rows,
        text,
    }
}

fn wait_text(id: u64, expected: &str, label: &str) -> SnapshotBytes {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = read_snapshot(id);
        if snapshot.text.contains(expected) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_dimensions(id: u64, columns: u32, rows: u32, label: &str) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let snapshot = read_snapshot(id);
        if snapshot.columns == columns && snapshot.rows == rows {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::sleep(POLL_INTERVAL);
    }
}

fn start_external_control(
    manifest: &FixtureManifest,
    session: &str,
    target: &str,
    takeover: bool,
) -> (Child, Receiver<String>) {
    let mut command = Command::new(&manifest.binary);
    command.env_clear().envs(&manifest.environment).args([
        "--session",
        session,
        "terminal",
        "session",
        "control",
        target,
        "--cols",
        "52",
        "--rows",
        "20",
    ]);
    if takeover {
        command.arg("--takeover");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start external Herdr controller");
    let stdout = child.stdout.take().expect("external controller stdout");
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).map(|_| line);
        let _ = sender.send(result.unwrap_or_default());
        let mut sink = Vec::new();
        let _ = reader.read_to_end(&mut sink);
    });
    (child, receiver)
}

fn stop_child(child: &mut Child) {
    if child.try_wait().expect("poll child").is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn shell_tui(root: &Path) -> PathBuf {
    let path = root.join("native-herdr-tui.py");
    fs::write(
        &path,
        r#"import os,select,sys,termios,tty
fd=0
old=termios.tcgetattr(fd)
def read_exact(size):
    data=b''
    while len(data)<size:
        if not select.select([fd],[],[],10)[0]: raise RuntimeError('input deadline')
        data += os.read(fd,size-len(data))
    return data
try:
    tty.setraw(fd)
    os.write(1,b'\x1b[?1049h\x1b[?1h\x1b[?2004h\x1b[2J\x1b[HDECCKM_READY')
    data=read_exact(3)
    os.write(1,b'\r\nUP_RESULT_'+data.hex().encode())
    data=b''
    while not data.endswith(b'\x1b[201~'):
        if not select.select([fd],[],[],10)[0]: raise RuntimeError('paste deadline')
        data += os.read(fd,64)
        if len(data)>256: raise RuntimeError('paste too large')
    os.write(1,b'\r\nPASTE_RESULT_'+data.hex().encode()+b'\r\nPASTE_RESULT_READY')
    data=read_exact(3)
    os.write(1,b'\r\nRECONNECT_UP_'+data.hex().encode())
    read_exact(1)
finally:
    os.write(1,b'\x1b[?1l\x1b[?2004l\x1b[?1049l')
    termios.tcsetattr(fd,termios.TCSANOW,old)
    os.write(1,b"\r\nTUI_DONE\r\n")
"#,
    )
    .expect("write native TUI fixture");
    path
}

#[test]
#[ignore = "requires MEETERM_HERDR_INTEGRATION=1 and a real Herdr 0.9.0 binary"]
fn real_herdr_native_backend_over_russh_fixture() {
    assert_eq!(
        env::var("MEETERM_HERDR_INTEGRATION").ok().as_deref(),
        Some("1")
    );
    let driver = Driver::start();
    let ssh = FixtureSsh::start(&driver.manifest);

    let default_id = create_terminal(40, 16).expect("create default Herdr terminal");
    let _default_guard = TerminalGuard { id: default_id };
    connect_terminal(default_id, options(&driver.manifest, &ssh, None))
        .expect("connect default Herdr runtime");
    wait_ready_with_host_key(default_id, "default Herdr connection");
    let initial = wait_session(default_id, "default hierarchy");
    let root = initial
        .panes
        .iter()
        .find(|pane| pane.selected)
        .expect("selected default pane")
        .clone();
    let runtime: Value =
        serde_json::from_str(&workspace_snapshot_json(default_id).unwrap()).unwrap();
    assert_eq!(runtime["backend"], "herdr");
    assert_eq!(runtime["groupsSupported"], true);
    assert_eq!(runtime["workspaces"][0]["name"], "default-workspace");
    assert_eq!(
        runtime["terminals"][0]["terminalId"],
        format!("native:{}", root.terminal_id)
    );

    let named_id = create_terminal(40, 16).expect("create named Herdr terminal");
    let _named_guard = TerminalGuard { id: named_id };
    connect_terminal(
        named_id,
        options(&driver.manifest, &ssh, Some("named-probe")),
    )
    .expect("connect named Herdr runtime");
    wait_ready_with_host_key(named_id, "named Herdr connection");
    let named_json: Value =
        serde_json::from_str(&workspace_snapshot_json(named_id).unwrap()).unwrap();
    assert_eq!(named_json["workspaces"][0]["name"], "named-probe-workspace");
    assert_ne!(
        named_json["workspaces"][0]["name"],
        runtime["workspaces"][0]["name"]
    );
    let named_before_disconnect = wait_session(named_id, "named hierarchy before disconnect");
    let named_remote_terminal = named_before_disconnect
        .panes
        .iter()
        .find(|pane| pane.selected)
        .expect("selected named pane")
        .terminal_id;
    assert!(
        select_pane(default_id, named_before_disconnect.selected_pane.unwrap()).is_err(),
        "a named runtime pane must not be selectable in the default runtime"
    );
    disconnect_terminal(named_id).expect("disconnect named runtime");
    wait_state(
        named_id,
        ConnectionState::Disconnected,
        "named runtime disconnect",
    );
    reconnect_terminal(named_id).expect("reconnect named runtime");
    wait_ready_with_host_key(named_id, "named runtime reconnect");
    let named_after_reconnect = wait_session(named_id, "named hierarchy after reconnect");
    assert_eq!(
        named_after_reconnect
            .panes
            .iter()
            .find(|pane| pane.selected)
            .expect("selected named pane after reconnect")
            .terminal_id,
        named_remote_terminal
    );
    let (mut wrong_runtime_controller, wrong_runtime_output) = start_external_control(
        &driver.manifest,
        "default",
        &driver.manifest.sessions["named-probe"].terminal_id,
        false,
    );
    let wrong_runtime_line = wrong_runtime_output
        .recv_timeout(Duration::from_secs(3))
        .expect("wrong-runtime controller response");
    assert!(
        wrong_runtime_line.contains("terminal.closed") && wrong_runtime_line.contains("not found"),
        "default runtime controlled a named runtime terminal: {wrong_runtime_line:?}"
    );
    stop_child(&mut wrong_runtime_controller);
    disconnect_terminal(named_id).expect("final disconnect named runtime");
    wait_state(
        named_id,
        ConnectionState::Disconnected,
        "final named disconnect",
    );

    // Exercise production workspace/group operations and selected-pane routing.
    let workspace_id = runtime["workspaces"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    create_group(default_id, workspace_id, "secondary").expect("create Herdr group");
    let grouped = wait_json(default_id, "group creation", |value| {
        value["groups"]
            .as_array()
            .is_some_and(|groups| groups.len() == 2)
    });
    assert_eq!(grouped["terminals"].as_array().unwrap().len(), 2);
    let group_id = grouped["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["name"] == "secondary")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    select_group(default_id, group_id).expect("select Herdr group");
    wait_json(default_id, "selected Herdr group", |value| {
        value["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| entity_id(&group["id"]) == group_id && group["selected"] == true)
    });
    rename_group(default_id, group_id, "secondary-renamed").expect("rename Herdr group");
    wait_json(default_id, "renamed Herdr group", |value| {
        value["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|group| group["name"] == "secondary-renamed")
    });

    let before_split = driver.api("default", "session.snapshot", json!({}));
    create_pane(default_id, workspace_id).expect("create Herdr pane");
    let with_extra_pane = wait_json(default_id, "Herdr pane creation", |value| {
        value["terminals"]
            .as_array()
            .is_some_and(|terminals| terminals.len() == 3)
    });
    let extra_pane = with_extra_pane["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|terminal| {
            entity_id(&terminal["groupId"]) == group_id
                && !grouped["terminals"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|old| old["id"] == terminal["id"])
        })
        .expect("new Herdr pane identity");
    let extra_pane_id = extra_pane["id"].as_str().unwrap().parse().unwrap();
    let after_split = driver.api("default", "session.snapshot", json!({}));
    let remote_pane = after_split["snapshot"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| {
            !before_split["snapshot"]["panes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|old| old["terminal_id"] == pane["terminal_id"])
        })
        .expect("one newly split remote pane")["pane_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (seq, state) in [(1, "working"), (2, "blocked"), (3, "unknown")] {
        driver.api("default", "pane.report_agent", json!({"pane_id":remote_pane,"source":"meeterm-fixture", "agent":"Claude", "state":state,"seq":seq}));
        wait_json(default_id, "new pane agent status event", |value| {
            value["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|terminal| {
                    entity_id(&terminal["id"]) == extra_pane_id
                        && terminal["agent"]["status"] == state
                })
        });
    }

    rename_pane(default_id, extra_pane_id, "secondary-pane-renamed").expect("rename Herdr pane");
    wait_json(default_id, "renamed Herdr pane", |value| {
        value["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|terminal| {
                entity_id(&terminal["id"]) == extra_pane_id
                    && terminal["name"] == "secondary-pane-renamed"
            })
    });
    select_pane(default_id, extra_pane_id).expect("select new Herdr pane");
    wait_json(default_id, "new Herdr pane selection", |value| {
        value["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|terminal| {
                entity_id(&terminal["id"]) == extra_pane_id && terminal["selected"] == true
            })
    });
    close_pane(default_id, extra_pane_id).expect("close Herdr pane");
    let after_pane_close = wait_json(default_id, "Herdr pane close", |value| {
        value["terminals"].as_array().is_some_and(|terminals| {
            terminals.len() == 2
                && !terminals
                    .iter()
                    .any(|terminal| entity_id(&terminal["id"]) == extra_pane_id)
        })
    });
    assert_eq!(after_pane_close["groups"].as_array().unwrap().len(), 2);
    select_pane(default_id, root.pane_id).expect("select original Herdr pane");
    wait_json(default_id, "original pane selection", |value| {
        value["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|terminal| {
                terminal["terminalId"] == format!("native:{}", root.terminal_id)
                    && terminal["selected"] == true
            })
    });
    close_group(default_id, group_id).expect("close Herdr group");
    let after_group_close = wait_json(default_id, "Herdr group close", |value| {
        value["groups"].as_array().is_some_and(|groups| {
            groups.len() == 1
                && !groups
                    .iter()
                    .any(|group| entity_id(&group["id"]) == group_id)
        })
    });
    assert_eq!(after_group_close["terminals"].as_array().unwrap().len(), 1);
    create_workspace(default_id, "native-extra").expect("create Herdr workspace");
    let extra = wait_json(default_id, "native workspace creation", |value| {
        value["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "native-extra")
    });
    assert_eq!(extra["terminals"].as_array().unwrap().len(), 2);
    let extra_id = extra["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workspace| workspace["name"] == "native-extra")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    rename_workspace(default_id, extra_id, "native-extra-renamed").expect("rename Herdr workspace");
    wait_json(default_id, "native workspace rename", |value| {
        value["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "native-extra-renamed")
    });
    let before_extra_close = terminal_revision(root.terminal_id).expect("root revision baseline");
    close_workspace(default_id, extra_id).expect("close Herdr workspace");
    wait_json(default_id, "native workspace close", |value| {
        !value["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| entity_id(&workspace["id"]) == extra_id)
    });
    wait_revision(
        default_id,
        root.terminal_id,
        before_extra_close,
        "root controller reactivation after workspace close",
    );

    let marker = "MEETERM_HERDR_NATIVE_SHELL_6A31";
    let command = marker_command(marker);
    assert_eq!(
        unsafe { meeterm_commit_utf8(root.terminal_id, command.as_ptr(), command.len()) },
        1
    );
    wait_text(root.terminal_id, marker, "production semantic shell marker");
    assert_eq!(meeterm_resize_terminal(root.terminal_id, 52, 20), 0);
    wait_dimensions(root.terminal_id, 52, 20, "production Herdr resize");
    let size_marker = "MEETERM_HERDR_NATIVE_SIZE_8D42";
    let size_command = format!("stty size; {}", marker_command(size_marker));
    assert_eq!(
        unsafe { meeterm_commit_utf8(root.terminal_id, size_command.as_ptr(), size_command.len()) },
        2
    );
    let resized = wait_text(root.terminal_id, size_marker, "actual Herdr PTY resize");
    assert!(
        resized.text.contains("20 52"),
        "stty did not observe native resize: {}",
        resized.text
    );

    // Herdr owns scrollback.  Scrolling up must show older output, and the
    // next semantic input must first return the remote pane to its live
    // bottom before the shell receives it.
    let scroll_command = "i=1; while [ \"$i\" -le 50 ]; do printf 'MEETERM_SCROLL_LINE_%02d\\n' \"$i\"; i=$((i+1)); done; printf '%s%s\\n' 'MEETERM_SCROLL_' 'READY'\n";
    assert_eq!(
        unsafe {
            meeterm_commit_utf8(
                root.terminal_id,
                scroll_command.as_ptr(),
                scroll_command.len(),
            )
        },
        3
    );
    wait_text(
        root.terminal_id,
        "MEETERM_SCROLL_READY",
        "Herdr scrollback fixture output",
    );
    assert_eq!(meeterm_scroll_lines(root.terminal_id, 100), 0);
    let scrolled = wait_text(
        root.terminal_id,
        "MEETERM_SCROLL_LINE_01",
        "Herdr scrollback up",
    );
    assert!(
        !scrolled.text.contains("MEETERM_SCROLL_LINE_50"),
        "scrolling up did not leave the live bottom"
    );
    let scroll_input_marker = "MEETERM_SCROLL_INPUT_1D2E";
    let scroll_input = marker_command(scroll_input_marker);
    assert_eq!(
        unsafe { meeterm_commit_utf8(root.terminal_id, scroll_input.as_ptr(), scroll_input.len()) },
        4
    );
    let bottom = wait_text(
        root.terminal_id,
        scroll_input_marker,
        "Herdr input after scrollback",
    );
    assert!(
        bottom.text.contains("MEETERM_SCROLL_LINE_50"),
        "input did not return the pane to the live bottom"
    );
    assert!(
        !bottom.text.contains("MEETERM_SCROLL_LINE_01"),
        "input remained in the scrolled viewport"
    );

    let tui = shell_tui(Path::new(&driver.manifest.root));
    let tui_command = format!("python3 {}\n", tui.display());
    assert_eq!(
        unsafe { meeterm_commit_utf8(root.terminal_id, tui_command.as_ptr(), tui_command.len()) },
        5
    );
    wait_text(
        root.terminal_id,
        "DECCKM_READY",
        "full-screen Herdr TUI readiness",
    );
    // Every transition must release/reacquire and produce a fresh frame.
    // Repeated transitions exercise the resize-sender-close/lifecycle race;
    // a failure ends the test, rather than retrying a failed transition.
    for transition in 0..8 {
        let before_visibility = terminal_revision(root.terminal_id).expect("visibility baseline");
        assert_eq!(
            meeterm_set_terminal_visible(default_id, 0),
            0,
            "release Herdr controller on rapid visibility transition {transition}"
        );
        assert_eq!(
            meeterm_set_terminal_visible(default_id, 1),
            0,
            "reacquire Herdr controller on rapid visibility transition {transition}"
        );
        wait_revision(
            default_id,
            root.terminal_id,
            before_visibility,
            &format!("rapid visibility false-to-true frame {transition}"),
        );
    }
    assert_eq!(
        meeterm_send_special_key(root.terminal_id, SpecialKey::Up as u32),
        1
    );
    wait_text(
        root.terminal_id,
        "UP_RESULT_1b4f41",
        "logical Up through Herdr API",
    );
    let paste = "first\n日本語";
    assert_eq!(
        unsafe { meeterm_paste_utf8(root.terminal_id, paste.as_ptr(), paste.len()) },
        paste.len() as i32
    );
    let pasted = wait_text(
        root.terminal_id,
        "PASTE_RESULT_READY",
        "CJK multiline semantic paste",
    );
    assert!(
        pasted
            .text
            .split_whitespace()
            .collect::<String>()
            .contains("PASTE_RESULT_1b5b3230307e66697273740ae697a5e69cace8aa9e1b5b3230317e"),
        "remote TUI did not receive the exact bracketed UTF-8/LF paste"
    );

    // A second controller is rejected without takeover while the production
    // controller remains live. An explicit takeover then closes production's
    // stream; reconnect must use the same stable remote terminal identity.
    let (mut rival, rejection) = start_external_control(
        &driver.manifest,
        "default",
        &driver.manifest.sessions["default"].terminal_id,
        false,
    );
    let rejection = rejection
        .recv_timeout(WAIT_TIMEOUT)
        .expect("rival rejection");
    assert!(
        rejection.contains("terminal.closed")
            && rejection.contains("already has an attached client"),
        "second controller was not rejected: {rejection:?}"
    );
    stop_child(&mut rival);
    let (mut takeover, takeover_output) = start_external_control(
        &driver.manifest,
        "default",
        &driver.manifest.sessions["default"].terminal_id,
        true,
    );
    let takeover_frame = takeover_output
        .recv_timeout(WAIT_TIMEOUT)
        .expect("takeover controller frame");
    assert!(
        takeover_frame.contains("terminal.frame"),
        "takeover did not become controller: {takeover_frame:?}"
    );
    wait_state(
        default_id,
        ConnectionState::Failed,
        "explicit Herdr takeover",
    );
    stop_child(&mut takeover);
    reconnect_terminal(default_id).expect("reconnect after explicit takeover");
    wait_ready_with_host_key(default_id, "Herdr reconnect after takeover");
    let reconnected = wait_session(default_id, "stable pane after takeover");
    let same = reconnected
        .panes
        .iter()
        .find(|pane| pane.terminal_id == root.terminal_id)
        .expect("stable remote terminal identity");
    assert_eq!(same.terminal_id, root.terminal_id);
    wait_text(
        root.terminal_id,
        "PASTE_RESULT_READY",
        "full-screen TUI restored after takeover",
    );
    assert_eq!(
        meeterm_send_special_key(root.terminal_id, SpecialKey::Up as u32),
        1
    );
    wait_text(
        root.terminal_id,
        "RECONNECT_UP_1b4f41",
        "mode-aware input after TUI reconnect",
    );
    assert_eq!(
        meeterm_send_special_key(root.terminal_id, SpecialKey::Enter as u32),
        1
    );
    wait_text(
        root.terminal_id,
        "TUI_DONE",
        "explicit TUI exit back to shell",
    );

    // External rename and move arrive through the subscription and are
    // reconciled by the production actor while preserving terminal identity.
    let workspace_snapshot = driver.cli("default", &["api", "snapshot"]);
    let external_workspace =
        workspace_snapshot["result"]["snapshot"]["workspaces"][0]["workspace_id"]
            .as_str()
            .unwrap();
    driver.cli(
        "default",
        &[
            "workspace",
            "rename",
            external_workspace,
            "externally-renamed",
        ],
    );
    wait_json(default_id, "external workspace rename", |value| {
        value["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "externally-renamed")
    });
    let external_pane = driver.manifest.sessions["default"].pane_id.clone();
    driver.cli(
        "default",
        &[
            "pane",
            "move",
            &external_pane,
            "--new-workspace",
            "--label",
            "externally-moved",
            "--focus",
        ],
    );
    let moved = wait_json(default_id, "external pane move", |value| {
        value["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "externally-moved")
    });
    assert!(
        moved["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|terminal| terminal["terminalId"] == format!("native:{}", root.terminal_id)),
        "moved pane lost stable native identity"
    );
    // A move updates the mutable pane alias while the stable controller
    // remains attached. No redraw is required when display content is equal;
    // the following shell round trip proves input uses the resynchronized ID.

    let final_marker = "MEETERM_HERDR_NATIVE_RECONNECT_4B20";
    let final_command = marker_command(final_marker);
    assert_eq!(
        unsafe {
            meeterm_commit_utf8(
                root.terminal_id,
                final_command.as_ptr(),
                final_command.len(),
            )
        },
        6
    );
    wait_text(root.terminal_id, final_marker, "post-resync input");
    let sticky = "export MEETERM_NATIVE_STICKY=6F19; printf '%s%s\\n' 'STICKY_' 'SET'\n";
    assert!(unsafe { meeterm_commit_utf8(root.terminal_id, sticky.as_ptr(), sticky.len()) } > 0);
    wait_text(root.terminal_id, "STICKY_SET", "remote shell variable set");
    set_foreground(default_id, false).expect("background Herdr connection");
    wait_state(
        default_id,
        ConnectionState::Reconnecting,
        "Herdr background suspension",
    );
    let rejected = "must-not-send";
    assert_eq!(
        unsafe { meeterm_commit_utf8(root.terminal_id, rejected.as_ptr(), rejected.len()) },
        0
    );
    let before_foreground = terminal_revision(root.terminal_id).unwrap();
    set_foreground(default_id, true).expect("foreground Herdr connection");
    wait_ready_with_host_key(default_id, "Herdr foreground reconnect");
    wait_revision(
        default_id,
        root.terminal_id,
        before_foreground,
        "fresh foreground frame",
    );
    commit_marker(
        root.terminal_id,
        "HERDR_FOREGROUND_OK_7E24",
        "foreground shell round trip",
    );

    ssh.lose_connections();
    wait_state(
        default_id,
        ConnectionState::Reconnecting,
        "server-side SSH connection loss",
    );
    wait_ready_with_host_key(default_id, "automatic Herdr reconnect after SSH loss");
    commit_marker(
        root.terminal_id,
        "HERDR_TRANSPORT_RECOVERED_32C4",
        "automatic transport recovery input",
    );

    // A new connection owner has no old registry state, as after app process
    // death. The same remote terminal and shell must still exist.
    disconnect_terminal(default_id).expect("disconnect original owner");
    wait_state(
        default_id,
        ConnectionState::Disconnected,
        "original owner disconnect",
    );
    let fresh_id = create_terminal(52, 20).unwrap();
    let _fresh_guard = TerminalGuard { id: fresh_id };
    connect_terminal(fresh_id, options(&driver.manifest, &ssh, None)).unwrap();
    wait_ready_with_host_key(fresh_id, "fresh owner Herdr reconnect");
    let fresh = wait_session(fresh_id, "fresh owner pane");
    let fresh_pane = fresh.panes.iter().find(|pane| pane.selected).unwrap();
    assert_ne!(fresh_pane.terminal_id, root.terminal_id);
    commit_marker(
        fresh_pane.terminal_id,
        "HERDR_FRESH_OWNER_OK_16A3",
        "fresh owner shell round trip",
    );
    let retained = "printf '%s%s\\n' 'STICKY_RETAINED_' \"$MEETERM_NATIVE_STICKY\"\n";
    assert!(
        unsafe { meeterm_commit_utf8(fresh_pane.terminal_id, retained.as_ptr(), retained.len()) }
            > 0
    );
    wait_text(
        fresh_pane.terminal_id,
        "STICKY_RETAINED_6F19",
        "same remote shell survived fresh owner",
    );
    assert_eq!(meeterm_set_terminal_visible(fresh_id, 0), 0);
    let pc = Command::new("python3")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/herdr/native_integration.py"),
        )
        .arg("--pc-handoff")
        .arg("--root")
        .arg(&driver.manifest.root)
        .arg("--manifest")
        .arg(driver.base.join("manifest.json"))
        .output()
        .expect("ordinary PC handoff fixture");
    assert!(
        pc.status.success(),
        "ordinary PC handoff failed: {}",
        String::from_utf8_lossy(&pc.stderr)
    );
    assert!(String::from_utf8_lossy(&pc.stdout).contains("PC_HANDOFF_OK"));
    let before_return = terminal_revision(fresh_pane.terminal_id).unwrap();
    assert_eq!(meeterm_set_terminal_visible(fresh_id, 1), 0);
    wait_revision(
        fresh_id,
        fresh_pane.terminal_id,
        before_return,
        "phone return after ordinary PC handoff",
    );
    commit_marker(
        fresh_pane.terminal_id,
        "PHONE_RETURNED_FROM_PC_3B84",
        "phone input after PC handoff",
    );
}
