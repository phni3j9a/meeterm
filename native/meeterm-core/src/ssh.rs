//! Rust-owned SSH lifecycle and host-key trust policy.
//!
//! This module deliberately exposes a small control plane.  Terminal bytes
//! remain in the registry and are exchanged with russh through bounded native
//! queues; callers poll the fixed connection snapshot and terminal revision.

mod control;
mod herdr_control;

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
use zeroize::{Zeroize, Zeroizing};

use crate::registry::{self, TerminalId};
use crate::terminal::INPUT_QUEUE_CAPACITY;
use crate::tmux::{self, PaneSnapshot, SessionSnapshot, WindowSnapshot};
use crate::workspace::{self, Backend, RuntimeSnapshot};

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
    AttachingTmux = 8,
    Synchronizing = 9,
    Reconnecting = 10,
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

/// Authentication credentials accepted by the native SSH connect entry point.
///
/// Secret values are wrapped in [`Zeroizing`] so an aborted or failed connect
/// still clears credentials that have not yet reached the SSH task.  The enum
/// keeps authentication method selection explicit: a password is never used
/// as a fallback for a key, and a key is never tried for a password request.
#[derive(Clone)]
pub enum AuthOptions {
    PublicKey {
        private_key: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
    Password {
        password: Zeroizing<String>,
    },
}

impl AuthOptions {
    /// Build a public-key credential while keeping the existing Rust-facing
    /// string API convenient for callers and tests.
    pub fn public_key(private_key: String, passphrase: Option<String>) -> Self {
        Self::PublicKey {
            private_key: Zeroizing::new(private_key),
            passphrase: passphrase.map(Zeroizing::new),
        }
    }

    /// Build a password credential without trimming or otherwise normalizing
    /// the password. Whitespace is valid SSH password input.
    pub fn password(password: String) -> Self {
        Self::Password {
            password: Zeroizing::new(password),
        }
    }

    fn validate(&self) -> Result<(), ConnectionError> {
        match self {
            Self::PublicKey {
                private_key,
                passphrase,
            } => {
                if private_key.is_empty()
                    || passphrase
                        .as_deref()
                        .is_some_and(|passphrase| passphrase.contains('\0'))
                {
                    return Err(ConnectionError::InvalidArgument);
                }
            }
            Self::Password { password } => {
                // Empty passwords are rejected locally. A whitespace-only
                // password remains valid and is passed byte-for-byte to SSH.
                if password.is_empty() || password.contains('\0') {
                    return Err(ConnectionError::InvalidArgument);
                }
            }
        }
        Ok(())
    }
}

/// Options accepted by the native SSH connect entry point.
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub credentials: AuthOptions,
    pub known_hosts_path: PathBuf,
    pub backend: Backend,
    pub runtime: Option<String>,
}

impl ConnectOptions {
    fn validate(self) -> Result<Self, ConnectionError> {
        let host = canonical_host(&self.host)?;
        if self.port == 0 || self.username.is_empty() || invalid_identity_component(&self.username)
        {
            return Err(ConnectionError::InvalidArgument);
        }
        if self.known_hosts_path.as_os_str().is_empty() {
            return Err(ConnectionError::InvalidArgument);
        }
        self.credentials.validate()?;
        let runtime = self.runtime.filter(|value| {
            !(value.is_empty() || self.backend == Backend::Herdr && value == "default")
        });
        if let Some(name) = runtime.as_deref()
            && (self.backend != Backend::Herdr
                || name.len() > 64
                || name == "."
                || name == ".."
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
        {
            return Err(ConnectionError::InvalidArgument);
        }
        Ok(Self {
            host,
            runtime,
            ..self
        })
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
    ReconnectUnavailable,
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
            Self::ReconnectUnavailable => -7,
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
            Self::ReconnectUnavailable => "reconnect_unavailable",
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
            Self::ReconnectUnavailable => "no in-memory credentials are available for reconnect",
        })
    }
}

impl std::error::Error for ConnectionError {}

/// Credentials retained only for an in-process reconnect. The private key is
/// parsed before it reaches this structure; no PEM or passphrase survives the
/// initial connect call. Password credentials remain in a process-local
/// [`Zeroizing`] buffer for the lifetime of the reconnect profile and are
/// removed when the owner terminal is destroyed.
#[derive(Clone)]
struct ConnectionProfile {
    host: String,
    port: u16,
    username: String,
    known_hosts_path: PathBuf,
    credentials: StoredCredentials,
    backend: Backend,
    runtime: Option<String>,
}

/// The last explicitly requested endpoint.  This remains after credentials
/// are invalidated so a subsequent connection to another host cannot inherit
/// the previous endpoint's pane topology.  It contains no authentication
/// material.
#[derive(Clone, PartialEq, Eq)]
struct SessionEndpoint {
    host: String,
    port: u16,
    username: String,
    known_hosts_path: PathBuf,
    backend: Backend,
    runtime: Option<String>,
}

impl SessionEndpoint {
    fn from_options(options: &ConnectOptions) -> Self {
        Self {
            host: options.host.clone(),
            port: options.port,
            username: options.username.clone(),
            known_hosts_path: options.known_hosts_path.clone(),
            backend: options.backend,
            runtime: options.runtime.clone(),
        }
    }

    fn matches(&self, options: &ConnectOptions) -> bool {
        self.host == options.host
            && self.port == options.port
            && self.username == options.username
            && self.known_hosts_path == options.known_hosts_path
            && self.backend == options.backend
            && self.runtime == options.runtime
    }
}

#[derive(Clone)]
enum StoredCredentials {
    PublicKey { key: Arc<keys::PrivateKey> },
    Password { password: Arc<Zeroizing<String>> },
}

