//! Rust-owned SSH lifecycle and host-key trust policy.
//!
//! This module deliberately exposes a small control plane.  Terminal bytes
//! remain in the registry and are exchanged with russh through bounded native
//! queues; callers poll the fixed connection snapshot and terminal revision.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use russh::client::{self, Handler};
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use zeroize::Zeroize;

use crate::registry::{self, TerminalId};
use crate::terminal::INPUT_QUEUE_CAPACITY;

/// Maximum number of bytes used by each fixed-size string in the C snapshot.
pub const HOST_CAPACITY: usize = 256;
pub const FINGERPRINT_CAPACITY: usize = 128;
pub const ALGORITHM_CAPACITY: usize = 64;
pub const ERROR_CODE_CAPACITY: usize = 64;
pub const ERROR_MESSAGE_CAPACITY: usize = 256;

/// Connection states mirrored by the TypeScript/native adapters.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected = 0,
    Connecting = 1,
    HostKeyPending = 2,
    Authenticating = 3,
    OpeningPty = 4,
    Ready = 5,
    Closing = 6,
    Failed = 7,
}

/// A fixed-layout snapshot for C, Swift, Kotlin, and other native callers.
///
/// Every string is UTF-8 with an explicit byte length.  The arrays are zeroed
/// after the meaningful prefix, so callers can copy this value by value
/// without allocating or crossing the JavaScript boundary.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ConnectionSnapshot {
    pub state: u32,
    pub port: u16,
    pub reserved: u16,
    pub host_len: u16,
    pub host: [u8; HOST_CAPACITY],
    pub fingerprint_len: u16,
    pub fingerprint: [u8; FINGERPRINT_CAPACITY],
    pub algorithm_len: u16,
    pub algorithm: [u8; ALGORITHM_CAPACITY],
    pub known_fingerprint_len: u16,
    pub known_fingerprint: [u8; FINGERPRINT_CAPACITY],
    pub error_code_len: u16,
    pub error_code: [u8; ERROR_CODE_CAPACITY],
    pub error_message_len: u16,
    pub error_message: [u8; ERROR_MESSAGE_CAPACITY],
}

impl ConnectionSnapshot {
    fn disconnected() -> Self {
        Self {
            state: ConnectionState::Disconnected as u32,
            port: 0,
            reserved: 0,
            host_len: 0,
            host: [0; HOST_CAPACITY],
            fingerprint_len: 0,
            fingerprint: [0; FINGERPRINT_CAPACITY],
            algorithm_len: 0,
            algorithm: [0; ALGORITHM_CAPACITY],
            known_fingerprint_len: 0,
            known_fingerprint: [0; FINGERPRINT_CAPACITY],
            error_code_len: 0,
            error_code: [0; ERROR_CODE_CAPACITY],
            error_message_len: 0,
            error_message: [0; ERROR_MESSAGE_CAPACITY],
        }
    }
}

/// Options accepted by the native SSH connect entry point.
///
/// The private key and passphrase are intentionally not part of a debug
/// representation and are owned only by the short-lived connection task.
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key: String,
    pub passphrase: Option<String>,
    pub known_hosts_path: PathBuf,
}

impl ConnectOptions {
    fn validate(self) -> Result<Self, ConnectionError> {
        let host = canonical_host(&self.host)?;
        if self.port == 0 || self.username.is_empty() || invalid_identity_component(&self.username)
        {
            return Err(ConnectionError::InvalidArgument);
        }
        if self.private_key.is_empty() || self.known_hosts_path.as_os_str().is_empty() {
            return Err(ConnectionError::InvalidArgument);
        }
        if self
            .passphrase
            .as_deref()
            .is_some_and(|passphrase| passphrase.contains('\0'))
        {
            return Err(ConnectionError::InvalidArgument);
        }
        Ok(Self { host, ..self })
    }
}

/// Synchronous errors returned by the native control plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    InvalidArgument,
    UnknownTerminal,
    RuntimeUnavailable,
    Internal,
    HostKeyResponse,
    TrustStore,
}

impl ConnectionError {
    pub const fn code(self) -> i32 {
        match self {
            Self::InvalidArgument => -1,
            Self::UnknownTerminal => -2,
            Self::RuntimeUnavailable => -3,
            Self::Internal => -4,
            Self::HostKeyResponse => -5,
            Self::TrustStore => -6,
        }
    }

    pub const fn error_code(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::UnknownTerminal => "unknown_terminal",
            Self::RuntimeUnavailable => "runtime_unavailable",
            Self::Internal => "internal_error",
            Self::HostKeyResponse => "host_key_response",
            Self::TrustStore => "trust_store",
        }
    }
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArgument => "connection arguments are invalid",
            Self::UnknownTerminal => "terminal ID is not registered",
            Self::RuntimeUnavailable => "native runtime is unavailable",
            Self::Internal => "native connection state is unavailable",
            Self::HostKeyResponse => "host-key response is stale or unexpected",
            Self::TrustStore => "host-key trust storage is unavailable",
        })
    }
}

impl std::error::Error for ConnectionError {}

struct PendingHostKey {
    fingerprint: String,
    response: oneshot::Sender<HostKeyDecision>,
}

struct HostKeyDecision {
    accept: bool,
}

struct ConnectionInfo {
    state: ConnectionState,
    host: String,
    port: u16,
    fingerprint: String,
    algorithm: String,
    known_fingerprint: String,
    error_code: String,
    error_message: String,
    pending: Option<PendingHostKey>,
}

impl ConnectionInfo {
    fn new(host: String, port: u16) -> Self {
        Self {
            state: ConnectionState::Connecting,
            host: host.clone(),
            port,
            fingerprint: String::new(),
            algorithm: String::new(),
            known_fingerprint: String::new(),
            error_code: String::new(),
            error_message: String::new(),
            pending: None,
        }
    }

    fn snapshot(&self) -> ConnectionSnapshot {
        let mut snapshot = ConnectionSnapshot {
            state: self.state as u32,
            port: self.port,
            reserved: 0,
            host_len: 0,
            host: [0; HOST_CAPACITY],
            fingerprint_len: 0,
            fingerprint: [0; FINGERPRINT_CAPACITY],
            algorithm_len: 0,
            algorithm: [0; ALGORITHM_CAPACITY],
            known_fingerprint_len: 0,
            known_fingerprint: [0; FINGERPRINT_CAPACITY],
            error_code_len: 0,
            error_code: [0; ERROR_CODE_CAPACITY],
            error_message_len: 0,
            error_message: [0; ERROR_MESSAGE_CAPACITY],
        };
        copy_string(&mut snapshot.host, &mut snapshot.host_len, &self.host);
        copy_string(
            &mut snapshot.fingerprint,
            &mut snapshot.fingerprint_len,
            &self.fingerprint,
        );
        copy_string(
            &mut snapshot.algorithm,
            &mut snapshot.algorithm_len,
            &self.algorithm,
        );
        copy_string(
            &mut snapshot.known_fingerprint,
            &mut snapshot.known_fingerprint_len,
            &self.known_fingerprint,
        );
        copy_string(
            &mut snapshot.error_code,
            &mut snapshot.error_code_len,
            &self.error_code,
        );
        copy_string(
            &mut snapshot.error_message,
            &mut snapshot.error_message_len,
            &self.error_message,
        );
        snapshot
    }
}