/// Durable in-process metadata associated with one owner terminal.  The
/// actual durable workspace remains tmux; this map only retains native IDs so
/// reconnecting the same owner can bind the same pane IDs back to the same
/// native terminal objects.
struct SessionState {
    viewport: Option<(u16, u16)>,
    generation: u64,
    snapshot: SessionSnapshot,
    herdr: herdr_control::Metadata,
    pane_terminals: HashMap<u64, TerminalId>,
    endpoint: Option<SessionEndpoint>,
    profile: Option<ConnectionProfile>,
    selected_pane: Option<u64>,
    /// True only when meeterm has zoomed the current window.  A desktop user's
    /// pre-existing zoom is observed but never claimed for cleanup.
    meeterm_zoomed: bool,
    meeterm_zoomed_pane: Option<u64>,
    foreground: bool,
    automatic_reconnect: bool,
    terminal_visible: bool,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            viewport: None,
            generation: 0,
            snapshot: SessionSnapshot::default(),
            herdr: herdr_control::Metadata::default(),
            pane_terminals: HashMap::new(),
            endpoint: None,
            profile: None,
            selected_pane: None,
            meeterm_zoomed: false,
            meeterm_zoomed_pane: None,
            foreground: true,
            automatic_reconnect: true,
            terminal_visible: true,
        }
    }
}

static SESSION_STATES: OnceLock<Mutex<HashMap<TerminalId, Arc<Mutex<SessionState>>>>> =
    OnceLock::new();

fn session_states() -> &'static Mutex<HashMap<TerminalId, Arc<Mutex<SessionState>>>> {
    SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_state(owner: TerminalId) -> Arc<Mutex<SessionState>> {
    let mut states = session_states()
        .lock()
        .expect("session state registry lock should not be poisoned");
    states
        .entry(owner)
        .or_insert_with(|| Arc::new(Mutex::new(SessionState::default())))
        .clone()
}

struct PendingHostKey {
    fingerprint: String,
    response: oneshot::Sender<HostKeyDecision>,
}

struct HostKeyDecision {
    accept: bool,
}

struct ConnectionInfo {
    finished: bool,
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
            finished: false,
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
    session: Arc<Mutex<SessionState>>,
    info: Mutex<ConnectionInfo>,
    commands: Mutex<Option<mpsc::Sender<ControlCommand>>>,
    cancelled: AtomicBool,
    cancel_notify: Arc<Notify>,
    finished_notify: Arc<Notify>,
    retry_notify: Arc<Notify>,
    foreground: AtomicBool,
    automatic_reconnect: AtomicBool,
    ready_once: AtomicBool,
    ready_epoch: AtomicU64,
}

impl ConnectionShared {
    fn new(
        terminal_id: TerminalId,
        generation: u64,
        host: String,
        port: u16,
        known_hosts_path: PathBuf,
    ) -> Self {
        let session = session_state(terminal_id);
        let (foreground, automatic_reconnect) = session
            .lock()
            .map(|state| (state.foreground, state.automatic_reconnect))
            .unwrap_or((true, true));
        Self {
            terminal_id,
            generation,
            host: host.clone(),
            port,
            known_hosts_path,
            session,
            info: Mutex::new(ConnectionInfo::new(host, port)),
            commands: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            cancel_notify: Arc::new(Notify::new()),
            finished_notify: Arc::new(Notify::new()),
            retry_notify: Arc::new(Notify::new()),
            foreground: AtomicBool::new(foreground),
            automatic_reconnect: AtomicBool::new(automatic_reconnect),
            ready_once: AtomicBool::new(false),
            ready_epoch: AtomicU64::new(0),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancel_with_state();
    }

    fn cancel_with_state(&self) {
        // State setters take this same lock before checking cancellation. By
        // setting the flag while holding it, no delayed trust/auth callback
        // can write a nonterminal state after cancellation has committed.
        if let Ok(mut info) = self.info.lock() {
            self.cancelled.store(true, Ordering::Release);
            info.state = if info.finished {
                ConnectionState::Disconnected
            } else {
                ConnectionState::Closing
            };
            info.pending = None;
        } else {
            self.cancelled.store(true, Ordering::Release);
        }
        self.cancel_notify.notify_waiters();
        self.retry_notify.notify_waiters();
    }

    fn set_profile(&self, profile: ConnectionProfile) {
        if let Ok(mut session) = self.session.lock()
            && session.generation == self.generation
            && !self.is_cancelled()
        {
            session.profile = Some(profile);
        }
    }

    fn set_commands(&self, sender: mpsc::Sender<ControlCommand>) {
        if let Ok(mut commands) = self.commands.lock() {
            *commands = Some(sender);
        }
    }

    fn clear_commands(&self) {
        if let Ok(mut commands) = self.commands.lock() {
            *commands = None;
        }
    }

    fn clear_owned_zoom(&self) {
        // A transport failure can leave the Control Mode actor without a
        // chance to run its cancellation branch.  Clear only the ownership
        // belonging to this generation so a later connection cannot inherit
        // stale cleanup authority for the same terminal ID.
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
        {
            state.meeterm_zoomed = false;
            state.meeterm_zoomed_pane = None;
        }
    }

    fn command_sender(&self) -> Option<mpsc::Sender<ControlCommand>> {
        self.commands
            .lock()
            .ok()
            .and_then(|commands| commands.clone())
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

    async fn finished(&self) {
        loop {
            let notified = self.finished_notify.notified();
            tokio::pin!(notified);
            // Register before checking the mutex-backed flag so completion
            // cannot land between the check and the wait.
            notified.as_mut().enable();
            let is_finished = self.info.lock().map(|info| info.finished).unwrap_or(true);
            if is_finished {
                return;
            }
            notified.await;
        }
    }

    fn set_state(&self, state: ConnectionState) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() || info.finished {
                return;
            }
            info.state = state;
            if state == ConnectionState::Ready {
                self.ready_once.store(true, Ordering::Release);
                self.ready_epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    fn mark_reconnecting(&self) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() || info.finished {
                return;
            }
            info.state = ConnectionState::Reconnecting;
            info.error_code.clear();
            info.error_message.clear();
            info.pending = None;
        }
    }

    fn set_foreground(&self, foreground: bool) {
        self.foreground.store(foreground, Ordering::Release);
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
        {
            state.foreground = foreground;
        }
        self.retry_notify.notify_waiters();
    }

    fn set_automatic_reconnect(&self, enabled: bool) {
        self.automatic_reconnect.store(enabled, Ordering::Release);
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
        {
            state.automatic_reconnect = enabled;
        }
        self.retry_notify.notify_waiters();
    }

    fn is_foreground(&self) -> bool {
        self.foreground.load(Ordering::Acquire)
    }

    fn automatic_reconnect_enabled(&self) -> bool {
        self.automatic_reconnect.load(Ordering::Acquire)
    }

    fn has_been_ready(&self) -> bool {
        self.ready_once.load(Ordering::Acquire)
    }

    fn ready_epoch(&self) -> u64 {
        self.ready_epoch.load(Ordering::Acquire)
    }

    fn set_host_key(&self, fingerprint: String, algorithm: String) {
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() || info.finished {
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
        if self.is_cancelled() || info.finished {
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
            if self.is_cancelled() || info.finished {
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
            if self.is_cancelled() || info.finished {
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
            if !info.finished {
                info.state = ConnectionState::Closing;
            }
            info.pending = None;
        }
    }

    fn finish(&self, result: Result<(), FlowFailure>) {
        if let Ok(mut info) = self.info.lock() {
            // Completion and cancellation commit under the same lock. A late
            // disconnect must not leave a finished actor permanently Closing.
            info.finished = true;
            match result {
                _ if self.is_cancelled() => info.state = ConnectionState::Disconnected,
                Ok(()) => info.state = ConnectionState::Disconnected,
                Err(failure) if info.state != ConnectionState::Failed => {
                    let (code, message) = failure.details();
                    info.state = ConnectionState::Failed;
                    info.error_code = code.to_owned();
                    info.error_message = message.to_owned();
                }
                Err(_) => {}
            }
            info.pending = None;
        }
        self.finished_notify.notify_waiters();
    }

    fn snapshot(&self) -> Result<ConnectionSnapshot, ConnectionError> {
        self.info
            .lock()
            .map(|info| info.snapshot())
            .map_err(|_| ConnectionError::Internal)
    }
}

enum ControlCommand {
    SelectPane { window_id: u64, pane_id: u64 },
    CreateWorkspace { name: String },
    RenameWorkspace { window_id: u64, name: String },
    CloseWorkspace { window_id: u64 },
    CreatePane { window_id: u64 },
    RenamePane { pane_id: u64, name: String },
    ClosePane { pane_id: u64 },
    RefreshTerminal,
    CreateGroup { window_id: u64, name: String },
    RenameGroup { group_id: u64, name: String },
    CloseGroup { group_id: u64 },
    SelectGroup { group_id: u64 },
    SetTerminalVisible { visible: bool },
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
    start_connection(terminal_id, ConnectionStart::Options(options), false)
}

/// Reconnect using the parsed private key retained by this process after a
/// successful (or partially established) connection.  Secrets are never
/// reconstructed into a PEM string and are never persisted by the core.
pub fn reconnect_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let profile = session_state(terminal_id)
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .profile
        .clone()
        .ok_or(ConnectionError::ReconnectUnavailable)?;
    start_connection(terminal_id, ConnectionStart::Profile(profile), true)
}

/// Select a pane by its stable tmux numeric ID. The desired selection is kept
/// while disconnected so the next reconnect restores the same mobile tab.
pub fn select_pane(terminal_id: TerminalId, pane_id: u64) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let sender = current_connection(terminal_id)
        .ok()
        .and_then(|shared| shared.command_sender());
    let state = session_state(terminal_id);
    let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
    let pane = state
        .snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .cloned()
        .ok_or(ConnectionError::InvalidArgument)?;
    if let Some(sender) = sender {
        sender
            .try_send(ControlCommand::SelectPane {
                window_id: pane.window_id,
                pane_id,
            })
            .map_err(|_| ConnectionError::Internal)?;
    }
    state.selected_pane = Some(pane_id);
    mark_selected(&mut state.snapshot, pane_id);
    if state
        .endpoint
        .as_ref()
        .is_some_and(|endpoint| endpoint.backend == Backend::Herdr)
    {
        for id in state.pane_terminals.values() {
            registry::detach_transport(*id, state.generation);
        }
        state.herdr.select(pane_id);
    }
    Ok(())
}

/// Create a tmux window for a new workspace. The command is queued on the
/// serialized Control Mode actor; topology is refreshed before the next
/// command is accepted, so callers never construct shell fragments locally.
pub fn create_workspace(terminal_id: TerminalId, name: &str) -> Result<(), ConnectionError> {
    validate_tmux_name(name)?;
    enqueue_control(
        terminal_id,
        ControlCommand::CreateWorkspace {
            name: name.to_owned(),
        },
    )
}

pub fn rename_workspace(
    terminal_id: TerminalId,
    window_id: u64,
    name: &str,
) -> Result<(), ConnectionError> {
    validate_tmux_name(name)?;
    ensure_window_target(terminal_id, window_id)?;
    enqueue_control(
        terminal_id,
        ControlCommand::RenameWorkspace {
            window_id,
            name: name.to_owned(),
        },
    )
}

pub fn close_workspace(terminal_id: TerminalId, window_id: u64) -> Result<(), ConnectionError> {
    ensure_window_target(terminal_id, window_id)?;
    enqueue_control(terminal_id, ControlCommand::CloseWorkspace { window_id })
}

pub fn create_pane(terminal_id: TerminalId, window_id: u64) -> Result<(), ConnectionError> {
    ensure_window_target(terminal_id, window_id)?;
    enqueue_control(terminal_id, ControlCommand::CreatePane { window_id })
}

pub fn rename_pane(
    terminal_id: TerminalId,
    pane_id: u64,
    name: &str,
) -> Result<(), ConnectionError> {
    validate_tmux_name(name)?;
    ensure_pane_target(terminal_id, pane_id)?;
    enqueue_control(
        terminal_id,
        ControlCommand::RenamePane {
            pane_id,
            name: name.to_owned(),
        },
    )
}

pub fn close_pane(terminal_id: TerminalId, pane_id: u64) -> Result<(), ConnectionError> {
    ensure_pane_target(terminal_id, pane_id)?;
    enqueue_control(terminal_id, ControlCommand::ClosePane { pane_id })
}

pub fn create_group(
    terminal_id: TerminalId,
    window_id: u64,
    name: &str,
) -> Result<(), ConnectionError> {
    validate_tmux_name(name)?;
    ensure_window_target(terminal_id, window_id)?;
    ensure_group_backend(terminal_id, None)?;
    enqueue_control(
        terminal_id,
        ControlCommand::CreateGroup {
            window_id,
            name: name.to_owned(),
        },
    )
}

pub fn rename_group(
    terminal_id: TerminalId,
    group_id: u64,
    name: &str,
) -> Result<(), ConnectionError> {
    validate_tmux_name(name)?;
    ensure_group_backend(terminal_id, Some(group_id))?;
    enqueue_control(
        terminal_id,
        ControlCommand::RenameGroup {
            group_id,
            name: name.to_owned(),
        },
    )
}

pub fn close_group(terminal_id: TerminalId, group_id: u64) -> Result<(), ConnectionError> {
    ensure_group_backend(terminal_id, Some(group_id))?;
    enqueue_control(terminal_id, ControlCommand::CloseGroup { group_id })
}

pub fn select_group(terminal_id: TerminalId, group_id: u64) -> Result<(), ConnectionError> {
    ensure_group_backend(terminal_id, Some(group_id))?;
    let pane = {
        let state = session_state(terminal_id);
        let state = state.lock().map_err(|_| ConnectionError::Internal)?;
        state
            .herdr
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.group_id == group_id.to_string() && terminal.active)
            .or_else(|| {
                state
                    .herdr
                    .snapshot
                    .terminals
                    .iter()
                    .find(|terminal| terminal.group_id == group_id.to_string())
            })
            .and_then(|terminal| terminal.id.parse().ok())
    };
    if let Some(pane) = pane {
        return select_pane(terminal_id, pane);
    }
    enqueue_control(terminal_id, ControlCommand::SelectGroup { group_id })
}

pub fn set_terminal_visible(terminal_id: TerminalId, visible: bool) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    {
        let state = session_state(terminal_id);
        let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
        state.terminal_visible = visible;
        if !visible
            && state
                .endpoint
                .as_ref()
                .is_some_and(|endpoint| endpoint.backend == Backend::Herdr)
        {
            for id in state.pane_terminals.values() {
                registry::detach_transport(*id, state.generation);
            }
        }
    }
    if let Ok(shared) = current_connection(terminal_id)
        && let Some(sender) = shared.command_sender()
    {
        sender
            .try_send(ControlCommand::SetTerminalVisible { visible })
            .map_err(|_| ConnectionError::Internal)?;
    }
    Ok(())
}