struct ConnectionShared {
    terminal_id: TerminalId,
    generation: u64,
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
    info: Mutex<ConnectionInfo>,
    cancelled: AtomicBool,
    cancel_notify: Arc<Notify>,
}

impl ConnectionShared {
    fn new(
        terminal_id: TerminalId,
        generation: u64,
        host: String,
        port: u16,
        known_hosts_path: PathBuf,
    ) -> Self {
        Self {
            terminal_id,
            generation,
            host: host.clone(),
            port,
            known_hosts_path,
            info: Mutex::new(ConnectionInfo::new(host, port)),
            cancelled: AtomicBool::new(false),
            cancel_notify: Arc::new(Notify::new()),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        // State setters take this same lock before checking cancellation. By
        // setting the flag while holding it, no delayed trust/auth callback
        // can write a nonterminal state after cancellation has committed.
        if let Ok(mut info) = self.info.lock() {
            self.cancelled.store(true, Ordering::Release);
            info.state = ConnectionState::Closing;
            info.pending = None;
        } else {
            self.cancelled.store(true, Ordering::Release);
        }
        self.cancel_notify.notify_waiters();
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.cancel_notify.notified();
            tokio::pin!(notified);
            // Register before reading the atomic flag.  Without enable(), a
            // cancel between the flag check and the first poll could be lost
            // because notify_waiters does not retain a permit.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
            if self.is_cancelled() {
                return;
            }
        }
    }

    fn set_state(&self, state: ConnectionState) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() {
                return;
            }
            info.state = state;
        }
    }

    fn set_host_key(&self, fingerprint: String, algorithm: String) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() {
                return;
            }
            info.fingerprint = fingerprint;
            info.algorithm = algorithm;
            info.known_fingerprint.clear();
        }
    }

    fn begin_host_prompt(
        &self,
        fingerprint: String,
        algorithm: String,
        response: oneshot::Sender<HostKeyDecision>,
    ) -> bool {
        let Ok(mut info) = self.info.lock() else {
            return false;
        };
        if self.is_cancelled() {
            return false;
        }
        info.state = ConnectionState::HostKeyPending;
        info.fingerprint = fingerprint.clone();
        info.algorithm = algorithm;
        info.known_fingerprint.clear();
        info.error_code.clear();
        info.error_message.clear();
        info.pending = Some(PendingHostKey {
            fingerprint,
            response,
        });
        true
    }

    fn set_changed_key(&self, fingerprint: String, algorithm: String, known: String) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() {
                return;
            }
            info.state = ConnectionState::Failed;
            info.fingerprint = fingerprint;
            info.algorithm = algorithm;
            info.known_fingerprint = known;
            info.error_code = "host_key_changed".to_owned();
            info.error_message = "The server host key changed; connection refused.".to_owned();
            info.pending = None;
        }
    }

    fn fail(&self, code: &'static str, message: &'static str) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() {
                return;
            }
            if info.state == ConnectionState::Failed {
                return;
            }
            info.state = ConnectionState::Failed;
            info.error_code = code.to_owned();
            info.error_message = message.to_owned();
            info.pending = None;
        }
    }

    fn mark_closing(&self) {
        if let Ok(mut info) = self.info.lock() {
            info.state = ConnectionState::Closing;
            info.pending = None;
        }
    }

    fn mark_disconnected(&self) {
        if let Ok(mut info) = self.info.lock() {
            info.state = ConnectionState::Disconnected;
            info.pending = None;
        }
    }

    fn snapshot(&self) -> Result<ConnectionSnapshot, ConnectionError> {
        self.info
            .lock()
            .map(|info| info.snapshot())
            .map_err(|_| ConnectionError::Internal)
    }
}

struct ConnectionEntry {
    shared: Arc<ConnectionShared>,
    abort: tokio::task::AbortHandle,
}

static RUNTIME: OnceLock<Result<Runtime, ()>> = OnceLock::new();
static CONNECTIONS: OnceLock<Mutex<HashMap<TerminalId, ConnectionEntry>>> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn runtime() -> Result<&'static Runtime, ConnectionError> {
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| ConnectionError::RuntimeUnavailable)
}

fn connections() -> &'static Mutex<HashMap<TerminalId, ConnectionEntry>> {
    CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start or replace the SSH session associated with a terminal ID.
pub fn connect_terminal(
    terminal_id: TerminalId,
    options: ConnectOptions,
) -> Result<(), ConnectionError> {
    let options = options.validate()?;
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let runtime = runtime()?;
    let generation = next_generation();

    let mut entries = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;

    if let Some(old) = entries.remove(&terminal_id) {
        old.shared.mark_closing();
        old.shared.cancel();
        old.abort.abort();
    }

    registry::begin_remote(terminal_id, generation).map_err(map_terminal_error)?;
    let shared = Arc::new(ConnectionShared::new(
        terminal_id,
        generation,
        options.host.clone(),
        options.port,
        options.known_hosts_path.clone(),
    ));
    let task_shared = Arc::clone(&shared);
    let join = runtime.spawn(async move {
        run_connection(task_shared, options).await;
    });
    entries.insert(
        terminal_id,
        ConnectionEntry {
            shared,
            abort: join.abort_handle(),
        },
    );
    Ok(())
}

/// Abort the session and leave the terminal in remote mode with local echo
/// disabled.  The registry entry remains available for state polling.
pub fn disconnect_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = {
        let mut entries = connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        cancel_entry_locked(&mut entries, terminal_id)
    };

    let Some(shared) = shared else {
        return Ok(());
    };
    registry::detach_transport(terminal_id, shared.generation);
    shared.mark_disconnected();
    Ok(())
}

fn cancel_entry_locked(
    entries: &mut HashMap<TerminalId, ConnectionEntry>,
    terminal_id: TerminalId,
) -> Option<Arc<ConnectionShared>> {
    let entry = entries.get(&terminal_id)?;
    // The map lock covers selection and cancellation together. A concurrent
    // connect therefore either replaces this entry before we select it, or
    // waits until this exact entry has been cancelled; it cannot have its new
    // generation aborted by a stale disconnect.
    entry.shared.mark_closing();
    entry.shared.cancel();
    entry.abort.abort();
    Some(Arc::clone(&entry.shared))
}

/// Stop any owned SSH task after its terminal registry entry has been
/// explicitly destroyed.  View unmounts do not call this path; they retain the
/// stable terminal ID and its connection.
pub(crate) fn terminal_destroyed(terminal_id: TerminalId) {
    let Some(entry) = connections()
        .lock()
        .ok()
        .and_then(|mut entries| entries.remove(&terminal_id))
    else {
        return;
    };
    entry.shared.mark_closing();
    entry.shared.cancel();
    entry.abort.abort();
}

/// Return a state snapshot.  A known terminal with no active SSH entry is
/// represented as `Disconnected` so callers can poll before the first connect.
pub fn connection_snapshot(terminal_id: TerminalId) -> Result<ConnectionSnapshot, ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let entries = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    entries
        .get(&terminal_id)
        .map(|entry| entry.shared.snapshot())
        .unwrap_or_else(|| Ok(ConnectionSnapshot::disconnected()))
}

/// Answer the one-shot prompt for a previously unknown host key.
pub fn respond_to_host_key(
    terminal_id: TerminalId,
    fingerprint: &str,
    accept: bool,
) -> Result<(), ConnectionError> {
    if fingerprint.is_empty() {
        return Err(ConnectionError::InvalidArgument);
    }
    let shared = current_connection(terminal_id)?;
    let pending = {
        let mut info = shared.info.lock().map_err(|_| ConnectionError::Internal)?;
        let pending = info
            .pending
            .take()
            .ok_or(ConnectionError::HostKeyResponse)?;
        if pending.fingerprint != fingerprint {
            info.pending = Some(pending);
            return Err(ConnectionError::HostKeyResponse);
        }
        pending
    };
    pending
        .response
        .send(HostKeyDecision { accept })
        .map_err(|_| ConnectionError::HostKeyResponse)
}

/// Forget all trusted entries matching the canonical host and port.
pub fn forget_host_key(
    host: &str,
    port: u16,
    known_hosts_path: &Path,
) -> Result<(), ConnectionError> {
    let host = canonical_host(host)?;
    if port == 0 || known_hosts_path.as_os_str().is_empty() {
        return Err(ConnectionError::InvalidArgument);
    }
    trust::forget(&host, port, known_hosts_path).map_err(|_| ConnectionError::TrustStore)
}

/// Query the monotonic native terminal-content revision.
pub fn terminal_revision(terminal_id: TerminalId) -> Result<u64, ConnectionError> {
    registry::terminal_revision(terminal_id).map_err(map_terminal_error)
}

/// Enqueue raw native bytes.  This is useful for platform input paths that
/// already encoded a terminal sequence; UTF-8 validation belongs to the
/// separate commit API.
pub fn send_bytes(terminal_id: TerminalId, bytes: &[u8]) -> Result<usize, ConnectionError> {
    registry::send_bytes(terminal_id, bytes).map_err(map_terminal_error)
}

fn current_connection(terminal_id: TerminalId) -> Result<Arc<ConnectionShared>, ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let entries = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    entries
        .get(&terminal_id)
        .map(|entry| Arc::clone(&entry.shared))
        .ok_or(ConnectionError::HostKeyResponse)
}

fn next_generation() -> u64 {
    loop {
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        if generation != 0 {
            return generation;
        }
    }
}

fn map_terminal_error(error: crate::terminal::TerminalError) -> ConnectionError {
    match error {
        crate::terminal::TerminalError::UnknownTerminal => ConnectionError::UnknownTerminal,
        _ => ConnectionError::Internal,
    }
}

struct HostKeyHandler {
    shared: Arc<ConnectionShared>,
    setup: Arc<ConnectIoControl>,
}

impl Handler for HostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if self.shared.is_cancelled() || self.setup.is_cancelled() {
            return Ok(false);
        }

        let key = server_public_key.public_key();
        let fingerprint = fingerprint(&key);
        let algorithm = key.algorithm().to_string();
        match trust::assess(
            &self.shared.host,
            self.shared.info_port(),
            &key,
            &self.shared.known_hosts_path,
        ) {
            Ok(trust::Decision::Trusted) => {
                if self.shared.is_cancelled() || self.setup.is_cancelled() {
                    return Ok(false);
                }
                self.shared.set_host_key(fingerprint, algorithm);
                Ok(true)
            }
            Ok(trust::Decision::Changed { known_fingerprint }) => {
                if self.shared.is_cancelled() || self.setup.is_cancelled() {
                    return Ok(false);
                }
                self.shared
                    .set_changed_key(fingerprint, algorithm, known_fingerprint);
                Ok(false)
            }
            Ok(trust::Decision::Unknown) => {
                if self.shared.is_cancelled() || self.setup.is_cancelled() {
                    return Ok(false);
                }
                let (sender, receiver) = oneshot::channel();
                if !self
                    .shared
                    .begin_host_prompt(fingerprint.clone(), algorithm, sender)
                {
                    return Ok(false);
                }

                let decision = tokio::select! {
                    _ = self.shared.cancelled() => return Ok(false),
                    _ = self.setup.cancelled() => return Ok(false),
                    result = tokio::time::timeout(HOST_KEY_PROMPT_TIMEOUT, receiver) => result,
                };
                match decision {
                    Ok(Ok(HostKeyDecision { accept: true })) => {
                        if self.shared.is_cancelled() || self.setup.is_cancelled() {
                            return Ok(false);
                        }
                        match trust::learn(
                            &self.shared.host,
                            self.shared.info_port(),
                            &key,
                            &self.shared.known_hosts_path,
                        ) {
                            Ok(()) => {
                                if self.shared.is_cancelled() || self.setup.is_cancelled() {
                                    return Ok(false);
                                }
                                self.shared
                                    .set_host_key(fingerprint, key.algorithm().to_string());
                                Ok(true)
                            }
                            Err(_) => {
                                self.shared.fail(
                                    "host_key_store",
                                    "The host-key trust file could not be updated.",
                                );
                                Ok(false)
                            }
                        }
                    }
                    Ok(Ok(HostKeyDecision { accept: false })) => {
                        if self.shared.is_cancelled() || self.setup.is_cancelled() {
                            return Ok(false);
                        }
                        self.shared
                            .fail("host_key_rejected", "The server host key was not accepted.");
                        Ok(false)
                    }
                    Ok(Err(_)) | Err(_) => {
                        self.shared
                            .fail("host_key_timeout", "The host-key confirmation timed out.");
                        Ok(false)
                    }
                }
            }
            Err(_) => {
                self.shared.fail(
                    "host_key_store",
                    "The host-key trust file could not be read safely.",
                );
                Ok(false)
            }
        }
    }
}