fn ensure_group_backend(owner: TerminalId, group_id: Option<u64>) -> Result<(), ConnectionError> {
    registry::shared_terminal(owner).map_err(map_terminal_error)?;
    let state = session_state(owner);
    let state = state.lock().map_err(|_| ConnectionError::Internal)?;
    if !state
        .endpoint
        .as_ref()
        .is_some_and(|endpoint| endpoint.backend == Backend::Herdr)
        || group_id.is_some_and(|id| {
            !state
                .herdr
                .snapshot
                .groups
                .iter()
                .any(|group| group.id == id.to_string())
        })
    {
        return Err(ConnectionError::InvalidArgument);
    }
    Ok(())
}

/// Ask the native Control Mode actor to recapture the selected pane. This is
/// the explicit redraw/recovery action used after a full-screen TUI loses its
/// local frame during a reconnect or lifecycle transition.
pub fn refresh_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    enqueue_control(terminal_id, ControlCommand::RefreshTerminal)
}

/// Mark whether the owning app is in the foreground. Automatic reconnects
/// wait in Rust while this is false and are resumed by the next foreground
/// transition; no timer state machine is needed in JavaScript.
pub fn set_foreground(terminal_id: TerminalId, foreground: bool) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&terminal_id)
        .map(|entry| Arc::clone(&entry.shared));
    if let Some(shared) = shared {
        shared.set_foreground(foreground);
    } else {
        let state = session_state(terminal_id);
        state
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .foreground = foreground;
    }
    Ok(())
}

/// Enable or disable bounded native reconnect attempts for this owner. The
/// preference is retained across explicit reconnects while credentials remain
/// in the process-local profile.
pub fn set_automatic_reconnect(
    terminal_id: TerminalId,
    enabled: bool,
) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&terminal_id)
        .map(|entry| Arc::clone(&entry.shared));
    if let Some(shared) = shared {
        shared.set_automatic_reconnect(enabled);
    } else {
        let state = session_state(terminal_id);
        state
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .automatic_reconnect = enabled;
    }
    Ok(())
}

fn validate_tmux_name(name: &str) -> Result<(), ConnectionError> {
    tmux::quote_tmux_argument(name)
        .map(|_| ())
        .map_err(|_| ConnectionError::InvalidArgument)
}

fn ensure_window_target(terminal_id: TerminalId, window_id: u64) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let state = session_state(terminal_id);
    let state = state.lock().map_err(|_| ConnectionError::Internal)?;
    state
        .snapshot
        .windows
        .iter()
        .any(|window| window.window_id == window_id)
        .then_some(())
        .ok_or(ConnectionError::InvalidArgument)
}

fn ensure_pane_target(terminal_id: TerminalId, pane_id: u64) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let state = session_state(terminal_id);
    let state = state.lock().map_err(|_| ConnectionError::Internal)?;
    state
        .snapshot
        .panes
        .iter()
        .any(|pane| pane.pane_id == pane_id)
        .then_some(())
        .ok_or(ConnectionError::InvalidArgument)
}

fn enqueue_control(
    terminal_id: TerminalId,
    command: ControlCommand,
) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let sender = current_connection(terminal_id)?
        .command_sender()
        .ok_or(ConnectionError::Internal)?;
    sender
        .try_send(command)
        .map_err(|_| ConnectionError::Internal)
}

/// Return the latest coherent tmux topology known to the native core.
pub fn session_snapshot(terminal_id: TerminalId) -> Result<SessionSnapshot, ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    session_state(terminal_id)
        .lock()
        .map(|state| state.snapshot.clone())
        .map_err(|_| ConnectionError::Internal)
}

pub fn workspace_snapshot_json(terminal_id: TerminalId) -> Result<String, ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let state = session_state(terminal_id);
    let state = state.lock().map_err(|_| ConnectionError::Internal)?;
    let snapshot = if let Some(endpoint) = state
        .endpoint
        .as_ref()
        .filter(|endpoint| endpoint.backend == Backend::Herdr)
    {
        let mut snapshot = state.herdr.snapshot.clone();
        snapshot.backend = Backend::Herdr;
        snapshot.runtime = endpoint
            .runtime
            .clone()
            .unwrap_or_else(|| "default".to_owned());
        snapshot.groups_supported = true;
        snapshot
    } else {
        RuntimeSnapshot::tmux(&state.snapshot)
    };
    serde_json::to_string(&snapshot).map_err(|_| ConnectionError::Internal)
}

fn prepare_session_endpoint(
    terminal_id: TerminalId,
    options: &ConnectOptions,
) -> Result<Vec<TerminalId>, ConnectionError> {
    let state = session_state(terminal_id);
    let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
    // Keep endpoint identity separately from credentials. An explicit connect
    // invalidates the old secret before parsing the new one, but a later
    // endpoint change still needs to know whether the retained pane topology
    // belongs to this host.
    let stale = state
        .endpoint
        .as_ref()
        .is_some_and(|endpoint| !endpoint.matches(options));
    // Every explicit connect replaces the selected credential. Clear the old
    // profile even when the endpoint is unchanged, so a malformed new key or
    // failed credential cannot leave an unrelated password/key available to
    // reconnect.
    state.endpoint = Some(SessionEndpoint::from_options(options));
    state.profile = None;
    if !stale {
        return Ok(Vec::new());
    }
    let stale_terminals = state
        .pane_terminals
        .drain()
        .filter_map(|(_, id)| (id != terminal_id).then_some(id))
        .collect::<Vec<_>>();
    state.generation = 0;
    state.snapshot = SessionSnapshot::default();
    state.herdr = herdr_control::Metadata::default();
    state.selected_pane = None;
    state.meeterm_zoomed = false;
    state.meeterm_zoomed_pane = None;
    Ok(stale_terminals)
}

enum ConnectionStart {
    Options(ConnectOptions),
    Profile(ConnectionProfile),
}

impl ConnectionStart {
    fn endpoint(&self) -> (&str, u16, &str, &Path) {
        match self {
            Self::Options(options) => (
                &options.host,
                options.port,
                &options.username,
                &options.known_hosts_path,
            ),
            Self::Profile(profile) => (
                &profile.host,
                profile.port,
                &profile.username,
                &profile.known_hosts_path,
            ),
        }
    }
}

/// Give the previous generation a bounded chance to send its tmux cleanup
/// before a replacement generation changes the shared session generation.
/// This runs the wait on a short-lived blocking helper thread so a native
/// caller that happens to be on a Tokio worker cannot starve the cancelled
/// Control Mode actor. The timeout is only a last-resort bound for a dead
/// transport; normal disconnects complete through `ConnectionShared::finish`.
fn wait_for_generation_finish(runtime: &'static Runtime, shared: Arc<ConnectionShared>) -> bool {
    let already_finished = shared.info.lock().map(|info| info.finished).unwrap_or(true);
    if already_finished {
        return true;
    }

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let finished = runtime.block_on(async {
            tokio::time::timeout(REPLACEMENT_GRACE_TIMEOUT, shared.finished())
                .await
                .is_ok()
        });
        let _ = sender.send(finished);
    });
    receiver
        .recv_timeout(REPLACEMENT_GRACE_TIMEOUT + Duration::from_millis(100))
        .unwrap_or(false)
}

fn start_connection(
    terminal_id: TerminalId,
    start: ConnectionStart,
    reconnecting: bool,
) -> Result<(), ConnectionError> {
    let runtime = runtime()?;
    let generation = next_generation();

    let (host, port, _username, known_hosts_path) = start.endpoint();

    let old = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .remove(&terminal_id);
    if let Some(old) = old {
        old.shared.mark_closing();
        old.shared.cancel();
        if !wait_for_generation_finish(runtime, Arc::clone(&old.shared)) {
            // A transport that never acknowledges cancellation cannot safely
            // retain the old generation. Cleanup is best effort in this
            // branch; the new generation still receives a fresh ownership
            // guard below.
            old.abort.abort();
        }
    }

    let stale_terminals = match &start {
        ConnectionStart::Options(options) => prepare_session_endpoint(terminal_id, options)?,
        ConnectionStart::Profile(_) => Vec::new(),
    };
    registry::begin_remote(terminal_id, generation).map_err(map_terminal_error)?;
    let shared = Arc::new(ConnectionShared::new(
        terminal_id,
        generation,
        host.to_owned(),
        port,
        known_hosts_path.to_owned(),
    ));
    {
        let mut state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        // Abort completion is asynchronous. Install the new generation and
        // discard the old actor's local cleanup authority in one operation.
        state.generation = generation;
        state.meeterm_zoomed = false;
        state.meeterm_zoomed_pane = None;
    }
    if reconnecting {
        shared.set_state(ConnectionState::Reconnecting);
    }
    let (command_sender, command_receiver) = mpsc::channel(32);
    shared.set_commands(command_sender);
    let task_shared = Arc::clone(&shared);
    let join = runtime.spawn(async move {
        run_connection(task_shared, start, command_receiver).await;
    });
    connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .insert(
            terminal_id,
            ConnectionEntry {
                shared,
                abort: join.abort_handle(),
            },
        );
    for id in stale_terminals {
        registry::destroy_terminal(id);
    }
    Ok(())
}

/// Abort the session and leave the terminal in remote mode with local echo
/// disabled.  The registry entry remains available for state polling.
pub fn disconnect_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = {
        let entries = connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let Some(entry) = entries.get(&terminal_id) else {
            return Ok(());
        };
        entry.shared.mark_closing();
        entry.shared.cancel();
        Arc::clone(&entry.shared)
    };

    detach_all(&shared);
    Ok(())
}

#[cfg(test)]
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
    if let Some(entry) = connections()
        .lock()
        .ok()
        .and_then(|mut entries| entries.remove(&terminal_id))
    {
        entry.shared.mark_closing();
        entry.shared.cancel();
        entry.abort.abort();
    }
    let stale_terminals = session_states()
        .lock()
        .ok()
        .and_then(|mut states| states.remove(&terminal_id))
        .and_then(|state| {
            state
                .lock()
                .ok()
                .map(|state| state.pane_terminals.values().copied().collect::<Vec<_>>())
        })
        .unwrap_or_default();
    for id in stale_terminals {
        if id != terminal_id {
            registry::destroy_terminal(id);
        }
    }
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