impl ConnectionShared {
    fn info_port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Copy)]
enum FlowFailure {
    KeyFile,
    Network,
    Authentication,
    Channel,
    Pty,
    Transport,
    RemoteClosed,
    Stale,
}

impl FlowFailure {
    const fn details(self) -> (&'static str, &'static str) {
        match self {
            Self::KeyFile => ("key_file", "The private key could not be loaded."),
            Self::Network => ("network", "The SSH connection could not be established."),
            Self::Authentication => ("auth_failed", "Public-key authentication failed."),
            Self::Channel => ("channel", "The SSH session channel could not be opened."),
            Self::Pty => ("pty_failed", "The remote pseudo-terminal request failed."),
            Self::Transport => ("transport", "The SSH terminal transport stopped."),
            Self::RemoteClosed => ("remote_closed", "The remote terminal closed the session."),
            Self::Stale => ("stale_connection", "The SSH connection was replaced."),
        }
    }
}

const SSH_STAGE_TIMEOUT: Duration = Duration::from_secs(30);
// The stream handshake includes the host-key callback, so this budget must
// leave room for the complete explicit trust prompt in addition to network
// setup.
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(180);
const HOST_KEY_PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Cancellation and the deadline used by the pre-authentication russh task.
///
/// russh 0.63.2 wraps its spawned session task in an oneshot-backed join
/// handle. Dropping that handle does not abort the task, so the stream itself
/// must observe both cancellation and the setup deadline. Once key exchange
/// has completed, the deadline is cleared while cancellation remains active.
struct ConnectIoControl {
    cancelled: AtomicBool,
    cancel_notify: Arc<Notify>,
    deadline: Mutex<Option<Instant>>,
}

impl ConnectIoControl {
    fn new(deadline: Instant) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            cancel_notify: Arc::new(Notify::new()),
            deadline: Mutex::new(Some(deadline)),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel_notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.cancel_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
            if self.is_cancelled() {
                return;
            }
        }
    }

    fn clear_deadline(&self) {
        if let Ok(mut deadline) = self.deadline.lock() {
            *deadline = None;
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match self.deadline.lock() {
            Ok(deadline) => *deadline,
            // A poisoned setup-control lock must not disable the deadline and
            // leave a russh task detached indefinitely.
            Err(_) => Some(Instant::now()),
        }
    }
}

/// Ensures a stream handed to russh is signalled if the owning connection
/// future is aborted before russh has shut down its spawned session task.
struct ConnectStreamGuard {
    control: Arc<ConnectIoControl>,
}

impl ConnectStreamGuard {
    fn new(control: Arc<ConnectIoControl>) -> Self {
        Self { control }
    }
}

impl Drop for ConnectStreamGuard {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

/// A TCP stream that wakes russh's pre-authentication session task when the
/// owning connection is cancelled or its setup deadline expires.
struct CancellableStream {
    inner: tokio::net::TcpStream,
    shared: Arc<ConnectionShared>,
    control: Arc<ConnectIoControl>,
    deadline_timer: Option<Pin<Box<tokio::time::Sleep>>>,
    shared_cancel: Pin<Box<tokio::sync::futures::OwnedNotified>>,
    local_cancel: Pin<Box<tokio::sync::futures::OwnedNotified>>,
}

impl CancellableStream {
    fn new(
        inner: tokio::net::TcpStream,
        shared: Arc<ConnectionShared>,
        control: Arc<ConnectIoControl>,
    ) -> Self {
        let shared_cancel = Arc::clone(&shared.cancel_notify).notified_owned();
        let local_cancel = Arc::clone(&control.cancel_notify).notified_owned();
        Self {
            inner,
            shared,
            control,
            deadline_timer: None,
            shared_cancel: Box::pin(shared_cancel),
            local_cancel: Box::pin(local_cancel),
        }
    }

    fn poll_cancel(&mut self, context: &mut Context<'_>) -> bool {
        // Register both waiters before checking the flags. `notify_waiters`
        // does not retain a permit, so this ordering closes the check/register
        // race for an explicit disconnect or a dropped connect guard.
        let shared_notified = self.shared_cancel.as_mut().enable();
        let local_notified = self.local_cancel.as_mut().enable();
        if shared_notified
            || local_notified
            || self.shared.is_cancelled()
            || self.control.is_cancelled()
        {
            return true;
        }
        if self.shared_cancel.as_mut().poll(context).is_ready()
            || self.local_cancel.as_mut().poll(context).is_ready()
        {
            return true;
        }
        false
    }

    fn poll_deadline(&mut self, context: &mut Context<'_>) -> bool {
        let Some(deadline) = self.control.deadline() else {
            // The setup deadline is cleared after successful key exchange.
            // Dropping this timer prevents a stale wakeup from being treated
            // as an interactive-session timeout.
            self.deadline_timer = None;
            return false;
        };
        if Instant::now() >= deadline {
            return true;
        }
        if self.deadline_timer.is_none() {
            self.deadline_timer = Some(Box::pin(tokio::time::sleep_until(deadline.into())));
        }
        self.deadline_timer
            .as_mut()
            .expect("deadline timer initialized")
            .as_mut()
            .poll(context)
            .is_ready()
    }

    fn cancelled_error() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "SSH connection was cancelled",
        )
    }

    fn deadline_error() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "SSH connection setup timed out",
        )
    }
}

// All self-referential state is kept behind Pin<Box<_>>; moving this wrapper
// does not move a pinned timer or notification future.
impl Unpin for CancellableStream {}

impl AsyncRead for CancellableStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.poll_cancel(context) {
            return Poll::Ready(Err(Self::cancelled_error()));
        }
        if self.poll_deadline(context) {
            return Poll::Ready(Err(Self::deadline_error()));
        }
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for CancellableStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.poll_cancel(context) {
            return Poll::Ready(Err(Self::cancelled_error()));
        }
        if self.poll_deadline(context) {
            return Poll::Ready(Err(Self::deadline_error()));
        }
        Pin::new(&mut self.inner).poll_write(context, bytes)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.poll_cancel(context) {
            return Poll::Ready(Err(Self::cancelled_error()));
        }
        if self.poll_deadline(context) {
            return Poll::Ready(Err(Self::deadline_error()));
        }
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.poll_cancel(context) {
            return Poll::Ready(Err(Self::cancelled_error()));
        }
        if self.poll_deadline(context) {
            return Poll::Ready(Err(Self::deadline_error()));
        }
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

async fn await_stage<F, T, E>(
    shared: &ConnectionShared,
    future: F,
    timeout: Duration,
    failure: FlowFailure,
) -> Result<T, FlowFailure>
where
    F: Future<Output = Result<T, E>>,
{
    if shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        result = tokio::time::timeout(timeout, future) => {
            match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) | Err(_) => Err(failure),
            }
        }
    }
}

async fn await_channel_message(
    shared: &ConnectionShared,
    reader: &mut russh::ChannelReadHalf,
) -> Result<Option<ChannelMsg>, FlowFailure> {
    if shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        result = tokio::time::timeout(SSH_STAGE_TIMEOUT, reader.wait()) => {
            result.map_err(|_| FlowFailure::Transport)
        }
    }
}

/// Wait for channel traffic while the interactive shell is Ready.  Idle
/// sessions are valid: russh keepalives run on the client handle, so a lack of
/// channel bytes is not a transport timeout.
async fn wait_channel_message(
    shared: &ConnectionShared,
    reader: &mut russh::ChannelReadHalf,
) -> Result<Option<ChannelMsg>, FlowFailure> {
    if shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        message = reader.wait() => Ok(message),
    }
}

async fn wait_request_reply(
    shared: &ConnectionShared,
    reader: &mut russh::ChannelReadHalf,
) -> Result<(), FlowFailure> {
    loop {
        match await_channel_message(shared, reader).await? {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) | Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                return Err(FlowFailure::Pty);
            }
            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                if !registry::feed_remote(shared.terminal_id, shared.generation, &data) {
                    if registry::transport_overloaded(shared.terminal_id) {
                        return Err(FlowFailure::Transport);
                    }
                    return Err(FlowFailure::Stale);
                }
            }
            Some(_) => {}
        }
    }
}

async fn run_connection(shared: Arc<ConnectionShared>, options: ConnectOptions) {
    let result = run_connection_flow(Arc::clone(&shared), options).await;
    registry::detach_transport(shared.terminal_id, shared.generation);
    if shared.is_cancelled() {
        return;
    }
    match result {
        Ok(()) => shared.mark_disconnected(),
        Err(failure) => {
            let (code, message) = failure.details();
            shared.fail(code, message);
        }
    }
}

async fn run_connection_flow(
    shared: Arc<ConnectionShared>,
    options: ConnectOptions,
) -> Result<(), FlowFailure> {
    let ConnectOptions {
        host,
        port,
        username,
        mut private_key,
        mut passphrase,
        known_hosts_path: _,
    } = options;
    let decoded_key = keys::decode_secret_key(&private_key, passphrase.as_deref());
    // Clear the caller-provided PEM and passphrase before the first await, so
    // the long-lived interactive task retains only the parsed key and the
    // non-secret username/endpoint values.
    private_key.zeroize();
    passphrase.zeroize();
    let key = decoded_key.map_err(|_| FlowFailure::KeyFile)?;
    if shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }

    let config = client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..client::Config::default()
    };
    let socket = await_stage(
        &shared,
        tokio::net::TcpStream::connect((host.as_str(), port)),
        SSH_CONNECT_TIMEOUT,
        FlowFailure::Network,
    )
    .await?;
    if config.nodelay {
        let _ = socket.set_nodelay(true);
    }

    let control = Arc::new(ConnectIoControl::new(Instant::now() + SSH_CONNECT_TIMEOUT));
    let guard = ConnectStreamGuard::new(Arc::clone(&control));
    let stream = CancellableStream::new(socket, Arc::clone(&shared), Arc::clone(&control));
    let mut connect_future = Box::pin(client::connect_stream(
        Arc::new(config),
        stream,
        HostKeyHandler {
            shared: Arc::clone(&shared),
            setup: Arc::clone(&control),
        },
    ));
    let mut session = tokio::select! {
        _ = shared.cancelled() => {
            control.cancel();
            return Err(FlowFailure::Stale);
        }
        result = tokio::time::timeout(SSH_CONNECT_TIMEOUT, &mut connect_future) => {
            match result {
                Ok(Ok(session)) => session,
                Ok(Err(_)) | Err(_) => {
                    control.cancel();
                    return Err(FlowFailure::Network);
                }
            }
        }
    };
    // Keep the cancellation guard alive for the complete session lifetime;
    // its Drop path closes a russh task even when this outer future is
    // aborted between authentication and the bounded disconnect below.
    let _connect_guard = guard;
    control.clear_deadline();

    let result = run_authenticated_session(&shared, &username, key, &mut session).await;
    // Dropping a russh Handle does not synchronously stop its event loop.  A
    // bounded disconnect gives normal failures and explicit cancellation a
    // chance to close the owned session before this task exits.
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        session.disconnect(Disconnect::ByApplication, "meeterm", "en"),
    )
    .await;
    control.cancel();
    result
}