fn mark_selected(snapshot: &mut SessionSnapshot, pane_id: u64) {
    snapshot.selected_pane = Some(pane_id);
    for pane in &mut snapshot.panes {
        pane.selected = pane.pane_id == pane_id;
    }
    for window in &mut snapshot.windows {
        window.selected = window.panes.iter().any(|pane| pane.pane_id == pane_id);
        for pane in &mut window.panes {
            pane.selected = pane.pane_id == pane_id;
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
    Transport,
    RemoteClosed,
    Tmux,
    TmuxProtocol,
    HerdrMissing,
    HerdrSessionMissing,
    HerdrIncompatible,
    HerdrUnsupported,
    HerdrForwarding,
    HerdrProtocol,
    HerdrController,
    HerdrOperation,
    HerdrWorkspaceGroup,
    Stale,
}

impl FlowFailure {
    const fn details(self) -> (&'static str, &'static str) {
        match self {
            Self::KeyFile => ("key_file", "The private key could not be loaded."),
            Self::Network => ("network", "The SSH connection could not be established."),
            Self::Authentication => ("auth_failed", "SSH authentication failed."),
            Self::Channel => ("channel", "The SSH session channel could not be opened."),
            Self::Transport => ("transport", "The SSH terminal transport stopped."),
            Self::RemoteClosed => ("remote_closed", "The remote terminal closed the session."),
            Self::Tmux => (
                "tmux_failed",
                "The managed tmux session could not be opened.",
            ),
            Self::TmuxProtocol => (
                "tmux_protocol",
                "The tmux Control Mode stream was malformed.",
            ),
            Self::Stale => ("stale_connection", "The SSH connection was replaced."),
            Self::HerdrMissing => (
                "herdr_missing",
                "Herdr was not found in the remote SSH command path.",
            ),
            Self::HerdrSessionMissing => (
                "herdr_session_missing",
                "The selected Herdr session is not running. Open it on your PC first.",
            ),
            Self::HerdrIncompatible => (
                "herdr_incompatible",
                "This Herdr client/server does not support the verified terminal protocol (22).",
            ),
            Self::HerdrUnsupported => (
                "herdr_unsupported",
                "This Herdr runtime does not provide the required public API or event subscription.",
            ),
            Self::HerdrForwarding => (
                "herdr_forwarding",
                "SSH access to the Herdr Unix socket was refused. AllowStreamLocalForwarding is required.",
            ),
            Self::HerdrProtocol => (
                "herdr_protocol",
                "The Herdr response was malformed or exceeded the supported bounds.",
            ),
            Self::HerdrController => (
                "herdr_controller_busy",
                "Another controller owns this terminal. Release it there, then reconnect here.",
            ),
            Self::HerdrOperation => (
                "herdr_operation",
                "Herdr rejected the operation. Reconnect to refresh the current workspace state.",
            ),
            Self::HerdrWorkspaceGroup => (
                "herdr_workspace_group",
                "This parent workspace has linked worktree workspaces. Close its panes or groups in the ordinary Herdr client after checking the affected workspaces.",
            ),
        }
    }
}

const SSH_STAGE_TIMEOUT: Duration = Duration::from_secs(30);
// The stream handshake includes the host-key callback, so this budget must
// leave room for the complete explicit trust prompt in addition to network
// setup.
const SSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(180);
const HOST_KEY_PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
const AUTO_RECONNECT_MAX_ATTEMPTS: u32 = 6;
const AUTO_RECONNECT_BASE_DELAY: Duration = Duration::from_millis(250);
const AUTO_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(15);
const REPLACEMENT_GRACE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// Run one tmux Control Mode client. The SSH channel deliberately has no PTY:
/// Control Mode is a line protocol and `%output` carries the remote panes'
/// byte stream. This is `-C`, rather than `-CC`; `-CC` additionally disables
/// tmux's client-side echo behavior for an embedded terminal and is not needed
/// when the command is executed over a non-PTY SSH channel.
async fn run_connection(
    shared: Arc<ConnectionShared>,
    mut start: ConnectionStart,
    mut commands: mpsc::Receiver<ControlCommand>,
) {
    let mut retries = 0;
    let result = loop {
        let ready_epoch = shared.ready_epoch();
        let result = run_connection_flow(Arc::clone(&shared), start, &mut commands).await;
        detach_all(&shared);

        // A flow may stay alive for hours after reaching Ready. Reset the
        // outage budget whenever this flow reached a fresh Ready snapshot so
        // six unrelated network drops over the lifetime of the owner cannot
        // permanently disable native reconnect.
        if shared.ready_epoch() != ready_epoch {
            retries = 0;
        }

        let Err(failure) = result else {
            shared.clear_owned_zoom();
            break Ok(());
        };
        if !automatic_retry_allowed(&shared, failure) || retries >= AUTO_RECONNECT_MAX_ATTEMPTS {
            shared.clear_owned_zoom();
            break Err(failure);
        }

        let delay = reconnect_delay(retries);
        shared.mark_reconnecting();
        if !wait_for_reconnect(&shared, delay).await {
            shared.clear_owned_zoom();
            break Err(failure);
        }
        let Some(profile) = retained_profile(&shared) else {
            shared.clear_owned_zoom();
            break Err(failure);
        };
        retries = retries.saturating_add(1);
        start = ConnectionStart::Profile(profile);
    };
    shared.clear_commands();
    detach_all(&shared);
    shared.clear_owned_zoom();
    shared.finish(result);
}

fn retained_profile(shared: &ConnectionShared) -> Option<ConnectionProfile> {
    shared
        .session
        .lock()
        .ok()
        .and_then(|state| state.profile.clone())
}

fn automatic_retry_allowed(shared: &ConnectionShared, failure: FlowFailure) -> bool {
    if shared.is_cancelled() || !shared.automatic_reconnect_enabled() || !shared.has_been_ready() {
        return false;
    }
    // A host-key or authentication failure is terminal even when russh
    // reports it through the generic network path. The explicit Failed state
    // is set by the host-key handler and is checked before retrying.
    let failed_state = shared
        .info
        .lock()
        .map(|info| info.state == ConnectionState::Failed)
        .unwrap_or(true);
    if failed_state {
        return false;
    }
    matches!(
        failure,
        FlowFailure::Network
            | FlowFailure::Channel
            | FlowFailure::Transport
            | FlowFailure::RemoteClosed
    )
}

fn reconnect_delay(retry: u32) -> Duration {
    let multiplier = 1_u32.checked_shl(retry.min(6)).unwrap_or(u32::MAX);
    AUTO_RECONNECT_BASE_DELAY
        .checked_mul(multiplier)
        .unwrap_or(AUTO_RECONNECT_MAX_DELAY)
        .min(AUTO_RECONNECT_MAX_DELAY)
}

async fn wait_for_reconnect(shared: &ConnectionShared, delay: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        if shared.is_cancelled() || !shared.automatic_reconnect_enabled() {
            return false;
        }
        if !shared.is_foreground() {
            let notified = shared.retry_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shared.is_cancelled() || !shared.automatic_reconnect_enabled() {
                return false;
            }
            if shared.is_foreground() {
                continue;
            }
            tokio::select! {
                _ = shared.cancelled() => return false,
                _ = notified => {}
            }
            continue;
        }
        if tokio::time::Instant::now() >= deadline {
            return true;
        }
        let notified = shared.retry_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        tokio::select! {
            _ = shared.cancelled() => return false,
            _ = &mut notified => {},
            _ = tokio::time::sleep_until(deadline) => return true,
        }
    }
}

async fn run_connection_flow(
    shared: Arc<ConnectionShared>,
    start: ConnectionStart,
    commands: &mut mpsc::Receiver<ControlCommand>,
) -> Result<(), FlowFailure> {
    let profile = match start {
        ConnectionStart::Profile(profile) => profile,
        ConnectionStart::Options(ConnectOptions {
            host,
            port,
            username,
            credentials,
            known_hosts_path,
            backend,
            runtime,
        }) => {
            let credentials = match credentials {
                AuthOptions::PublicKey {
                    mut private_key,
                    mut passphrase,
                } => {
                    let decoded_key = keys::decode_secret_key(
                        &private_key,
                        passphrase.as_deref().map(String::as_str),
                    );
                    // Clear the caller-provided PEM and passphrase before the
                    // first await. The reconnect profile retains only the
                    // parsed key.
                    private_key.zeroize();
                    if let Some(passphrase) = passphrase.as_mut() {
                        passphrase.zeroize();
                    }
                    let key = decoded_key.map_err(|_| FlowFailure::KeyFile)?;
                    StoredCredentials::PublicKey { key: Arc::new(key) }
                }
                AuthOptions::Password { password } => StoredCredentials::Password {
                    // Move the already zeroizing input into the reconnect
                    // profile. Arc cloning below shares this buffer without
                    // making another password copy in native state.
                    password: Arc::new(password),
                },
            };
            let profile = ConnectionProfile {
                host,
                port,
                username,
                known_hosts_path,
                credentials,
                backend,
                runtime,
            };
            shared.set_profile(profile.clone());
            profile
        }
    };
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
        tokio::net::TcpStream::connect((profile.host.as_str(), profile.port)),
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

    let result = run_tmux_authenticated_session(&shared, &profile, &mut session, commands).await;
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

async fn run_tmux_authenticated_session(
    shared: &Arc<ConnectionShared>,
    profile: &ConnectionProfile,
    session: &mut client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlCommand>,
) -> Result<(), FlowFailure> {
    shared.set_state(ConnectionState::Authenticating);
    let authentication = match &profile.credentials {
        StoredCredentials::PublicKey { key } => {
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
            await_stage(
                shared,
                session.authenticate_publickey(
                    profile.username.to_owned(),
                    PrivateKeyWithHashAlg::new(Arc::clone(key), hash_alg),
                ),
                SSH_STAGE_TIMEOUT,
                FlowFailure::Authentication,
            )
            .await?
        }
        StoredCredentials::Password { password } => {
            // russh owns a transient String while it processes the SSH
            // USERAUTH request; the retained reconnect copy remains wrapped
            // in Zeroizing and is never logged or persisted. Passing the
            // exact &str preserves whitespace passwords byte-for-byte.
            await_stage(
                shared,
                session.authenticate_password(profile.username.to_owned(), password.as_str()),
                SSH_STAGE_TIMEOUT,
                FlowFailure::Authentication,
            )
            .await?
        }
    };
    if !matches!(authentication, client::AuthResult::Success) {
        return Err(FlowFailure::Authentication);
    }
    if shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }

    match profile.backend {
        Backend::Tmux => control::run(shared, session, commands).await,
        Backend::Herdr => herdr_control::run(shared, profile, session, commands).await,
    }
}

fn detach_all(shared: &ConnectionShared) {
    if let Ok(state) = shared.session.lock() {
        for id in state.pane_terminals.values() {
            registry::detach_transport(*id, shared.generation);
        }
    }
    registry::detach_transport(shared.terminal_id, shared.generation);
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
    fn backend_runtime_validation_and_endpoint_scope() {
        let options = |backend, runtime: Option<&str>| ConnectOptions {
            host: "EXAMPLE.test".into(),
            port: 22,
            username: "fixture".into(),
            credentials: AuthOptions::password("fixture-only".into()),
            known_hosts_path: PathBuf::from("/tmp/fixture-known-hosts"),
            backend,
            runtime: runtime.map(str::to_owned),
        };
        let tmux = options(Backend::Tmux, None).validate().unwrap();
        let endpoint = SessionEndpoint::from_options(&tmux);
        assert!(endpoint.matches(&tmux));
        let herdr = options(Backend::Herdr, None).validate().unwrap();
        assert!(!endpoint.matches(&herdr));
        let herdr_endpoint = SessionEndpoint::from_options(&herdr);
        assert!(
            herdr_endpoint.matches(&options(Backend::Herdr, Some("default")).validate().unwrap())
        );
        assert!(!herdr_endpoint.matches(&options(Backend::Herdr, Some("dev")).validate().unwrap()));
        assert!(options(Backend::Tmux, Some("dev")).validate().is_err());
        for name in ["../other", "..", ".", "a/b", "a b", "x;exit", "$(id)"] {
            assert!(options(Backend::Herdr, Some(name)).validate().is_err());
        }
        assert!(
            options(Backend::Herdr, Some(&"x".repeat(65)))
                .validate()
                .is_err()
        );
        assert!(
            options(Backend::Herdr, Some("dev-session_1.0"))
                .validate()
                .is_ok()
        );
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
    fn reconnect_wait_is_foreground_aware_and_disableable() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let shared = Arc::new(ConnectionShared::new(
                9003,
                1,
                "example.test".to_owned(),
                22,
                PathBuf::from("/tmp/example-known-hosts"),
            ));
            shared.set_state(ConnectionState::Ready);
            assert!(automatic_retry_allowed(&shared, FlowFailure::Transport));
            assert_eq!(shared.ready_epoch(), 1);
            shared.set_state(ConnectionState::Ready);
            assert_eq!(shared.ready_epoch(), 2);

            shared.set_foreground(false);
            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_millis(1)).await }
            });
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(!waiting.is_finished(), "background reconnect must wait");
            shared.set_foreground(true);
            assert!(
                tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .expect("foreground should wake retry")
                    .expect("retry task should join")
            );

            shared.set_foreground(false);
            shared.set_automatic_reconnect(false);
            assert!(!wait_for_reconnect(&shared, Duration::from_millis(1)).await);
        });
    }

    #[test]
    fn failed_explicit_connect_clears_credentials_and_next_endpoint_discards_topology() {
        let owner = registry::create_terminal(80, 24).expect("owner terminal");
        let stale_pane = registry::create_terminal(80, 24).expect("stale pane terminal");
        let known_hosts_path = PathBuf::from("/tmp/meeterm-endpoint-regression-known-hosts");
        let old_profile = ConnectionProfile {
            host: "old.example.test".to_owned(),
            port: 22,
            username: "meeterm".to_owned(),
            known_hosts_path: known_hosts_path.clone(),
            backend: Backend::Tmux,
            runtime: None,
            credentials: StoredCredentials::Password {
                password: Arc::new(Zeroizing::new("old secret".to_owned())),
            },
        };

        {
            let state = session_state(owner);
            let mut state = state.lock().expect("session state");
            // Simulate a previously connected password session. Endpoint
            // identity is retained independently from the credential profile.
            state.endpoint = Some(SessionEndpoint {
                host: "old.example.test".to_owned(),
                port: 22,
                username: "meeterm".to_owned(),
                known_hosts_path: known_hosts_path.clone(),
                backend: Backend::Tmux,
                runtime: None,
            });
            state.profile = Some(old_profile);
            state.pane_terminals.insert(42, stale_pane);
            state.snapshot.selected_pane = Some(42);
        }

        let malformed_key = |host: &str| ConnectOptions {
            host: host.to_owned(),
            port: 22,
            username: "meeterm".to_owned(),
            credentials: AuthOptions::public_key("definitely-not-a-private-key".to_owned(), None),
            known_hosts_path: known_hosts_path.clone(),
            backend: Backend::Tmux,
            runtime: None,
        };

        // The key is non-empty and passes synchronous argument validation, so
        // the connection task reaches key parsing after the old profile has
        // already been invalidated.
        connect_terminal(owner, malformed_key("old.example.test"))
            .expect("start malformed same-endpoint connection");
        assert_eq!(
            reconnect_terminal(owner),
            Err(ConnectionError::ReconnectUnavailable)
        );
        {
            let state = session_state(owner);
            assert!(state.lock().expect("session state").profile.is_none());
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = connection_snapshot(owner).expect("connection snapshot");
            if snapshot.state == ConnectionState::Failed as u32 {
                break;
            }
            assert!(Instant::now() < deadline, "malformed key did not fail");
            std::thread::sleep(Duration::from_millis(2));
        }

        // A new endpoint must compare against the retained endpoint identity,
        // even though the failed credential profile has been removed. This
        // drains the old pane mapping before the new connection starts.
        connect_terminal(owner, malformed_key("new.example.test"))
            .expect("start malformed new-endpoint connection");
        assert_eq!(
            session_snapshot(owner).expect("new endpoint snapshot"),
            SessionSnapshot::default()
        );
        {
            let state = session_state(owner);
            let state = state.lock().expect("session state");
            assert!(state.profile.is_none());
            assert!(state.pane_terminals.is_empty());
            assert_eq!(
                state
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.host.as_str()),
                Some("new.example.test")
            );
        }

        registry::destroy_terminal(owner);
    }

    #[test]
    fn disconnect_and_flow_completion_never_leave_a_finished_actor_closing() {
        fn connection() -> Arc<ConnectionShared> {
            Arc::new(ConnectionShared::new(
                9013,
                next_generation(),
                "example.test".into(),
                22,
                PathBuf::from("/tmp/unused-known-hosts"),
            ))
        }
        for cancel_first in [false, true] {
            let shared = connection();
            if cancel_first {
                shared.cancel();
            }
            shared.finish(Err(FlowFailure::Transport));
            if !cancel_first {
                assert_eq!(
                    shared.snapshot().unwrap().state,
                    ConnectionState::Failed as u32
                );
                shared.mark_closing();
                shared.cancel();
            }
            assert_eq!(
                shared.snapshot().unwrap().state,
                ConnectionState::Disconnected as u32
            );
        }
        for _ in 0..32 {
            let shared = connection();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let worker_shared = Arc::clone(&shared);
            let worker_barrier = Arc::clone(&barrier);
            let completion = std::thread::spawn(move || {
                worker_barrier.wait();
                worker_shared.finish(Err(FlowFailure::Transport));
            });
            barrier.wait();
            shared.mark_closing();
            shared.cancel();
            completion.join().unwrap();
            assert_eq!(
                shared.snapshot().unwrap().state,
                ConnectionState::Disconnected as u32
            );
            shared.set_state(ConnectionState::Ready);
            assert_eq!(
                shared.snapshot().unwrap().state,
                ConnectionState::Disconnected as u32
            );
        }
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