async fn run_authenticated_session(
    shared: &Arc<ConnectionShared>,
    username: &str,
    key: russh::keys::PrivateKey,
    session: &mut client::Handle<HostKeyHandler>,
) -> Result<(), FlowFailure> {
    shared.set_state(ConnectionState::Authenticating);
    let hash_alg = if key.algorithm().is_rsa() {
        await_stage(
            shared,
            session.best_supported_rsa_hash(),
            SSH_STAGE_TIMEOUT,
            FlowFailure::Authentication,
        )
        .await?
        .flatten()
    } else {
        None
    };
    let authentication = await_stage(
        shared,
        session.authenticate_publickey(
            username.to_owned(),
            PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
        ),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Authentication,
    )
    .await?;
    if !matches!(authentication, client::AuthResult::Success) {
        return Err(FlowFailure::Authentication);
    }
    if shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }

    shared.set_state(ConnectionState::OpeningPty);
    let channel = await_stage(
        shared,
        session.channel_open_session(),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
    )
    .await?;
    let (columns, rows) =
        registry::terminal_dimensions(shared.terminal_id).map_err(|_| FlowFailure::Stale)?;
    let (mut reader, writer) = channel.split();
    let (input_sender, mut input_receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    let (resize_sender, mut resize_receiver) = watch::channel((columns, rows));
    registry::attach_transport(
        shared.terminal_id,
        shared.generation,
        input_sender,
        resize_sender.clone(),
    )
    .map_err(|_| FlowFailure::Stale)?;

    // Bind before the requests so terminal-generated DA/DSR replies during
    // shell startup still reach the remote side.  User input remains rejected
    // until both request replies have been accepted below.
    await_stage(
        shared,
        writer.request_pty(
            true,
            "xterm-256color",
            u32::from(columns),
            u32::from(rows),
            0,
            0,
            &[],
        ),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Pty,
    )
    .await?;
    wait_request_reply(shared, &mut reader).await?;
    await_stage(
        shared,
        writer.request_shell(true),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Pty,
    )
    .await?;
    wait_request_reply(shared, &mut reader).await?;

    // A resize can race the PTY request.  Re-read after binding the transport
    // and use the watch channel's latest-value semantics to repair that race.
    if let Ok(latest) = registry::terminal_dimensions(shared.terminal_id)
        && latest != (columns, rows)
    {
        let _ = resize_sender.send(latest);
    }
    if !registry::mark_transport_ready(shared.terminal_id, shared.generation) {
        return Err(FlowFailure::Stale);
    }
    shared.set_state(ConnectionState::Ready);

    let mut saw_exit_status = false;
    let mut saw_eof = false;
    loop {
        tokio::select! {
            _ = shared.cancelled() => return Err(FlowFailure::Stale),
            input = input_receiver.recv() => {
                let Some(input) = input else {
                    return Err(FlowFailure::Transport);
                };
                await_stage(
                    shared,
                    writer.data_bytes(input),
                    SSH_STAGE_TIMEOUT,
                    FlowFailure::Transport,
                )
                .await?;
            }
            resize = resize_receiver.changed() => {
                resize.map_err(|_| FlowFailure::Transport)?;
                let (columns, rows) = *resize_receiver.borrow_and_update();
                await_stage(
                    shared,
                    writer.window_change(u32::from(columns), u32::from(rows), 0, 0),
                    SSH_STAGE_TIMEOUT,
                    FlowFailure::Transport,
                )
                .await?;
            }
            message = async {
                if saw_eof {
                    // EOF is the remote side's half-close.  Give russh a
                    // bounded window to deliver ExitStatus/Close, then treat
                    // an idle post-EOF channel as an orderly shell exit.
                    await_channel_message(shared, &mut reader).await
                } else {
                    wait_channel_message(shared, &mut reader).await
                }
            } => {
                let message = match message {
                    Ok(message) => message,
                    Err(FlowFailure::Transport) if saw_eof => return Ok(()),
                    Err(failure) => return Err(failure),
                };
                match message {
                    Some(ChannelMsg::Data { data })
                    | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if !registry::feed_remote(shared.terminal_id, shared.generation, &data) {
                            if registry::transport_overloaded(shared.terminal_id) {
                                return Err(FlowFailure::Transport);
                            }
                            return Err(FlowFailure::Stale);
                        }
                    }
                    Some(ChannelMsg::ExitStatus { .. }) => {
                        saw_exit_status = true;
                    }
                    Some(ChannelMsg::Eof) => {
                        saw_eof = true;
                    }
                    Some(ChannelMsg::Close) | None => {
                        if saw_exit_status || saw_eof {
                            return Ok(());
                        }
                        return Err(FlowFailure::RemoteClosed);
                    }
                    Some(_) => {}
                }
            }
        }
    }
}

fn canonical_host(host: &str) -> Result<String, ConnectionError> {
    if host.is_empty() || invalid_identity_component(host) {
        return Err(ConnectionError::InvalidArgument);
    }
    Ok(host.to_ascii_lowercase())
}

fn invalid_identity_component(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
}

fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

fn copy_string(destination: &mut [u8], length: &mut u16, value: &str) {
    destination.fill(0);
    let mut end = value.len().min(destination.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    destination[..end].copy_from_slice(&value.as_bytes()[..end]);
    *length = u16::try_from(end).unwrap_or(u16::MAX);
}

mod trust {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    static TRUST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct Record {
        line: usize,
        host_field: String,
        key: PublicKey,
    }

    struct FileState {
        records: Vec<Record>,
        non_comment_lines: Vec<usize>,
    }

    pub(super) enum Decision {
        Trusted,
        Unknown,
        Changed { known_fingerprint: String },
    }

    #[derive(Debug)]
    pub(super) enum Error {
        Io,
        Corrupt,
        Changed,
    }

    fn lock() -> Result<std::sync::MutexGuard<'static, ()>, Error> {
        TRUST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| Error::Io)
    }

    fn read_state(path: &Path) -> Result<Option<FileState>, Error> {
        match fs::read(path) {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(|_| Error::Corrupt)?;
                let mut records = Vec::new();
                let mut non_comment_lines = Vec::new();
                for (index, raw_line) in text.split('\n').enumerate() {
                    let line_number = index + 1;
                    let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
                    if line.starts_with('#') {
                        continue;
                    }
                    if line.trim_start().starts_with('#') {
                        // Leading-space comments are not in the small parser's
                        // supported format.  Rejecting them avoids ambiguity
                        // when deleting exact line numbers.
                        return Err(Error::Corrupt);
                    }
                    non_comment_lines.push(line_number);
                    if line.trim().is_empty() {
                        continue;
                    }

                    let mut fields = line.split_whitespace();
                    let host_field = fields.next().ok_or(Error::Corrupt)?;
                    let _algorithm = fields.next().ok_or(Error::Corrupt)?;
                    let encoded_key = fields.next().ok_or(Error::Corrupt)?;
                    if host_field.starts_with('@')
                        || host_field.contains('*')
                        || host_field.contains('?')
                        || host_field.contains('!')
                    {
                        return Err(Error::Corrupt);
                    }
                    let key =
                        keys::parse_public_key_base64(encoded_key).map_err(|_| Error::Corrupt)?;
                    records.push(Record {
                        line: line_number,
                        host_field: host_field.to_owned(),
                        key,
                    });
                }
                Ok(Some(FileState {
                    records,
                    non_comment_lines,
                }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Error::Io),
        }
    }

    fn host_token(host: &str, port: u16) -> String {
        if port == 22 {
            host.to_owned()
        } else {
            format!("[{host}]:{port}")
        }
    }

    fn direct_match(host_field: &str, token: &str) -> bool {
        host_field
            .split(',')
            .any(|entry| entry.eq_ignore_ascii_case(token))
    }

    fn matching(host: &str, port: u16, path: &Path) -> Result<Vec<(usize, PublicKey)>, Error> {
        let Some(state) = read_state(path)? else {
            return Ok(Vec::new());
        };
        let token = host_token(host, port);
        let mut matches = state
            .records
            .iter()
            .filter(|record| direct_match(&record.host_field, &token))
            .map(|record| (record.line, record.key.clone()))
            .collect::<Vec<_>>();

        // Russh handles OpenSSH's |1| hashed host form.  Its public helper's
        // line counter intentionally skips comments, so translate that
        // counter through the strict parser's exact physical line map before
        // exposing records to forget().
        let hashed = keys::known_hosts::known_host_keys_path(host, port, path)
            .map_err(|_| Error::Corrupt)?;
        for (logical_line, key) in hashed {
            let actual_line = *state
                .non_comment_lines
                .get(logical_line.saturating_sub(1))
                .ok_or(Error::Corrupt)?;
            let record = state
                .records
                .iter()
                .find(|record| record.line == actual_line)
                .ok_or(Error::Corrupt)?;
            if record.key != key {
                return Err(Error::Corrupt);
            }
            if !matches.iter().any(|(line, _)| *line == actual_line) {
                matches.push((actual_line, key));
            }
        }
        Ok(matches)
    }

    pub(super) fn assess(
        host: &str,
        port: u16,
        key: &PublicKey,
        path: &Path,
    ) -> Result<Decision, Error> {
        let _guard = lock()?;
        let records = matching(host, port, path)?;
        if records.is_empty() {
            return Ok(Decision::Unknown);
        }
        if records.iter().any(|(_, recorded)| recorded == key) {
            return Ok(Decision::Trusted);
        }
        let known_fingerprint = records
            .first()
            .map(|(_, recorded)| fingerprint(recorded))
            .ok_or(Error::Corrupt)?;
        Ok(Decision::Changed { known_fingerprint })
    }

    pub(super) fn learn(host: &str, port: u16, key: &PublicKey, path: &Path) -> Result<(), Error> {
        let _guard = lock()?;
        let records = matching(host, port, path)?;
        if records.iter().any(|(_, recorded)| recorded == key) {
            return Ok(());
        }
        if !records.is_empty() {
            return Err(Error::Changed);
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| Error::Io)?;
        }
        let existing = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(Error::Io),
        };
        let encoded_key = key.to_openssh().map_err(|_| Error::Io)?;
        let mut replacement = Vec::with_capacity(existing.len() + encoded_key.len() + 64);
        replacement.extend_from_slice(&existing);
        if !replacement.is_empty() && !replacement.ends_with(b"\n") {
            replacement.push(b'\n');
        }
        replacement.extend_from_slice(host_token(host, port).as_bytes());
        replacement.push(b' ');
        replacement.extend_from_slice(encoded_key.as_bytes());
        replacement.push(b'\n');
        atomic_replace(path, &replacement).map_err(|_| Error::Io)?;
        let file = File::open(path).map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        let records = matching(host, port, path)?;
        if records.iter().any(|(_, recorded)| recorded == key) {
            Ok(())
        } else {
            Err(Error::Corrupt)
        }
    }

    pub(super) fn forget(host: &str, port: u16, path: &Path) -> Result<(), Error> {
        let _guard = lock()?;
        let records = matching(host, port, path)?;
        if records.is_empty() {
            return Ok(());
        }
        let bytes = fs::read(path).map_err(|_| Error::Io)?;
        let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
        let remove: Vec<usize> = records.into_iter().map(|(line, _)| line).collect();
        let mut replacement = Vec::with_capacity(bytes.len());
        for (index, line) in lines.iter().enumerate() {
            let line_number = index + 1;
            if !remove.contains(&line_number) {
                replacement.extend_from_slice(line);
            }
        }
        atomic_replace(path, &replacement).map_err(|_| Error::Io)
    }

    fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("known_hosts");
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".{name}.meeterm-{sequence}.tmp"));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            // Directory fsync is best effort on mobile filesystems. The file
            // itself is durable before rename; syncing the parent closes the
            // rename durability window where the platform supports it.
            if let Ok(directory) = File::open(parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const KEY_ONE: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_TWO: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    fn path(name: &str) -> PathBuf {
        let process_id = std::process::id();
        for attempt in 0..16 {
            let root = std::env::temp_dir().join(format!(
                "meeterm-core-ssh-{name}-{process_id}-{}-{attempt}",
                next_generation()
            ));
            match fs::create_dir(&root) {
                Ok(()) => return root.join("known_hosts"),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("temporary trust directory: {error}"),
            }
        }
        panic!("could not allocate a unique temporary trust directory");
    }

    fn key(value: &str) -> PublicKey {
        keys::parse_public_key_base64(value).expect("test public key")
    }

    #[test]
    fn disconnect_cancels_selected_generation_before_replacement() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let terminal_id = 9001;
        let old_shared = Arc::new(ConnectionShared::new(
            terminal_id,
            1,
            "old.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/old-known-hosts"),
        ));
        let old_abort = runtime.spawn(std::future::pending::<()>()).abort_handle();
        let mut entries = HashMap::new();
        entries.insert(
            terminal_id,
            ConnectionEntry {
                shared: Arc::clone(&old_shared),
                abort: old_abort,
            },
        );

        let selected = cancel_entry_locked(&mut entries, terminal_id).expect("old entry");
        assert_eq!(selected.generation, 1);
        assert!(selected.is_cancelled());

        let new_shared = Arc::new(ConnectionShared::new(
            terminal_id,
            2,
            "new.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/new-known-hosts"),
        ));
        let new_abort = runtime.spawn(std::future::pending::<()>()).abort_handle();
        entries.insert(
            terminal_id,
            ConnectionEntry {
                shared: Arc::clone(&new_shared),
                abort: new_abort,
            },
        );

        // The deterministic replacement happens only after the selected old
        // entry was cancelled; the new generation remains live.
        assert!(!new_shared.is_cancelled());
        assert_eq!(entries[&terminal_id].shared.generation, 2);
        entries[&terminal_id].abort.abort();
    }

    #[test]
    fn cancellation_prevents_delayed_state_callbacks_after_disconnect() {
        let shared = ConnectionShared::new(
            9002,
            1,
            "example.test".to_owned(),
            22,
            PathBuf::from("/tmp/example-known-hosts"),
        );
        shared.mark_closing();
        shared.cancel();

        // These represent trust/auth callbacks that were already in flight
        // when disconnect won the lifecycle race.
        shared.set_state(ConnectionState::Ready);
        shared.set_host_key("SHA256/new".to_owned(), "ssh-ed25519".to_owned());
        let (sender, _receiver) = oneshot::channel();
        assert!(!shared.begin_host_prompt(
            "SHA256/pending".to_owned(),
            "ssh-ed25519".to_owned(),
            sender,
        ));
        shared.set_changed_key(
            "SHA256/changed".to_owned(),
            "ssh-ed25519".to_owned(),
            "SHA256/known".to_owned(),
        );
        shared.fail("transport", "should stay disconnected");

        let snapshot = shared.snapshot().expect("state snapshot");
        assert_eq!(snapshot.state, ConnectionState::Closing as u32);
        assert_eq!(snapshot.fingerprint_len, 0);
        assert_eq!(snapshot.error_code_len, 0);
    }

    #[test]
    fn cancelled_russh_connect_stream_closes_repeated_silent_peers() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        runtime.block_on(async {
            for generation in 1..=3 {
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind silent peer");
                let address = listener.local_addr().expect("silent peer address");
                let (kex_sender, kex_receiver) = oneshot::channel();
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.expect("accept client");
                    let mut client_id = Vec::new();
                    loop {
                        let mut byte = [0_u8; 1];
                        let count = socket.read(&mut byte).await.expect("read client ID");
                        assert_eq!(count, 1, "client closed before SSH ID");
                        client_id.push(byte[0]);
                        assert!(client_id.len() <= 256, "client ID is too long");
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    socket
                        .write_all(b"SSH-2.0-meeterm-silent\r\n")
                        .await
                        .expect("write server ID");
                    let mut first_kex_byte = [0_u8; 1];
                    let first_kex_count = tokio::time::timeout(
                        Duration::from_secs(2),
                        socket.read(&mut first_kex_byte),
                    )
                    .await
                    .expect("client KEX start timeout")
                    .expect("read first client KEX byte");
                    assert_eq!(first_kex_count, 1, "client did not start KEX");
                    let _ = kex_sender.send(());

                    // Cancellation can race with the rest of the KEX packet,
                    // so drain boundedly until the wrapper closes the socket.
                    let mut drained = first_kex_count;
                    let mut bytes = [0_u8; 1024];
                    loop {
                        let result =
                            tokio::time::timeout(Duration::from_secs(2), socket.read(&mut bytes))
                                .await
                                .expect("client socket close timeout");
                        match result {
                            Ok(0) => break,
                            Ok(count) => {
                                drained += count;
                                assert!(drained <= 64 * 1024, "silent peer was not bounded");
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                                break;
                            }
                            Err(error) => panic!("unexpected silent peer socket error: {error}"),
                        }
                    }
                });

                let shared = Arc::new(ConnectionShared::new(
                    9003,
                    generation,
                    "silent.test".to_owned(),
                    address.port(),
                    PathBuf::from("/tmp/silent-known-hosts"),
                ));
                let socket = tokio::net::TcpStream::connect(address)
                    .await
                    .expect("connect silent peer");
                let control = Arc::new(ConnectIoControl::new(
                    Instant::now() + Duration::from_secs(10),
                ));
                let stream =
                    CancellableStream::new(socket, Arc::clone(&shared), Arc::clone(&control));
                let config = Arc::new(client::Config::default());
                let handler_shared = Arc::clone(&shared);
                let client_task = tokio::spawn(async move {
                    let guard = ConnectStreamGuard::new(Arc::clone(&control));
                    let result = client::connect_stream(
                        config,
                        stream,
                        HostKeyHandler {
                            shared: handler_shared,
                            setup: control,
                        },
                    )
                    .await;
                    drop(guard);
                    result
                });

                tokio::time::timeout(Duration::from_secs(1), kex_receiver)
                    .await
                    .expect("silent peer KEX timeout")
                    .expect("silent peer KEX sender");
                shared.cancel();
                let result = tokio::time::timeout(Duration::from_secs(1), client_task)
                    .await
                    .expect("cancelled russh task timeout")
                    .expect("cancelled russh task join");
                assert!(result.is_err(), "silent peer unexpectedly completed KEX");
                server.await.expect("silent peer server task");
            }
        });
    }

    #[test]
    fn setup_deadline_wakes_a_pending_stream_read() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");

        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind silent peer");
            let address = listener.local_addr().expect("silent peer address");
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept client");
                let mut bytes = [0_u8; 1];
                let result = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut bytes))
                    .await
                    .expect("deadline did not close socket");
                match result {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    Ok(count) => assert_eq!(count, 0, "deadline test peer received data"),
                    Err(error) => panic!("unexpected deadline peer socket error: {error}"),
                }
            });

            let socket = tokio::net::TcpStream::connect(address)
                .await
                .expect("connect silent peer");
            let shared = Arc::new(ConnectionShared::new(
                9004,
                1,
                "silent.test".to_owned(),
                address.port(),
                PathBuf::from("/tmp/silent-known-hosts"),
            ));
            let control = Arc::new(ConnectIoControl::new(
                Instant::now() + Duration::from_millis(30),
            ));
            let mut stream = CancellableStream::new(socket, shared, control);
            let mut bytes = [0_u8; 1];
            let result = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut bytes))
                .await
                .expect("setup deadline read timeout");
            assert_eq!(
                result
                    .expect_err("silent stream unexpectedly returned data")
                    .kind(),
                std::io::ErrorKind::TimedOut
            );
            drop(stream);
            server.await.expect("deadline peer server task");
        });
    }

    #[test]
    fn trust_file_is_tofu_then_pinned_and_changed_keys_fail_closed() {
        let path = path("trust");
        let first = key(KEY_ONE);
        let second = key(KEY_TWO);

        assert!(matches!(
            trust::assess("example.test", 2222, &first, &path),
            Ok(trust::Decision::Unknown)
        ));
        trust::learn("example.test", 2222, &first, &path).expect("learn first key");
        assert!(matches!(
            trust::assess("EXAMPLE.TEST", 2222, &first, &path),
            Ok(trust::Decision::Trusted)
        ));
        let changed = trust::assess("example.test", 2222, &second, &path);
        assert!(matches!(changed, Ok(trust::Decision::Changed { .. })));
        assert!(std::str::from_utf8(&fs::read(&path).expect("trust file")).is_ok());
    }

    #[test]
    fn corrupt_trust_bytes_are_rejected() {
        let path = path("corrupt");
        fs::write(&path, [0xff, 0xfe]).expect("write corrupt trust file");
        let first = key(KEY_ONE);
        assert!(matches!(
            trust::assess("example.test", 22, &first, &path),
            Err(trust::Error::Corrupt)
        ));
    }

    #[test]
    fn forget_removes_matching_host_without_touching_other_hosts() {
        let path = path("forget");
        let first = key(KEY_ONE);
        trust::learn("example.test", 2222, &first, &path).expect("learn key");
        trust::learn("other.test", 2222, &first, &path).expect("learn other key");
        forget_host_key("EXAMPLE.TEST", 2222, &path).expect("forget key");
        let contents = fs::read_to_string(&path).expect("read trust file");
        assert!(!contents.contains("[example.test]:2222"));
        assert!(contents.contains("[other.test]:2222"));
    }

    #[cfg(unix)]
    #[test]
    fn learned_trust_file_is_private_and_atomic_record_is_parseable() {
        use std::os::unix::fs::PermissionsExt;

        let path = path("atomic");
        let first = key(KEY_ONE);
        trust::learn("example.test", 2222, &first, &path).expect("learn key");
        assert_eq!(
            fs::metadata(&path)
                .expect("trust metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(matches!(
            trust::assess("example.test", 2222, &first, &path),
            Ok(trust::Decision::Trusted)
        ));
    }
}
