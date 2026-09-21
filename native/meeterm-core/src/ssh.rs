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
use crate::workspace::{
    self, Backend, RecoveryPhase, RecoverySnapshot, RuntimeCandidate, RuntimeControlSnapshot,
    RuntimeDiscoverySnapshot, RuntimeSection, RuntimeSectionState, RuntimeSnapshot, RuntimeState,
};

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
    /// SSH is authenticated and the two backend runtime lists are being
    /// collected. Values are appended to preserve the existing ABI.
    DiscoveringRuntimes = 11,
    AwaitingRuntimeSelection = 12,
    AttachingRuntime = 13,
    CreatingRuntime = 14,
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
        let runtime = self.runtime.filter(|value| !value.is_empty());
        if let Some(name) = runtime.as_deref() {
            if name.len() > tmux::MAX_SESSION_NAME_BYTES
                || invalid_runtime_name(name)
                || (self.backend == Backend::Herdr
                    && (name.len() > 64
                        || name == "."
                        || name == ".."
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))))
            {
                return Err(ConnectionError::InvalidArgument);
            }
            if self.backend == Backend::Herdr && name == "default" {
                return Ok(Self {
                    host,
                    runtime: None,
                    ..self
                });
            }
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
    RuntimeSelectionUnavailable,
    RuntimeStale,
    RuntimeCreateCollision,
    RuntimeCreateUnknown,
    TopologyUnsafe,
    RecoveryUnavailable,
    RecoveryStale,
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
            Self::RuntimeSelectionUnavailable => -8,
            Self::RuntimeStale => -9,
            Self::RuntimeCreateCollision => -10,
            Self::RuntimeCreateUnknown => -11,
            Self::TopologyUnsafe => -12,
            Self::RecoveryUnavailable => -13,
            Self::RecoveryStale => -14,
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
            Self::RuntimeSelectionUnavailable => "runtime_selection_unavailable",
            Self::RuntimeStale => "runtime_stale",
            Self::RuntimeCreateCollision => "runtime_create_collision",
            Self::RuntimeCreateUnknown => "runtime_create_unknown",
            Self::TopologyUnsafe => "tmux_topology_unsafe",
            Self::RecoveryUnavailable => "recovery_unavailable",
            Self::RecoveryStale => "recovery_stale",
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
            Self::RuntimeSelectionUnavailable => "the runtime candidate is no longer available",
            Self::RuntimeStale => "the selected remote runtime changed or disappeared",
            Self::RuntimeCreateCollision => "the requested runtime name is already in use",
            Self::RuntimeCreateUnknown => "runtime creation outcome could not be verified",
            Self::TopologyUnsafe => "the tmux topology may be shared with another session",
            Self::RecoveryUnavailable => "recovery is not available for this connection",
            Self::RecoveryStale => "the recovery request belongs to an older operation epoch",
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
    tmux_identity: Option<tmux::SessionIdentity>,
    /// A resolved absolute Herdr executable. This capability is deliberately
    /// kept out of all public snapshots and is revalidated on reconnect.
    herdr_executable: Option<String>,
}

#[derive(Clone)]
enum RuntimeBinding {
    Tmux(tmux::SessionIdentity),
    Herdr {
        name: String,
        default: bool,
        executable: String,
    },
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

    fn from_profile(profile: &ConnectionProfile) -> Self {
        Self {
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            known_hosts_path: profile.known_hosts_path.clone(),
            backend: profile.backend,
            runtime: profile.runtime.clone(),
        }
    }

    #[cfg(test)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZoomCleanupOutcome {
    NotNeeded,
    RestoredConfirmed,
    UnconfirmedOrFailed,
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
    /// Ownership belongs to a tmux window, not to one transient pane target.
    /// The pane ID is retained as the preferred cleanup target while the
    /// window identity keeps ownership through pane switches/removal.
    meeterm_zoomed_window: Option<u64>,
    meeterm_zoomed_pane: Option<u64>,
    foreground: bool,
    automatic_reconnect: bool,
    terminal_visible: bool,
    runtime_discovery: RuntimeDiscoverySnapshot,
    runtime_candidates: HashMap<String, RuntimeBinding>,
    /// The Herdr executable resolved for this authenticated endpoint. Keep it
    /// even while the picker is showing a tmux selection so a refresh cannot
    /// silently move the same SSH lifecycle to another installation.
    herdr_executable: Option<String>,
    /// Monotonic native operation boundary.  It is serialized as a decimal
    /// string in `RuntimeSnapshot::control` so JavaScript cannot round it.
    operation_epoch: u64,
    /// Recovery state and gates are kept under the same lock as the topology
    /// to make one workspace JSON response coherent.
    recovery: RecoverySnapshot,
    /// A confirmation is consumed synchronously by the public API and handed
    /// to the actor through this private slot.  Keeping the public token empty
    /// closes the double-submit race before the command is dequeued.
    pending_confirmation_token: Option<String>,
    /// The last selected Herdr stable terminal identity.  Herdr pane aliases
    /// are mutable and therefore never serve as the recovery identity.
    recovery_terminal_id: Option<String>,
    /// A selected Herdr group can legitimately have no panes. Keep that
    /// distinction separate from a previously selected terminal that has
    /// disappeared, so recovery never falls back to an unrelated pane.
    recovery_group_id: Option<u64>,
    runtime_operations_ready: bool,
    terminal_input_ready: bool,
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
            meeterm_zoomed_window: None,
            meeterm_zoomed_pane: None,
            foreground: true,
            automatic_reconnect: true,
            terminal_visible: true,
            runtime_discovery: RuntimeDiscoverySnapshot::default(),
            runtime_candidates: HashMap::new(),
            herdr_executable: None,
            operation_epoch: 0,
            recovery: RecoverySnapshot::default(),
            pending_confirmation_token: None,
            recovery_terminal_id: None,
            recovery_group_id: None,
            runtime_operations_ready: false,
            terminal_input_ready: false,
        }
    }
}

impl SessionState {
    fn has_retained_work(&self) -> bool {
        !self.snapshot.windows.is_empty()
            || !self.snapshot.panes.is_empty()
            || !self.herdr.snapshot.workspaces.is_empty()
            || !self.herdr.snapshot.groups.is_empty()
            || !self.herdr.snapshot.terminals.is_empty()
            || !self.pane_terminals.is_empty()
    }

    fn control_snapshot(&self) -> RuntimeControlSnapshot {
        RuntimeControlSnapshot {
            operation_epoch: self.operation_epoch.to_string(),
            has_retained_work: self.has_retained_work(),
            runtime_operations_ready: self.runtime_operations_ready,
            terminal_input_ready: self.terminal_input_ready,
            recovery: self.recovery.clone(),
        }
    }
}

fn control_snapshot(state: &SessionState) -> RuntimeControlSnapshot {
    state.control_snapshot()
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
    commands: Mutex<Option<mpsc::Sender<ControlRequest>>>,
    cancelled: AtomicBool,
    /// Set before an explicit disconnect/runtime handoff changes recovery
    /// state. The controller uses this intent to keep its bounded zoom/hook
    /// cleanup ahead of the transport-loss exit. It is deliberately separate
    /// from `cancelled`: explicit shutdown first wakes the established actor,
    /// then hard cancellation is used only after cleanup/session teardown or
    /// the bounded fallback.
    explicit_cleanup_requested: AtomicBool,
    explicit_cleanup_notify: Arc<Notify>,
    cancel_notify: Arc<Notify>,
    finished_notify: Arc<Notify>,
    retry_notify: Arc<Notify>,
    /// Serializes foreground transitions with backend wake handling. The
    /// guard is held only across synchronous session/gate updates; no network
    /// await occurs while it is held.
    foreground_transition: Mutex<()>,
    foreground: AtomicBool,
    automatic_reconnect: AtomicBool,
    ready_once: AtomicBool,
    ready_epoch: AtomicU64,
    /// Serializes the one replacement actor that can be started by a stopped
    /// recovery. Active recovery retries are already serialized by the actor's
    /// command receiver; this guard closes the small race between two callers
    /// pressing Retry after that actor has finished.
    recovery_starting: AtomicBool,
    /// Explicit zoom/hook cleanup is reported through the existing connection
    /// error boundary, so the fixed C snapshot ABI remains unchanged.
    zoom_cleanup_outcome: Mutex<ZoomCleanupOutcome>,
    /// Actor-private intent is published before the Control Mode writer await
    /// and lets a forced shutdown report an unconfirmed mutation in flight.
    zoom_cleanup_pending: AtomicBool,
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
            explicit_cleanup_requested: AtomicBool::new(false),
            explicit_cleanup_notify: Arc::new(Notify::new()),
            cancel_notify: Arc::new(Notify::new()),
            finished_notify: Arc::new(Notify::new()),
            retry_notify: Arc::new(Notify::new()),
            foreground_transition: Mutex::new(()),
            foreground: AtomicBool::new(foreground),
            automatic_reconnect: AtomicBool::new(automatic_reconnect),
            ready_once: AtomicBool::new(false),
            ready_epoch: AtomicU64::new(0),
            recovery_starting: AtomicBool::new(false),
            zoom_cleanup_outcome: Mutex::new(ZoomCleanupOutcome::NotNeeded),
            zoom_cleanup_pending: AtomicBool::new(false),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn explicit_cleanup_requested(&self) -> bool {
        self.explicit_cleanup_requested.load(Ordering::Acquire)
    }

    async fn explicit_cleanup(&self) {
        loop {
            let notified = self.explicit_cleanup_notify.notified();
            tokio::pin!(notified);
            // `notify_waiters` does not retain a permit. Register before the
            // flag check so an explicit request cannot be lost between the
            // two operations.
            notified.as_mut().enable();
            if self.explicit_cleanup_requested() {
                return;
            }
            notified.await;
        }
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
        self.explicit_cleanup_notify.notify_waiters();
        self.retry_notify.notify_waiters();
    }

    fn set_profile(&self, profile: ConnectionProfile) {
        if let Ok(mut session) = self.session.lock()
            && session.generation == self.generation
            && !self.is_cancelled()
        {
            // The endpoint is deliberately credential-free, but it must still
            // follow the selected profile.  Picker selection can switch from
            // tmux to Herdr after the host-stage endpoint was installed; all
            // public workspace/selection paths consult this field to choose
            // the backend-specific native snapshot and behavior.
            session.endpoint = Some(SessionEndpoint::from_profile(&profile));
            if let Some(executable) = profile.herdr_executable.clone() {
                session.herdr_executable = Some(executable);
            }
            session.profile = Some(profile);
        }
    }

    fn set_commands(&self, sender: mpsc::Sender<ControlRequest>) {
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
            state.meeterm_zoomed_window = None;
            state.meeterm_zoomed_pane = None;
        }
        self.zoom_cleanup_pending.store(false, Ordering::Release);
    }

    fn mark_zoom_cleanup_pending(&self) {
        self.zoom_cleanup_pending.store(true, Ordering::Release);
    }

    fn clear_zoom_cleanup_pending(&self) {
        self.zoom_cleanup_pending.store(false, Ordering::Release);
    }

    fn has_zoom_cleanup_intent(&self) -> bool {
        self.zoom_cleanup_pending.load(Ordering::Acquire)
            || self
                .session
                .lock()
                .map(|state| state.generation == self.generation && state.meeterm_zoomed)
                .unwrap_or(true)
    }

    fn record_zoom_cleanup(&self, outcome: ZoomCleanupOutcome) {
        if let Ok(mut current) = self.zoom_cleanup_outcome.lock() {
            *current = match (*current, outcome) {
                (ZoomCleanupOutcome::UnconfirmedOrFailed, _)
                | (_, ZoomCleanupOutcome::UnconfirmedOrFailed) => {
                    ZoomCleanupOutcome::UnconfirmedOrFailed
                }
                (ZoomCleanupOutcome::RestoredConfirmed, _)
                | (_, ZoomCleanupOutcome::RestoredConfirmed) => {
                    ZoomCleanupOutcome::RestoredConfirmed
                }
                _ => ZoomCleanupOutcome::NotNeeded,
            };
        }
    }

    fn zoom_cleanup_outcome(&self) -> ZoomCleanupOutcome {
        self.zoom_cleanup_outcome
            .lock()
            .map(|outcome| *outcome)
            .unwrap_or(ZoomCleanupOutcome::UnconfirmedOrFailed)
    }

    fn publish_layout_restore_warning(&self) {
        if let Ok(mut info) = self.info.lock() {
            info.error_code = "layout_restore_unconfirmed".to_owned();
            info.error_message = recovery_reason_message("layout_restore_unconfirmed");
        }
    }

    fn command_sender(&self) -> Option<mpsc::Sender<ControlRequest>> {
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
        if state == ConnectionState::Ready {
            if self.mark_ready() {
                return;
            }
            // Keep the fixed C-state helper usable by focused tests and by
            // pre-session trust/auth fixtures whose SessionState generation
            // has not been installed yet.  Backend actors use `mark_ready`
            // directly, so this compatibility path cannot bypass recovery
            // validation in production.
            let recovery_phase = self.recovery_phase();
            if matches!(
                recovery_phase,
                RecoveryPhase::AwaitingConfirmation | RecoveryPhase::Stopped
            ) {
                return;
            }
            if let Ok(mut info) = self.info.lock() {
                if self.is_cancelled() || info.finished {
                    return;
                }
                info.state = ConnectionState::Ready;
                self.ready_once.store(true, Ordering::Release);
                self.ready_epoch.fetch_add(1, Ordering::AcqRel);
            }
            return;
        }
        if let Ok(mut info) = self.info.lock() {
            if self.is_cancelled() || info.finished {
                return;
            }
            info.state = state;
        }
    }

    fn operation_epoch(&self) -> u64 {
        self.session
            .lock()
            .map(|state| state.operation_epoch)
            .unwrap_or(0)
    }

    fn recovery_phase(&self) -> RecoveryPhase {
        self.session
            .lock()
            .map(|state| state.recovery.phase)
            .unwrap_or(RecoveryPhase::Stopped)
    }

    /// A backend may start with an already-committed recovery phase while it
    /// reconstructs the retained runtime. That initial phase is allowed to
    /// complete, but any later committed phase must interrupt the live
    /// controller loop even when foreground has already returned to true.
    /// Comparing the operation epoch also catches a foreground loss that
    /// happens during the initial reconstruction before its first wake is
    /// consumed.
    fn recovery_requires_controller_exit(&self, initial_epoch: Option<u64>) -> bool {
        self.session
            .lock()
            .map(|state| {
                state.generation != self.generation
                    || (state.recovery.phase != RecoveryPhase::None
                        && initial_epoch.is_none_or(|epoch| state.operation_epoch != epoch))
            })
            .unwrap_or(true)
    }

    /// Invalidate all remote permissions at a recoverable loss boundary while
    /// leaving the last coherent topology and native terminal objects intact.
    /// The caller must detach transports immediately after this method.
    fn begin_recovery(&self, reason: &'static str, attempt: u32) -> Option<u64> {
        let Ok(mut info) = self.info.lock() else {
            return None;
        };
        if self.is_cancelled() || info.finished {
            return None;
        }
        if info.state == ConnectionState::Failed && is_terminal_security_error(&info.error_code) {
            // A host-key/auth failure is a user-action boundary. Do not let
            // a concurrent lifecycle notification clear its diagnostics and
            // turn it back into an automatic reconnect.
            return None;
        }
        let Ok(mut state) = self.session.lock() else {
            return None;
        };
        if state.generation != self.generation {
            return None;
        }
        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        state.recovery = RecoverySnapshot {
            phase: RecoveryPhase::Reconnecting,
            reason: sanitize_recovery_reason(reason),
            attempt: attempt.min(workspace::DEFAULT_RECOVERY_MAX_ATTEMPTS),
            max_attempts: workspace::DEFAULT_RECOVERY_MAX_ATTEMPTS,
            confirmation_token: String::new(),
        };
        state.pending_confirmation_token = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        if state
            .endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.backend == Backend::Herdr)
        {
            let selected_group = state.herdr.active_group().or_else(|| {
                state
                    .selected_pane
                    .and_then(|id| state.herdr.panes.get(&id).map(|pane| pane.group))
            });
            let selected_terminal = state.selected_pane.and_then(|id| {
                state
                    .herdr
                    .panes
                    .get(&id)
                    .map(|pane| pane.terminal_id.clone())
            });
            state.recovery_terminal_id = selected_terminal;
            state.recovery_group_id = selected_group
                .filter(|group| !state.herdr.panes.values().any(|pane| pane.group == *group));
        } else {
            state.recovery_terminal_id = None;
            state.recovery_group_id = None;
        }
        info.state = ConnectionState::Reconnecting;
        info.error_code.clear();
        info.error_message.clear();
        info.pending = None;
        Some(state.operation_epoch)
    }

    /// Re-open a stopped recovery after its actor has finished.  This is a
    /// deliberate handoff boundary rather than a normal loss transition:
    /// `begin_recovery` rejects finished actors so late transport callbacks
    /// cannot mutate their state.  An explicit Retry, however, is allowed to
    /// create one replacement generation, provided the caller still owns the
    /// displayed epoch and the old actor is genuinely finished.
    fn begin_stopped_recovery(&self, expected_epoch: u64) -> Result<u64, ConnectionError> {
        let mut info = self.info.lock().map_err(|_| ConnectionError::Internal)?;
        if self.is_cancelled() || !info.finished {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        let mut state = self.session.lock().map_err(|_| ConnectionError::Internal)?;
        if state.generation != self.generation || state.recovery.phase != RecoveryPhase::Stopped {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        if state.operation_epoch != expected_epoch {
            return Err(ConnectionError::RecoveryStale);
        }

        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        state.recovery = RecoverySnapshot {
            phase: RecoveryPhase::Reconnecting,
            reason: "manual_retry".to_owned(),
            attempt: 0,
            max_attempts: workspace::DEFAULT_RECOVERY_MAX_ATTEMPTS,
            confirmation_token: String::new(),
        };
        state.pending_confirmation_token = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        if state
            .endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.backend == Backend::Herdr)
        {
            let selected_group = state.herdr.active_group().or_else(|| {
                state
                    .selected_pane
                    .and_then(|id| state.herdr.panes.get(&id).map(|pane| pane.group))
            });
            let selected_terminal = state.selected_pane.and_then(|id| {
                state
                    .herdr
                    .panes
                    .get(&id)
                    .map(|pane| pane.terminal_id.clone())
            });
            state.recovery_terminal_id = selected_terminal;
            state.recovery_group_id = selected_group
                .filter(|group| !state.herdr.panes.values().any(|pane| pane.group == *group));
        } else {
            state.recovery_terminal_id = None;
            state.recovery_group_id = None;
        }
        // The old ConnectionInfo is not published after the replacement is
        // installed, but keeping this snapshot coherent closes the interval
        // in which a concurrent native poll could still observe the old map
        // entry during the handoff.
        info.state = ConnectionState::Reconnecting;
        info.error_code.clear();
        info.error_message.clear();
        info.pending = None;
        Ok(state.operation_epoch)
    }

    fn begin_binding_transition(&self) -> bool {
        let Ok(mut info) = self.info.lock() else {
            return false;
        };
        if self.is_cancelled() || info.finished {
            return false;
        }
        let Ok(mut state) = self.session.lock() else {
            return false;
        };
        if state.generation != self.generation {
            return false;
        }
        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        state.recovery = RecoverySnapshot::default();
        state.pending_confirmation_token = None;
        state.recovery_terminal_id = None;
        state.recovery_group_id = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        info.state = ConnectionState::AttachingRuntime;
        true
    }

    fn publish_recovery_confirmation(&self, token: String) -> Result<u64, FlowFailure> {
        if token.is_empty() || token.len() > RECOVERY_TOKEN_MAX_BYTES {
            return Err(FlowFailure::HerdrProtocol);
        }
        let Ok(mut info) = self.info.lock() else {
            return Err(FlowFailure::Stale);
        };
        if self.is_cancelled() || info.finished {
            return Err(FlowFailure::Stale);
        }
        let Ok(mut state) = self.session.lock() else {
            return Err(FlowFailure::Stale);
        };
        if state.generation != self.generation
            || state.recovery.phase != RecoveryPhase::Reconnecting
        {
            return Err(FlowFailure::Stale);
        }
        state.recovery.phase = RecoveryPhase::AwaitingConfirmation;
        state.recovery.confirmation_token = token;
        state.pending_confirmation_token = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        info.state = ConnectionState::Reconnecting;
        Ok(state.operation_epoch)
    }

    fn take_pending_confirmation(&self, token: &str) -> bool {
        self.session
            .lock()
            .map(|mut state| {
                state
                    .pending_confirmation_token
                    .take()
                    .is_some_and(|pending| pending == token)
            })
            .unwrap_or(false)
    }

    /// Publish a fully verified backend/frame commit for the operation that
    /// started the synchronization. The backend-specific topology/projection
    /// mutation runs while the same session lock is held as the Ready gate, so
    /// a late capture/frame cannot complete a newer recovery attempt between
    /// those two publications.
    fn commit_ready_at_epoch(
        &self,
        expected_epoch: u64,
        commit: impl FnOnce(&mut SessionState),
    ) -> bool {
        self.commit_ready_at_epoch_result(expected_epoch, |state| {
            commit(state);
            Ok(())
        })
        .is_ok()
    }

    /// Result-bearing variant used by backend transactions that must perform
    /// a native commit while the session epoch lock is held.  In particular,
    /// strict tmux capture application must not happen before this boundary:
    /// if the expected epoch is stale, the callback is never run and the last
    /// public Term remains untouched.
    fn commit_ready_at_epoch_result(
        &self,
        expected_epoch: u64,
        commit: impl FnOnce(&mut SessionState) -> Result<(), FlowFailure>,
    ) -> Result<(), FlowFailure> {
        let Ok(mut info) = self.info.lock() else {
            return Err(FlowFailure::Stale);
        };
        if self.is_cancelled() || info.finished {
            return Err(FlowFailure::Stale);
        }
        let Ok(mut state) = self.session.lock() else {
            return Err(FlowFailure::Stale);
        };
        if state.generation != self.generation {
            return Err(FlowFailure::Stale);
        }
        if state.operation_epoch != expected_epoch {
            return Err(FlowFailure::Stale);
        }
        if matches!(
            state.recovery.phase,
            RecoveryPhase::AwaitingConfirmation | RecoveryPhase::Stopped
        ) {
            return Err(FlowFailure::Stale);
        }
        commit(&mut state)?;
        state.recovery = RecoverySnapshot::default();
        state.pending_confirmation_token = None;
        state.recovery_terminal_id = None;
        state.recovery_group_id = None;
        state.runtime_operations_ready = true;
        state.terminal_input_ready = state.terminal_visible
            && state.foreground
            && state.selected_pane.is_some_and(|selected| {
                state.pane_terminals.contains_key(&selected)
                    && state
                        .snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == selected)
                    && state.pane_terminals.get(&selected).is_some_and(|native| {
                        registry::transport_ready_or_local(*native, state.generation)
                    })
            });
        info.state = ConnectionState::Ready;
        info.error_code.clear();
        info.error_message.clear();
        info.pending = None;
        self.ready_once.store(true, Ordering::Release);
        self.ready_epoch.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Mark a fully verified backend/frame commit for the operation that
    /// started the synchronization. A late capture/frame cannot complete a
    /// newer recovery attempt because the epoch is checked under the session
    /// lock before any Ready state is published.
    fn mark_ready_at_epoch(&self, expected_epoch: u64) -> bool {
        self.commit_ready_at_epoch(expected_epoch, |_| {})
    }

    /// Compatibility wrapper for callers that have no asynchronous boundary.
    /// Backend actors use `mark_ready_at_epoch` with a captured epoch.
    fn mark_ready(&self) -> bool {
        let expected_epoch = self.operation_epoch();
        self.mark_ready_at_epoch(expected_epoch)
    }

    fn stop_recovery(&self, reason: &'static str) {
        let Ok(mut info) = self.info.lock() else {
            return;
        };
        if self.is_cancelled() || info.finished {
            return;
        }
        // Host-key and authentication callbacks can publish a terminal
        // security failure before the transport future returns. Keep that
        // bounded error (and its fingerprints) intact while committing the
        // retained-work recovery stop below.
        let preserve_security_failure =
            info.state == ConnectionState::Failed && is_terminal_security_error(&info.error_code);
        let Ok(mut state) = self.session.lock() else {
            return;
        };
        if state.generation != self.generation {
            return;
        }
        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        state.recovery.phase = RecoveryPhase::Stopped;
        state.recovery.reason = sanitize_recovery_reason(reason);
        state.recovery.confirmation_token.clear();
        state.pending_confirmation_token = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        if !preserve_security_failure {
            info.state = ConnectionState::Failed;
            info.error_code = sanitize_recovery_reason(reason);
            info.error_message = recovery_reason_message(reason);
        }
        info.pending = None;
    }

    /// Explicit disconnect/runtime switch boundary. This revokes operation
    /// gates and wakes the established actor, but intentionally does not set
    /// `cancelled` yet: the controller must retain a usable SSH stream long
    /// enough to acknowledge its bounded cleanup. Callers force cancellation
    /// if the actor misses the shutdown deadline.
    fn invalidate_explicitly(&self, reason: &'static str) {
        let Ok(mut info) = self.info.lock() else {
            return;
        };
        if info.finished {
            return;
        }
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
        {
            // Publish the explicit lifecycle intent before changing the
            // recovery phase. The actor can then choose its cleanup branch
            // while the transport remains usable. Repeated callers (for
            // example Change followed by replacement) keep the first epoch
            // boundary and only re-wake the same actor.
            if !self.explicit_cleanup_requested() {
                self.explicit_cleanup_requested
                    .store(true, Ordering::Release);
                state.operation_epoch = next_operation_epoch(state.operation_epoch);
                state.recovery.phase = RecoveryPhase::Stopped;
                state.recovery.reason = sanitize_recovery_reason(reason);
                state.recovery.confirmation_token.clear();
                state.pending_confirmation_token = None;
                state.runtime_operations_ready = false;
                state.terminal_input_ready = false;
            }
            info.state = ConnectionState::Closing;
            info.pending = None;
            self.explicit_cleanup_notify.notify_waiters();
            return;
        }
        info.pending = None;
    }

    fn current_request_is_ready(&self, epoch: u64) -> bool {
        if self.is_cancelled() {
            return false;
        }
        self.session
            .lock()
            .map(|state| {
                state.generation == self.generation
                    && state.operation_epoch == epoch
                    && state.recovery.phase == RecoveryPhase::None
                    && state.runtime_operations_ready
            })
            .unwrap_or(false)
    }

    fn current_terminal_input_is_ready(&self, epoch: u64) -> bool {
        if self.is_cancelled() {
            return false;
        }
        self.session
            .lock()
            .map(|state| {
                state.generation == self.generation
                    && state.operation_epoch == epoch
                    && state.recovery.phase == RecoveryPhase::None
                    && state.runtime_operations_ready
                    && state.terminal_input_ready
            })
            .unwrap_or(false)
    }

    fn current_request_epoch(&self, epoch: u64) -> bool {
        !self.is_cancelled()
            && !self.explicit_cleanup_requested()
            && self
                .session
                .lock()
                .map(|state| state.generation == self.generation && state.operation_epoch == epoch)
                .unwrap_or(false)
    }

    fn refresh_terminal_input_ready(&self) {
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
            && state.recovery.phase == RecoveryPhase::None
            && state.runtime_operations_ready
        {
            state.terminal_input_ready = state.terminal_visible
                && state.foreground
                && state.selected_pane.is_some_and(|selected| {
                    state.pane_terminals.contains_key(&selected)
                        && state
                            .snapshot
                            .panes
                            .iter()
                            .any(|pane| pane.pane_id == selected)
                        && state.pane_terminals.get(&selected).is_some_and(|native| {
                            registry::transport_ready_or_local(*native, state.generation)
                        })
                });
        }
    }

    /// Serialize the synchronous foreground gate with a backend actor's
    /// same-controller wake. This prevents a wake that observed `true` from
    /// rearming a terminal after a newer background transition has completed.
    fn foreground_transition_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.foreground_transition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A controller may finish attaching after the app has already gone into
    /// the background. Recheck under the same transition boundary used by
    /// `set_foreground` so a newly attached binding cannot become an input
    /// path that the earlier suspend pass could not have seen.
    fn suspend_terminal_if_background(&self, terminal_id: TerminalId) {
        let _transition = self.foreground_transition_lock();
        if !self.is_foreground() {
            registry::suspend_transport(terminal_id, self.generation);
        }
    }

    fn set_foreground(&self, foreground: bool) {
        let _transition = self.foreground_transition_lock();
        self.foreground.store(foreground, Ordering::Release);

        let (generation, backend, terminal_ids) = match self.session.lock() {
            Ok(mut state) if state.generation == self.generation => {
                state.foreground = foreground;
                if !foreground {
                    state.terminal_input_ready = false;
                    // A confirmation token is tied to the visible recovery
                    // attempt. If the app backgrounds while that token is
                    // shown, revoke it synchronously and let the Herdr
                    // coordinator run a fresh bounded discovery on the next
                    // wake. A healthy live controller is not torn down merely
                    // because the app changed foreground state.
                    if state.recovery.phase == RecoveryPhase::AwaitingConfirmation {
                        state.operation_epoch = next_operation_epoch(state.operation_epoch);
                        state.recovery.phase = RecoveryPhase::Reconnecting;
                        state.recovery.confirmation_token.clear();
                        state.pending_confirmation_token = None;
                        state.runtime_operations_ready = false;
                    }
                }

                // Do not call into the registry while holding SessionState.
                // The established lock order is session -> registry ->
                // Terminal, and this list is the only state needed for the
                // synchronous gate pass below.
                let mut terminal_ids = state.pane_terminals.values().copied().collect::<Vec<_>>();
                terminal_ids.push(self.terminal_id);
                terminal_ids.sort_unstable();
                terminal_ids.dedup();
                (
                    state.generation,
                    state.endpoint.as_ref().map(|endpoint| endpoint.backend),
                    terminal_ids,
                )
            }
            _ => {
                self.retry_notify.notify_one();
                return;
            }
        };

        if foreground {
            // A tmux actor has independent pane routes, so all existing
            // bindings can be rearmed synchronously. Herdr's controller is
            // resumed only by its live actor after this method wakes it.
            if backend == Some(Backend::Tmux) {
                for terminal_id in terminal_ids {
                    registry::resume_transport(terminal_id, generation);
                }
                self.refresh_terminal_input_ready();
            }
        } else {
            // The session flags already reject connection-level sends. This
            // terminal pass closes the direct native registry boundary before
            // this synchronous API returns, while retaining every binding and
            // cached Term.
            for terminal_id in terminal_ids {
                registry::suspend_transport(terminal_id, generation);
            }
        }
        // Retain one wake permit when the actor is between awaits; a
        // waiters-only notification could be lost while Herdr is processing a
        // frame/input request and leave its suspended controller asleep.
        self.retry_notify.notify_one();
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

    fn is_awaiting_runtime_selection(&self) -> bool {
        self.info
            .lock()
            .map(|info| {
                !info.finished
                    && !self.is_cancelled()
                    && info.state == ConnectionState::AwaitingRuntimeSelection
            })
            .unwrap_or(false)
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
            if info.finished {
                return;
            }
            // Completion and cancellation commit under the same lock. A late
            // disconnect must not leave a finished actor permanently Closing.
            info.finished = true;
            match (self.zoom_cleanup_outcome(), result) {
                (ZoomCleanupOutcome::UnconfirmedOrFailed, _) => {
                    info.state = ConnectionState::Disconnected;
                    info.error_code = "layout_restore_unconfirmed".to_owned();
                    info.error_message = recovery_reason_message("layout_restore_unconfirmed");
                }
                (_, _) if self.is_cancelled() || self.explicit_cleanup_requested() => {
                    info.state = ConnectionState::Disconnected
                }
                (_, Ok(())) => info.state = ConnectionState::Disconnected,
                (_, Err(failure)) if info.state != ConnectionState::Failed => {
                    let (code, message) = failure.details();
                    info.state = ConnectionState::Failed;
                    info.error_code = code.to_owned();
                    info.error_message = message.to_owned();
                }
                (_, Err(_)) => {}
            }
            if !self.is_cancelled()
                && !self.explicit_cleanup_requested()
                && let Err(failure) = result
                && let Ok(mut state) = self.session.lock()
                && state.generation == self.generation
                && state.recovery.phase != RecoveryPhase::None
                && state.recovery.phase != RecoveryPhase::Stopped
            {
                let reason = recovery_reason_for_failure(failure);
                state.operation_epoch = next_operation_epoch(state.operation_epoch);
                state.recovery.phase = RecoveryPhase::Stopped;
                state.recovery.reason = sanitize_recovery_reason(reason);
                state.recovery.confirmation_token.clear();
                state.pending_confirmation_token = None;
                state.runtime_operations_ready = false;
                state.terminal_input_ready = false;
                if info.state != ConnectionState::Disconnected {
                    info.state = ConnectionState::Failed;
                    info.error_code = sanitize_recovery_reason(reason);
                    info.error_message = recovery_reason_message(reason);
                }
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

/// A command is tagged at acceptance time.  The actor checks the tag again
/// immediately before any remote operation, so a command accepted just
/// before a transport loss can never be replayed in a later operation epoch.
struct ControlRequest {
    epoch: u64,
    command: ControlCommand,
}

enum ControlCommand {
    RefreshRuntimes,
    SelectRuntime { candidate_id: String },
    CreateRuntime { backend: Backend, name: String },
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
    RetryRecovery,
    ConfirmRecovery { token: String },
}

struct ConnectionEntry {
    shared: Arc<ConnectionShared>,
    abort: tokio::task::AbortHandle,
}

/// Per-terminal lifecycle ownership for connect/replace/disconnect. The
/// serial mutex prevents two synchronous starts from preparing the same
/// SessionState concurrently; the short commit mutex closes the remove/check/
/// install race without holding the global connection map while SSH cleanup
/// or network waits are in progress.
struct OwnerTransition {
    serial: Mutex<()>,
    commit: Mutex<()>,
    next_ticket: AtomicU64,
    active_ticket: AtomicU64,
    cancelled_ticket: AtomicU64,
    /// Monotonic cancellation fence for starts that have entered `begin` but
    /// have not published their active ticket yet.  A single active-ticket
    /// slot cannot represent that interval by itself.
    cancel_epoch: AtomicU64,
}

impl OwnerTransition {
    fn new() -> Self {
        Self {
            serial: Mutex::new(()),
            commit: Mutex::new(()),
            next_ticket: AtomicU64::new(1),
            active_ticket: AtomicU64::new(0),
            cancelled_ticket: AtomicU64::new(0),
            cancel_epoch: AtomicU64::new(0),
        }
    }

    fn begin(&self) -> Result<(std::sync::MutexGuard<'_, ()>, u64), ConnectionError> {
        self.begin_impl(None)
    }

    #[cfg(test)]
    fn begin_with_barriers(
        &self,
        entered: &std::sync::Barrier,
        release: &std::sync::Barrier,
    ) -> Result<(std::sync::MutexGuard<'_, ()>, u64), ConnectionError> {
        self.begin_impl(Some((entered, release)))
    }

    fn begin_impl(
        &self,
        barriers: Option<(&std::sync::Barrier, &std::sync::Barrier)>,
    ) -> Result<(std::sync::MutexGuard<'_, ()>, u64), ConnectionError> {
        // Capture the fence before waiting for the per-owner serial lock.  If
        // Disconnect/Change is accepted while another start is using the
        // serial lock, this start still belongs to the older owner attempt
        // and must not publish/install after that cancellation returns.
        let request_epoch = self.cancel_epoch.load(Ordering::Acquire);
        let ticket = self.next_ticket.fetch_add(1, Ordering::AcqRel).max(1);
        let serial = self.serial.lock().map_err(|_| ConnectionError::Internal)?;
        if let Some((entered, release)) = barriers {
            // Test-only pause: the caller can accept Disconnect while this
            // start has captured the old fence but before publication enters
            // the commit mutex.
            entered.wait();
            release.wait();
        }
        // Ticket publication and the cancellation judgement share this short
        // commit boundary.  Disconnect never observes the old active=0 state
        // and then loses the newly published owner.
        let _commit = self.commit.lock().map_err(|_| ConnectionError::Internal)?;
        if self.cancel_epoch.load(Ordering::Acquire) != request_epoch {
            // Keep the ticket visibly rejected for the caller's subsequent
            // install checks.  The next begin gets a fresh ticket/fence and
            // cannot inherit this cancellation.
            self.cancelled_ticket.store(ticket, Ordering::Release);
            self.active_ticket.store(0, Ordering::Release);
        } else {
            self.active_ticket.store(ticket, Ordering::Release);
        }
        Ok((serial, ticket))
    }

    fn cancel_current_locked(&self) {
        self.cancel_epoch.fetch_add(1, Ordering::AcqRel);
        let active = self.active_ticket.load(Ordering::Acquire);
        if active != 0 {
            self.cancelled_ticket.store(active, Ordering::Release);
        }
    }

    fn cancel_current(&self) {
        if let Ok(_commit) = self.commit.lock() {
            self.cancel_current_locked();
        }
    }

    fn install_allowed(&self, ticket: u64) -> bool {
        ticket != 0
            && self.active_ticket.load(Ordering::Acquire) == ticket
            && self.cancelled_ticket.load(Ordering::Acquire) != ticket
    }

    fn finish(&self, ticket: u64) {
        let _ = self
            .active_ticket
            .compare_exchange(ticket, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

static RUNTIME: OnceLock<Result<Runtime, ()>> = OnceLock::new();
static CONNECTIONS: OnceLock<Mutex<HashMap<TerminalId, ConnectionEntry>>> = OnceLock::new();
static OWNER_TRANSITIONS: OnceLock<Mutex<HashMap<TerminalId, Arc<OwnerTransition>>>> =
    OnceLock::new();
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

fn owner_transition(terminal_id: TerminalId) -> Result<Arc<OwnerTransition>, ConnectionError> {
    let mut owners = OWNER_TRANSITIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    Ok(owners
        .entry(terminal_id)
        .or_insert_with(|| Arc::new(OwnerTransition::new()))
        .clone())
}

/// Start or replace the SSH session associated with a terminal ID using the
/// Rust-only direct-options path. Platform FFI/JNI bridges must use
/// [`connect_host`] so a fresh connection always authenticates and discovers
/// runtimes before an explicit bind. This test-only entry point remains for
/// internal Rust lifecycle tests.
#[cfg(test)]
pub(crate) fn connect_terminal(
    terminal_id: TerminalId,
    options: ConnectOptions,
) -> Result<(), ConnectionError> {
    let options = options.validate()?;
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    start_connection(terminal_id, ConnectionStart::Options(options))
}

/// Start the picker connection. This authenticates the SSH host and leaves
/// the actor in `AwaitingRuntimeSelection` after independent tmux/Herdr
/// discovery; it never creates or attaches a runtime on its own.
pub fn connect_host(
    terminal_id: TerminalId,
    mut options: ConnectOptions,
) -> Result<(), ConnectionError> {
    // Host-stage options carry no authoritative backend/runtime. Reuse the
    // existing validation and credential representation while forcing the
    // endpoint metadata to remain unselected.
    options.backend = Backend::Tmux;
    options.runtime = None;
    let options = options.validate()?;
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    start_connection(terminal_id, ConnectionStart::Host(options))
}

/// Request a fresh runtime list. The operation is low-frequency and is
/// serialized with selection/creation by the owning Rust actor.
pub fn list_runtimes(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    if !shared.is_awaiting_runtime_selection() {
        return Err(ConnectionError::RuntimeSelectionUnavailable);
    }
    let epoch = shared.operation_epoch();
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::RuntimeSelectionUnavailable)?;
    sender
        .try_send(ControlRequest {
            epoch,
            command: ControlCommand::RefreshRuntimes,
        })
        .map_err(|_| ConnectionError::RuntimeSelectionUnavailable)
}

/// Select one native-owned candidate from the current discovery revision. The
/// remote session ID/name is resolved only inside the actor at execution time.
pub fn select_runtime(terminal_id: TerminalId, candidate_id: &str) -> Result<(), ConnectionError> {
    if candidate_id.is_empty() || candidate_id.len() > 128 || invalid_runtime_name(candidate_id) {
        return Err(ConnectionError::InvalidArgument);
    }
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    if !shared.is_awaiting_runtime_selection() {
        return Err(ConnectionError::RuntimeSelectionUnavailable);
    }
    let state = session_state(terminal_id);
    let state = state.lock().map_err(|_| ConnectionError::Internal)?;
    let Some(candidate) = state
        .runtime_discovery
        .tmux
        .candidates
        .iter()
        .chain(state.runtime_discovery.herdr.candidates.iter())
        .find(|candidate| candidate.id == candidate_id)
    else {
        return Err(ConnectionError::RuntimeSelectionUnavailable);
    };
    if !candidate.selectable || !state.runtime_candidates.contains_key(candidate_id) {
        return Err(ConnectionError::RuntimeSelectionUnavailable);
    }
    drop(state);
    let epoch = shared.operation_epoch();
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::RuntimeSelectionUnavailable)?;
    sender
        .try_send(ControlRequest {
            epoch,
            command: ControlCommand::SelectRuntime {
                candidate_id: candidate_id.to_owned(),
            },
        })
        .map_err(|_| ConnectionError::RuntimeSelectionUnavailable)
}

/// Explicitly create a tmux session. Herdr creation/start remains outside the
/// first picker milestone because the upstream 0.9.0 detached-start proof is
/// not part of this core operation.
pub fn create_runtime(
    terminal_id: TerminalId,
    backend: Backend,
    name: &str,
) -> Result<(), ConnectionError> {
    if backend != Backend::Tmux {
        return Err(ConnectionError::RuntimeUnavailable);
    }
    tmux::validate_create_name(name).map_err(|_| ConnectionError::InvalidArgument)?;
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    if !shared.is_awaiting_runtime_selection() {
        return Err(ConnectionError::RuntimeSelectionUnavailable);
    }
    // This is deliberately synchronous and local: a new accepted create
    // request must make the prior operation result observable as "clear"
    // before the actor can publish the next result. A discovery Error section
    // is not cleared here.
    clear_tmux_create_error(&shared)?;
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::RuntimeSelectionUnavailable)?;
    let epoch = shared.operation_epoch();
    sender
        .try_send(ControlRequest {
            epoch,
            command: ControlCommand::CreateRuntime {
                backend,
                name: name.to_owned(),
            },
        })
        .map_err(|_| ConnectionError::RuntimeSelectionUnavailable)
}

/// Return the bounded, native-owned runtime picker snapshot.
pub fn runtime_discovery_snapshot(
    terminal_id: TerminalId,
) -> Result<RuntimeDiscoverySnapshot, ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    session_state(terminal_id)
        .lock()
        .map(|state| state.runtime_discovery.clone())
        .map_err(|_| ConnectionError::Internal)
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
    start_connection(terminal_id, ConnectionStart::ManualReconnect(profile))
}

/// Retry the retained recovery intent for the current owner.  An active actor
/// receives a tagged command and joins the existing attempt; only a stopped,
/// still-authenticated actor starts a replacement generation.
pub fn retry_recovery(terminal_id: TerminalId, expected_epoch: u64) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    if shared.is_cancelled() {
        // Explicit disconnect is a one-way lifecycle boundary.  A later
        // foreground notification or stale Retry button must not resurrect
        // the cancelled actor; a fresh connect is required instead.
        return Err(ConnectionError::RecoveryUnavailable);
    }
    let (phase, profile, epoch) = {
        let state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        (
            state.recovery.phase,
            state.profile.clone(),
            state.operation_epoch,
        )
    };
    if epoch != expected_epoch {
        return Err(ConnectionError::RecoveryStale);
    }
    if !shared.has_been_ready() || phase == RecoveryPhase::None {
        return Err(ConnectionError::RecoveryUnavailable);
    }

    if phase != RecoveryPhase::Stopped {
        let sender = shared
            .command_sender()
            .ok_or(ConnectionError::RecoveryUnavailable)?;
        sender
            .try_send(ControlRequest {
                epoch,
                command: ControlCommand::RetryRecovery,
            })
            .map_err(|_| ConnectionError::RecoveryUnavailable)?;
        return Ok(());
    }

    let profile = profile.ok_or(ConnectionError::ReconnectUnavailable)?;
    // The old actor is left in the map until start_connection performs its
    // bounded cancellation/drain.  The retained SessionState and native Term
    // are intentionally not cleared by this automatic-recovery path.
    if shared.recovery_starting.swap(true, Ordering::AcqRel) {
        // A second Retry arriving while the first replacement is draining is
        // already represented by that in-flight operation. Do not replace
        // the replacement actor with another parallel generation.
        return Ok(());
    }
    // The stopped actor has already committed `finished=true`, so the normal
    // loss transition intentionally cannot be reused here.  Commit the
    // explicit handoff while the old generation is still the map owner; this
    // both makes the fresh epoch visible before the replacement starts and
    // prevents a duplicate Retry from observing another Stopped boundary.
    if let Err(error) = shared.begin_stopped_recovery(expected_epoch) {
        shared.recovery_starting.store(false, Ordering::Release);
        return Err(error);
    }
    let result = start_connection(terminal_id, ConnectionStart::AutomaticReconnect(profile));
    if result.is_err() {
        shared.recovery_starting.store(false, Ordering::Release);
    }
    result
}

/// Confirm the currently displayed Herdr recovery candidate.  Token
/// consumption and the operation-epoch bump happen synchronously, before the
/// actor is allowed to perform any controller acquisition.
pub fn confirm_recovery(terminal_id: TerminalId, token: &str) -> Result<(), ConnectionError> {
    if token.is_empty()
        || token.len() > RECOVERY_TOKEN_MAX_BYTES
        || token.chars().any(char::is_control)
    {
        return Err(ConnectionError::InvalidArgument);
    }
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::RecoveryUnavailable)?;
    let epoch = {
        let mut state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if state.recovery.phase != RecoveryPhase::AwaitingConfirmation
            || state.recovery.confirmation_token != token
        {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        let next = next_operation_epoch(state.operation_epoch);
        state.operation_epoch = next;
        state.recovery.phase = RecoveryPhase::Resynchronizing;
        state.recovery.confirmation_token.clear();
        state.pending_confirmation_token = Some(token.to_owned());
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        next
    };
    if sender
        .try_send(ControlRequest {
            epoch,
            command: ControlCommand::ConfirmRecovery {
                token: token.to_owned(),
            },
        })
        .is_err()
    {
        shared.stop_recovery("recovery_unavailable");
        return Err(ConnectionError::RecoveryUnavailable);
    }
    Ok(())
}

/// Explicitly leave the retained runtime and enter the existing authenticated
/// picker path.  This is the only recovery API that clears the cached runtime
/// binding/native view, and it does so before a new runtime can be acquired.
pub fn change_runtime(terminal_id: TerminalId, expected_epoch: u64) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    let profile = {
        let state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if state.operation_epoch != expected_epoch {
            return Err(ConnectionError::RecoveryStale);
        }
        state
            .profile
            .clone()
            .ok_or(ConnectionError::RecoveryUnavailable)?
    };
    // Cancel a possible in-flight replacement before starting the explicit
    // runtime change. The owner ticket is checked again by start_connection,
    // so a late handoff cannot install the old actor after this boundary.
    owner_transition(terminal_id)?.cancel_current();
    shared.invalidate_explicitly("runtime_changed");
    detach_all(&shared);
    start_connection(terminal_id, ConnectionStart::ManualReconnect(profile))
}

/// Select a pane by its stable tmux numeric ID. The desired selection is kept
/// while disconnected so the next reconnect restores the same mobile tab.
pub fn select_pane(terminal_id: TerminalId, pane_id: u64) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::RecoveryUnavailable)?;
    // Keep the queue acceptance and the synchronous input revoke under the
    // session lock. A full queue therefore leaves both the authoritative
    // selection and every transport untouched; an accepted request prevents
    // old input before the actor can commit the payload's new selection.
    let mut state = shared
        .session
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    if state.recovery.phase != RecoveryPhase::None || !state.runtime_operations_ready {
        return Err(ConnectionError::RecoveryUnavailable);
    }
    let pane = state
        .snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .cloned()
        .ok_or(ConnectionError::InvalidArgument)?;
    let epoch = state.operation_epoch;
    let herdr = state
        .endpoint
        .as_ref()
        .is_some_and(|endpoint| endpoint.backend == Backend::Herdr);
    sender
        .try_send(ControlRequest {
            epoch,
            command: ControlCommand::SelectPane {
                window_id: pane.window_id,
                pane_id,
            },
        })
        .map_err(|_| ConnectionError::RecoveryUnavailable)?;

    // Selection itself is committed by the serialized backend actor after it
    // has accepted this exact payload. Only the input gate is revoked here so
    // a queued command cannot race with a user keystroke. Detach while the
    // session lock is still held: the actor may already be waiting on the
    // queue, but it cannot acquire the state lock and attach a new target
    // before this accepted request's old bindings are revoked.
    state.terminal_input_ready = false;
    if herdr {
        let generation = state.generation;
        let ids = state.pane_terminals.values().copied().collect::<Vec<_>>();
        for id in ids {
            registry::detach_transport(id, generation);
        }
        registry::detach_transport(shared.terminal_id, generation);
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
    let shared = current_connection(terminal_id).ok();
    if let Some(shared) = &shared {
        let sender = shared
            .command_sender()
            .ok_or(ConnectionError::RecoveryUnavailable)?;
        // Queue acceptance and the visibility/revoke transition are one
        // synchronous ownership boundary. The actor cannot observe the
        // request before this lock is released, and a full queue returns with
        // the prior visibility and registry gate unchanged.
        let mut state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if state.generation != shared.generation {
            return Err(ConnectionError::RecoveryStale);
        }
        let epoch = state.operation_epoch;
        let herdr = state
            .endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.backend == Backend::Herdr);
        sender
            .try_send(ControlRequest {
                epoch,
                command: ControlCommand::SetTerminalVisible { visible },
            })
            .map_err(|_| ConnectionError::RecoveryUnavailable)?;

        apply_terminal_visibility_state(&mut state, visible);
        if !visible && herdr {
            // Hiding is an unconditional revoke boundary. Detach only after
            // the command has been accepted, and while the state lock blocks
            // the actor from reacquiring a controller in the gap.
            let generation = state.generation;
            let ids = state.pane_terminals.values().copied().collect::<Vec<_>>();
            for id in ids {
                registry::detach_transport(id, generation);
            }
            registry::detach_transport(shared.terminal_id, generation);
        }
    } else {
        let state = session_state(terminal_id);
        let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
        state.terminal_visible = visible;
        if !visible {
            state.terminal_input_ready = false;
        }
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
    if state.recovery.phase != RecoveryPhase::None || !state.runtime_operations_ready {
        return Err(ConnectionError::RecoveryUnavailable);
    }
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
    if state.recovery.phase != RecoveryPhase::None || !state.runtime_operations_ready {
        return Err(ConnectionError::RecoveryUnavailable);
    }
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
    let shared = current_connection(terminal_id)?;
    let epoch = {
        let state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if state.recovery.phase != RecoveryPhase::None || !state.runtime_operations_ready {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        state.operation_epoch
    };
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::RecoveryUnavailable)?;
    sender
        .try_send(ControlRequest { epoch, command })
        .map_err(|_| ConnectionError::RecoveryUnavailable)
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
        let runtime = state
            .endpoint
            .as_ref()
            .filter(|endpoint| endpoint.backend == Backend::Tmux)
            .and_then(|endpoint| endpoint.runtime.as_deref())
            .unwrap_or(tmux::SESSION_NAME);
        RuntimeSnapshot::tmux_for_runtime(&state.snapshot, runtime)
    };
    let mut snapshot = snapshot;
    snapshot.control = control_snapshot(&state);
    serde_json::to_string(&snapshot).map_err(|_| ConnectionError::Internal)
}

#[cfg(test)]
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
    state.runtime_candidates.clear();
    state.runtime_discovery = RuntimeDiscoverySnapshot::default();
    state.herdr_executable = None;
    state.operation_epoch = next_operation_epoch(state.operation_epoch);
    state.recovery = RecoverySnapshot::default();
    state.pending_confirmation_token = None;
    state.recovery_terminal_id = None;
    state.recovery_group_id = None;
    state.runtime_operations_ready = false;
    state.terminal_input_ready = false;
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
    state.meeterm_zoomed_window = None;
    state.meeterm_zoomed_pane = None;
    Ok(stale_terminals)
}

fn prepare_host_endpoint(
    terminal_id: TerminalId,
    options: &ConnectOptions,
) -> Result<Vec<TerminalId>, ConnectionError> {
    let state = session_state(terminal_id);
    let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
    state.endpoint = Some(SessionEndpoint::from_options(options));
    state.profile = None;
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
    state.meeterm_zoomed_window = None;
    state.meeterm_zoomed_pane = None;
    state.runtime_candidates.clear();
    state.runtime_discovery = RuntimeDiscoverySnapshot::default();
    state.herdr_executable = None;
    state.operation_epoch = next_operation_epoch(state.operation_epoch);
    state.recovery = RecoverySnapshot::default();
    state.pending_confirmation_token = None;
    state.recovery_terminal_id = None;
    state.recovery_group_id = None;
    state.runtime_operations_ready = false;
    state.terminal_input_ready = false;
    Ok(stale_terminals)
}

fn prepare_manual_reconnect(
    terminal_id: TerminalId,
    profile: &ConnectionProfile,
) -> Result<Vec<TerminalId>, ConnectionError> {
    let state = session_state(terminal_id);
    let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
    // Keep the credentials in the ManualReconnect value while removing the
    // selected runtime from the credential-free session metadata. The next
    // authenticated flow must discover a fresh candidate list rather than
    // briefly exposing or reusing the old binding.
    state.endpoint = Some(SessionEndpoint {
        host: profile.host.clone(),
        port: profile.port,
        username: profile.username.clone(),
        known_hosts_path: profile.known_hosts_path.clone(),
        backend: Backend::Tmux,
        runtime: None,
    });
    state.profile = None;
    state.runtime_candidates.clear();
    state.runtime_discovery = RuntimeDiscoverySnapshot::default();
    state.herdr_executable = None;
    let stale_terminals = state
        .pane_terminals
        .drain()
        .filter_map(|(_, id)| (id != terminal_id).then_some(id))
        .collect::<Vec<_>>();
    state.snapshot = SessionSnapshot::default();
    state.herdr = herdr_control::Metadata::default();
    state.selected_pane = None;
    state.meeterm_zoomed = false;
    state.meeterm_zoomed_window = None;
    state.meeterm_zoomed_pane = None;
    state.operation_epoch = next_operation_epoch(state.operation_epoch);
    state.recovery = RecoverySnapshot::default();
    state.pending_confirmation_token = None;
    state.recovery_terminal_id = None;
    state.recovery_group_id = None;
    state.runtime_operations_ready = false;
    state.terminal_input_ready = false;
    Ok(stale_terminals)
}

enum ConnectionStart {
    /// Rust-only direct-options compatibility path. It is intentionally not
    /// reachable from the production FFI/JNI bridges; the host-only bridges
    /// use [`ConnectionStart::Host`] and therefore enter the picker.
    #[cfg(test)]
    Options(ConnectOptions),
    Host(ConnectOptions),
    /// A transport loss after Ready may retry the selected binding. This mode
    /// is deliberately distinct from the public manual reconnect operation.
    AutomaticReconnect(ConnectionProfile),
    /// The UI-requested reconnect reauthenticates but must always rediscover
    /// and wait for an explicit runtime choice, even if one candidate exists.
    ManualReconnect(ConnectionProfile),
}

impl ConnectionStart {
    fn enters_picker(&self) -> bool {
        matches!(self, Self::Host(_) | Self::ManualReconnect(_))
    }

    fn is_automatic_reconnect(&self) -> bool {
        matches!(self, Self::AutomaticReconnect(_))
    }

    fn is_manual_reconnect(&self) -> bool {
        matches!(self, Self::ManualReconnect(_))
    }

    fn endpoint(&self) -> (&str, u16, &str, &Path) {
        match self {
            #[cfg(test)]
            Self::Options(options) => (
                &options.host,
                options.port,
                &options.username,
                &options.known_hosts_path,
            ),
            Self::Host(options) => (
                &options.host,
                options.port,
                &options.username,
                &options.known_hosts_path,
            ),
            Self::AutomaticReconnect(profile) | Self::ManualReconnect(profile) => (
                &profile.host,
                profile.port,
                &profile.username,
                &profile.known_hosts_path,
            ),
        }
    }
}

/// Give the previous generation a bounded chance to finish its explicit
/// cleanup/session teardown before a replacement generation changes the
/// shared session generation. This runs the wait on a short-lived blocking
/// helper thread so a native caller that happens to be on a Tokio worker
/// cannot starve the actor. The timeout is only a last-resort bound for a
/// dead transport; normal disconnects complete through `ConnectionShared::finish`.
fn wait_for_generation_finish(runtime: &'static Runtime, shared: Arc<ConnectionShared>) -> bool {
    wait_for_generation_finish_with_timeout(runtime, shared, REPLACEMENT_GRACE_TIMEOUT)
}

fn wait_for_generation_finish_with_timeout(
    runtime: &'static Runtime,
    shared: Arc<ConnectionShared>,
    timeout: Duration,
) -> bool {
    let already_finished = shared.info.lock().map(|info| info.finished).unwrap_or(true);
    if already_finished {
        return true;
    }

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let finished = runtime.block_on(async {
            tokio::time::timeout(timeout, shared.finished())
                .await
                .is_ok()
        });
        let _ = sender.send(finished);
    });
    receiver
        .recv_timeout(timeout.saturating_add(Duration::from_millis(100)))
        .unwrap_or(false)
}

/// Finish the ordered explicit shutdown when possible, otherwise revoke the
/// remaining transport and abort the actor. Keeping this fallback in one
/// helper makes Disconnect and runtime replacement share the same bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExplicitShutdownResult {
    finished: bool,
    cleanup: ZoomCleanupOutcome,
}

fn finish_or_force_explicit_shutdown(
    runtime: &'static Runtime,
    shared: Arc<ConnectionShared>,
    abort: tokio::task::AbortHandle,
) -> ExplicitShutdownResult {
    let finished = wait_for_generation_finish(runtime, Arc::clone(&shared));
    if !finished {
        if shared.has_zoom_cleanup_intent() {
            shared.record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
        }
        shared.cancel();
        abort.abort();
        // The actor may have been aborted before it could publish finish;
        // make the local lifecycle terminal and preserve the cleanup result.
        shared.finish(Err(FlowFailure::Stale));
    }
    ExplicitShutdownResult {
        finished,
        cleanup: shared.zoom_cleanup_outcome(),
    }
}

#[cfg(test)]
fn finish_or_force_explicit_shutdown_with_timeout(
    runtime: &'static Runtime,
    shared: Arc<ConnectionShared>,
    abort: tokio::task::AbortHandle,
    timeout: Duration,
) -> ExplicitShutdownResult {
    let finished = wait_for_generation_finish_with_timeout(runtime, Arc::clone(&shared), timeout);
    if !finished {
        if shared.has_zoom_cleanup_intent() {
            shared.record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
        }
        shared.cancel();
        abort.abort();
        shared.finish(Err(FlowFailure::Stale));
    }
    ExplicitShutdownResult {
        finished,
        cleanup: shared.zoom_cleanup_outcome(),
    }
}

fn destroy_stale_terminals(generation: u64, terminals: impl IntoIterator<Item = TerminalId>) {
    for id in terminals {
        registry::detach_transport(id, generation);
        registry::destroy_terminal(id);
    }
}

/// Tear down a prepared generation that lost its owner ticket before the
/// connection entry could be installed. This is deliberately separate from a
/// normal actor drop: there is no map entry for Disconnect to cancel, so the
/// local transport and its child native terminals must be revoked here.
fn abandon_uninstalled_connection(
    shared: &ConnectionShared,
    stale_terminals: impl IntoIterator<Item = TerminalId>,
) {
    shared.invalidate_explicitly("explicit_disconnect");
    shared.cancel();
    registry::detach_transport(shared.terminal_id, shared.generation);
    destroy_stale_terminals(shared.generation, stale_terminals);
}

fn start_connection(
    terminal_id: TerminalId,
    start: ConnectionStart,
) -> Result<(), ConnectionError> {
    let runtime = runtime()?;
    let owner = owner_transition(terminal_id)?;
    let (_serial, ticket) = owner.begin()?;
    let generation = next_generation();
    let reconnecting = start.is_automatic_reconnect();
    let manual_reconnect = start.is_manual_reconnect();

    let (host, port, _username, known_hosts_path) = start.endpoint();

    let old = {
        let _commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
        if !owner.install_allowed(ticket) {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .remove(&terminal_id)
    };
    let mut inherited_cleanup_warning = false;
    if let Some(old) = old {
        let old_shared = Arc::clone(&old.shared);
        old.shared.invalidate_explicitly("runtime_replaced");
        // A controller/transport that never acknowledges the explicit
        // cleanup cannot safely retain the old generation. The shared helper
        // forces both cancellation and task abort before the new generation
        // is allowed to touch SessionState.
        let shutdown = finish_or_force_explicit_shutdown(runtime, old.shared, old.abort);
        if shutdown.cleanup == ZoomCleanupOutcome::UnconfirmedOrFailed {
            inherited_cleanup_warning = true;
            old_shared.publish_layout_restore_warning();
        }
    }

    if !owner.install_allowed(ticket) {
        return Err(ConnectionError::RecoveryUnavailable);
    }

    let stale_terminals = match &start {
        #[cfg(test)]
        ConnectionStart::Options(options) => prepare_session_endpoint(terminal_id, options)?,
        ConnectionStart::Host(options) => prepare_host_endpoint(terminal_id, options)?,
        ConnectionStart::AutomaticReconnect(_) => Vec::new(),
        ConnectionStart::ManualReconnect(profile) => {
            prepare_manual_reconnect(terminal_id, profile)?
        }
    };
    if !owner.install_allowed(ticket) {
        destroy_stale_terminals(generation, stale_terminals);
        return Err(ConnectionError::RecoveryUnavailable);
    }
    if let Err(error) = registry::begin_remote(terminal_id, generation).map_err(map_terminal_error)
    {
        destroy_stale_terminals(generation, stale_terminals);
        return Err(error);
    }
    if manual_reconnect {
        // A manual runtime picker is a new binding, not a reconnect capture.
        // Replace the owner's native Term before authentication so stale
        // cells/history cannot be observed while the fresh picker is loading.
        if let Err(error) =
            registry::reset_remote_binding(terminal_id, generation).map_err(map_terminal_error)
        {
            registry::detach_transport(terminal_id, generation);
            destroy_stale_terminals(generation, stale_terminals);
            return Err(error);
        }
    }
    let shared = Arc::new(ConnectionShared::new(
        terminal_id,
        generation,
        host.to_owned(),
        port,
        known_hosts_path.to_owned(),
    ));
    if inherited_cleanup_warning {
        // Runtime replacement must not erase the old operation's cleanup
        // warning before the app has had a chance to display it.
        shared.publish_layout_restore_warning();
    }
    if reconnecting {
        // `ready_once` belongs to the actor, while the retained topology and
        // recovery phase belong to SessionState.  A replacement actor must
        // inherit the fact that its intent was already Ready; otherwise its
        // first failed reconstruction would be mistaken for a pre-Ready
        // connection and native retry would stop after one attempt.
        shared.ready_once.store(true, Ordering::Release);
    }
    {
        let mut state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        // Abort completion is asynchronous. Install the new generation and
        // discard the old actor's local cleanup authority in one operation.
        state.generation = generation;
        if !reconnecting {
            state.meeterm_zoomed = false;
            state.meeterm_zoomed_window = None;
            state.meeterm_zoomed_pane = None;
        }
        // Candidate IDs are scoped to the connection generation. A reconnect
        // must not expose or accept the previous generation's picker IDs
        // before a fresh discovery pass publishes replacements.
        state.runtime_candidates.clear();
        state.runtime_discovery = RuntimeDiscoverySnapshot::default();
        if reconnecting && state.recovery.phase != RecoveryPhase::Reconnecting {
            state.operation_epoch = next_operation_epoch(state.operation_epoch);
            state.recovery = RecoverySnapshot {
                phase: RecoveryPhase::Reconnecting,
                reason: "reconnecting".to_owned(),
                attempt: 0,
                max_attempts: workspace::DEFAULT_RECOVERY_MAX_ATTEMPTS,
                confirmation_token: String::new(),
            };
            state.runtime_operations_ready = false;
            state.terminal_input_ready = false;
        }
    }
    if reconnecting {
        shared.set_state(ConnectionState::Reconnecting);
    }
    if !owner.install_allowed(ticket) {
        abandon_uninstalled_connection(&shared, stale_terminals);
        return Err(ConnectionError::RecoveryUnavailable);
    }
    let (command_sender, command_receiver) = mpsc::channel(32);
    shared.set_commands(command_sender);
    let (start_gate_sender, start_gate_receiver) = oneshot::channel();
    let task_shared = Arc::clone(&shared);
    let join = runtime.spawn(async move {
        run_connection(task_shared, start, command_receiver, start_gate_receiver).await;
    });
    {
        let _commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
        if !owner.install_allowed(ticket) {
            drop(start_gate_sender);
            abandon_uninstalled_connection(&shared, stale_terminals);
            join.abort();
            return Err(ConnectionError::RecoveryUnavailable);
        }
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
        owner.finish(ticket);
        // Keep the gate closed until both the map install and ticket finish
        // are committed.  Disconnect cannot interleave while this lock is
        // held; if it wins later, the actor is already the map owner and its
        // shared cancellation flag stops it at the next lifecycle boundary.
        let _ = start_gate_sender.send(());
    }
    destroy_stale_terminals(generation, stale_terminals);
    Ok(())
}

/// Abort the session and leave the terminal in remote mode with local echo
/// disabled.  The registry entry remains available for state polling.
pub fn disconnect_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let owner = owner_transition(terminal_id)?;
    let (shared, abort) = {
        let _commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
        owner.cancel_current_locked();
        let entries = connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let Some(entry) = entries.get(&terminal_id) else {
            return Ok(());
        };
        entry.shared.invalidate_explicitly("explicit_disconnect");
        (Arc::clone(&entry.shared), entry.abort.clone())
    };

    detach_all(&shared);
    // The normal path hard-cancels only after the controller has sent and
    // acknowledged its cleanup and the authenticated session has closed. A
    // dead transport gets the bounded force path so Disconnect cannot remain
    // pending forever.
    match runtime() {
        Ok(runtime) => {
            let shutdown = finish_or_force_explicit_shutdown(runtime, Arc::clone(&shared), abort);
            if shutdown.cleanup == ZoomCleanupOutcome::UnconfirmedOrFailed {
                shared.publish_layout_restore_warning();
            }
        }
        Err(_) => {
            // An active connection implies the native runtime exists, but a
            // poisoned/unavailable runtime must still fail closed.
            if shared.has_zoom_cleanup_intent() {
                shared.record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
            }
            shared.cancel();
            abort.abort();
            shared.finish(Err(FlowFailure::Stale));
        }
    }
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
    entry.shared.invalidate_explicitly("explicit_disconnect");
    entry.shared.mark_closing();
    entry.shared.cancel();
    entry.abort.abort();
    Some(Arc::clone(&entry.shared))
}

/// Stop any owned SSH task after its terminal registry entry has been
/// explicitly destroyed.  View unmounts do not call this path; they retain the
/// stable terminal ID and its connection.
pub(crate) fn terminal_destroyed(terminal_id: TerminalId) {
    let owner = owner_transition(terminal_id).ok();
    let entry = owner.as_ref().and_then(|owner| {
        let _commit = owner.commit.lock().ok()?;
        owner.cancel_current_locked();
        connections().lock().ok()?.remove(&terminal_id)
    });
    if let Some(entry) = entry {
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

fn apply_terminal_visibility_state(state: &mut SessionState, visible: bool) {
    state.terminal_visible = visible;
    if !visible || state.recovery.phase != RecoveryPhase::None {
        state.terminal_input_ready = false;
    } else if state
        .endpoint
        .as_ref()
        .is_some_and(|endpoint| endpoint.backend == Backend::Tmux)
        && state.runtime_operations_ready
    {
        // tmux keeps its ordinary Control Mode transport while the native
        // view is hidden. Re-arm input only when the selected registry binding
        // is still Ready; this prevents a show command from publishing a
        // session-ready gate over a revoked native transport.
        state.terminal_input_ready = state.foreground
            && state.selected_pane.is_some_and(|selected| {
                state
                    .snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == selected)
                    && state.pane_terminals.get(&selected).is_some_and(|native| {
                        registry::transport_ready_or_local(*native, state.generation)
                    })
            });
    } else {
        // Herdr must reacquire its semantic controller and complete a fresh
        // full frame before the selected native terminal becomes an input
        // target again.
        state.terminal_input_ready = false;
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
        if self.shared.is_cancelled()
            || self.shared.explicit_cleanup_requested()
            || self.setup.is_cancelled()
        {
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
                if self.shared.is_cancelled()
                    || self.shared.explicit_cleanup_requested()
                    || self.setup.is_cancelled()
                {
                    return Ok(false);
                }
                self.shared.set_host_key(fingerprint, algorithm);
                Ok(true)
            }
            Ok(trust::Decision::Changed { known_fingerprint }) => {
                if self.shared.is_cancelled()
                    || self.shared.explicit_cleanup_requested()
                    || self.setup.is_cancelled()
                {
                    return Ok(false);
                }
                self.shared
                    .set_changed_key(fingerprint, algorithm, known_fingerprint);
                Ok(false)
            }
            Ok(trust::Decision::Unknown) => {
                if self.shared.is_cancelled()
                    || self.shared.explicit_cleanup_requested()
                    || self.setup.is_cancelled()
                {
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
                    _ = self.shared.explicit_cleanup() => return Ok(false),
                    _ = self.setup.cancelled() => return Ok(false),
                    result = tokio::time::timeout(HOST_KEY_PROMPT_TIMEOUT, receiver) => result,
                };
                match decision {
                    Ok(Ok(HostKeyDecision { accept: true })) => {
                        if self.shared.is_cancelled()
                            || self.shared.explicit_cleanup_requested()
                            || self.setup.is_cancelled()
                        {
                            return Ok(false);
                        }
                        match trust::learn(
                            &self.shared.host,
                            self.shared.info_port(),
                            &key,
                            &self.shared.known_hosts_path,
                        ) {
                            Ok(()) => {
                                if self.shared.is_cancelled()
                                    || self.shared.explicit_cleanup_requested()
                                    || self.setup.is_cancelled()
                                {
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
                        if self.shared.is_cancelled()
                            || self.shared.explicit_cleanup_requested()
                            || self.setup.is_cancelled()
                        {
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
    TmuxRuntimeMissing,
    TmuxRuntimeCollision,
    TmuxRuntimeUnknown,
    TmuxTopologyUnsafe,
    TmuxDiscoveryMissing,
    TmuxDiscoveryPermission,
    TmuxDiscoveryMalformed,
    TmuxDiscoveryTimeout,
    RuntimeSelection,
    HerdrDiscoveryMissing,
    HerdrDiscoveryIncompatible,
    HerdrDiscoveryMalformed,
    HerdrDiscoveryPermission,
    HerdrDiscoveryTimeout,
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
            Self::TmuxRuntimeMissing => (
                "tmux_runtime_missing",
                "The selected tmux session disappeared or its server was replaced. Choose a runtime again.",
            ),
            Self::TmuxRuntimeCollision => (
                "tmux_runtime_collision",
                "A tmux session with that name already exists.",
            ),
            Self::TmuxRuntimeUnknown => (
                "tmux_runtime_unknown",
                "The tmux session creation outcome could not be verified; no retry was attempted.",
            ),
            Self::TmuxTopologyUnsafe => (
                "tmux_topology_unsafe",
                "The tmux window may be linked to another session; the topology mutation was refused.",
            ),
            Self::TmuxDiscoveryMissing => (
                "tmux_missing",
                "tmux is not installed or is not available in the remote SSH command path.",
            ),
            Self::TmuxDiscoveryPermission => (
                "tmux_permission",
                "The remote tmux server could not be listed because access was denied.",
            ),
            Self::TmuxDiscoveryMalformed => (
                "tmux_discovery_malformed",
                "The remote tmux session list was malformed or exceeded its bound.",
            ),
            Self::TmuxDiscoveryTimeout => (
                "tmux_discovery_timeout",
                "The remote tmux session list did not finish before the discovery deadline.",
            ),
            Self::RuntimeSelection => (
                "runtime_selection",
                "The selected runtime candidate is no longer available. Refresh the runtime list.",
            ),
            Self::HerdrDiscoveryMissing => (
                "herdr_missing",
                "Herdr 0.9.0 was not found in the remote SSH command path or common install paths.",
            ),
            Self::HerdrDiscoveryIncompatible => (
                "herdr_incompatible",
                "A remote Herdr executable was found, but it is not compatible with 0.9.0.",
            ),
            Self::HerdrDiscoveryMalformed => (
                "herdr_discovery_malformed",
                "The Herdr session list was malformed or exceeded its bound.",
            ),
            Self::HerdrDiscoveryPermission => (
                "herdr_permission",
                "The remote Herdr session list could not be read.",
            ),
            Self::HerdrDiscoveryTimeout => (
                "herdr_discovery_timeout",
                "The remote Herdr session list did not finish before the discovery deadline.",
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
const MAX_RUNTIME_COMMAND_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const RECOVERY_TOKEN_MAX_BYTES: usize = 128;

fn next_operation_epoch(current: u64) -> u64 {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

fn sanitize_recovery_reason(reason: &str) -> String {
    let mut sanitized = String::with_capacity(reason.len().min(ERROR_CODE_CAPACITY));
    for byte in reason.bytes().take(ERROR_CODE_CAPACITY) {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' {
            sanitized.push(byte as char);
        } else {
            sanitized.push('_');
        }
    }
    sanitized
}

fn recovery_reason_message(reason: &str) -> String {
    match reason {
        "transport" | "network" | "channel" | "remote_closed" => {
            "The remote connection was lost; the retained workspace is available for retry."
                .to_owned()
        }
        "host_key_changed" => "The server host key changed; connection refused.".to_owned(),
        "host_key_rejected" => "The server host key was not accepted.".to_owned(),
        "host_key_timeout" => "The host-key confirmation timed out.".to_owned(),
        "host_key_store" => "The host-key trust file could not be updated safely.".to_owned(),
        "authentication_failed" | "auth_failed" => "SSH authentication failed.".to_owned(),
        "foreground_lost" => {
            "The connection is being checked after returning to the foreground.".to_owned()
        }
        "runtime_identity_uncertain" => {
            "The selected runtime could not be verified safely.".to_owned()
        }
        "herdr_terminal_missing" => {
            "The selected Herdr terminal is no longer available.".to_owned()
        }
        "controller_conflict" => "Another controller owns the selected Herdr terminal.".to_owned(),
        "explicit_disconnect" => "The connection was disconnected.".to_owned(),
        "runtime_changed" => "The runtime selection is being changed.".to_owned(),
        "layout_restore_unconfirmed" => {
            "The connection closed, but the desktop layout could not be confirmed as restored."
                .to_owned()
        }
        "retry_exhausted" => {
            "Automatic recovery stopped; retry or choose another runtime.".to_owned()
        }
        _ => "Recovery stopped while preserving the last known workspace.".to_owned(),
    }
}

fn is_terminal_security_error(code: &str) -> bool {
    matches!(
        code,
        "host_key_changed"
            | "host_key_rejected"
            | "host_key_timeout"
            | "host_key_store"
            | "authentication_failed"
            | "auth_failed"
            | "host_authentication_failed"
    )
}

/// Keep a callback-published host/auth failure authoritative when the SSH
/// transport returns a less specific error (for example Network). The
/// recovery snapshot uses the canonical authentication spelling while the
/// fixed connection snapshot keeps its existing `auth_failed` compatibility
/// code when that is what the callback published.
fn preserved_recovery_reason(shared: &ConnectionShared) -> Option<&'static str> {
    let info = shared.info.lock().ok()?;
    if info.state != ConnectionState::Failed {
        return None;
    }
    match info.error_code.as_str() {
        "host_key_changed" => Some("host_key_changed"),
        "host_key_rejected" => Some("host_key_rejected"),
        "host_key_timeout" => Some("host_key_timeout"),
        "host_key_store" => Some("host_key_store"),
        "authentication_failed" | "auth_failed" | "host_authentication_failed" => {
            Some("authentication_failed")
        }
        _ => None,
    }
}

fn recovery_reason_for_failure(failure: FlowFailure) -> &'static str {
    match failure {
        FlowFailure::HerdrController => "controller_conflict",
        FlowFailure::HerdrSessionMissing => "herdr_terminal_missing",
        FlowFailure::HerdrIncompatible
        | FlowFailure::HerdrUnsupported
        | FlowFailure::HerdrDiscoveryIncompatible => "herdr_incompatible",
        FlowFailure::HerdrProtocol
        | FlowFailure::HerdrDiscoveryMalformed
        | FlowFailure::HerdrDiscoveryPermission
        | FlowFailure::HerdrDiscoveryTimeout
        | FlowFailure::TmuxProtocol
        | FlowFailure::TmuxRuntimeMissing
        | FlowFailure::TmuxRuntimeCollision
        | FlowFailure::TmuxRuntimeUnknown
        | FlowFailure::TmuxTopologyUnsafe => "runtime_identity_uncertain",
        FlowFailure::HerdrMissing
        | FlowFailure::HerdrForwarding
        | FlowFailure::HerdrDiscoveryMissing => "herdr_session_missing",
        FlowFailure::Authentication => "authentication_failed",
        _ => failure.details().0,
    }
}

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
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => Err(FlowFailure::Stale),
        result = tokio::time::timeout(timeout, future) => {
            match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) | Err(_) => Err(failure),
            }
        }
    }
}

/// Await one SSH setup operation while preserving the distinction between an
/// operation error and an elapsed deadline. Runtime discovery uses the latter
/// to report an uncertain picker section instead of labelling every failure as
/// a permission or malformed-output problem.
async fn await_stage_with_timeout<F, T, E>(
    shared: &ConnectionShared,
    future: F,
    timeout: Duration,
    failure: FlowFailure,
    timeout_failure: FlowFailure,
) -> Result<T, FlowFailure>
where
    F: Future<Output = Result<T, E>>,
{
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => Err(FlowFailure::Stale),
        result = tokio::time::timeout(timeout, future) => {
            match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) => Err(failure),
                Err(_) => Err(timeout_failure),
            }
        }
    }
}

/// Bounded output from one non-interactive SSH exec channel. Discovery uses
/// this helper so tmux and Herdr can classify their own exit status without
/// sharing a command, socket, or failure state.
struct RemoteCommandOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) exit_status: Option<u32>,
}

async fn run_remote_command_with_timeout(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    command: String,
    max_bytes: usize,
    overflow: FlowFailure,
    timeout_failure: FlowFailure,
) -> Result<RemoteCommandOutput, FlowFailure> {
    run_remote_command_with_timeout_at_epoch(
        shared,
        session,
        command,
        max_bytes,
        overflow,
        timeout_failure,
        None,
    )
    .await
}

async fn run_remote_command_with_timeout_at_epoch(
    shared: &ConnectionShared,
    session: &client::Handle<HostKeyHandler>,
    command: String,
    max_bytes: usize,
    overflow: FlowFailure,
    timeout_failure: FlowFailure,
    expected_epoch: Option<u64>,
) -> Result<RemoteCommandOutput, FlowFailure> {
    if expected_epoch.is_some_and(|epoch| !shared.current_request_epoch(epoch)) {
        return Err(FlowFailure::Stale);
    }
    let mut channel = await_stage_with_timeout(
        shared,
        session.channel_open_session(),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
        timeout_failure,
    )
    .await?;
    if expected_epoch.is_some_and(|epoch| !shared.current_request_epoch(epoch)) {
        let _ = channel.close().await;
        return Err(FlowFailure::Stale);
    }
    await_stage_with_timeout(
        shared,
        channel.exec(true, command),
        SSH_STAGE_TIMEOUT,
        FlowFailure::Channel,
        timeout_failure,
    )
    .await?;
    if expected_epoch.is_some_and(|epoch| !shared.current_request_epoch(epoch)) {
        let _ = channel.close().await;
        return Err(FlowFailure::Stale);
    }

    let read = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    if stdout.len().saturating_add(data.len()) > max_bytes {
                        return Err(overflow);
                    }
                    stdout.extend_from_slice(&data);
                }
                ChannelMsg::ExtendedData { data, .. } => {
                    if stderr.len().saturating_add(data.len()) > max_bytes {
                        return Err(overflow);
                    }
                    stderr.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus {
                    exit_status: status,
                } => exit_status = Some(status),
                ChannelMsg::Failure => return Err(FlowFailure::Channel),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok(RemoteCommandOutput {
            stdout,
            stderr,
            exit_status,
        })
    };
    let result = tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => Err(FlowFailure::Stale),
        result = tokio::time::timeout(SSH_STAGE_TIMEOUT, read) => result.map_err(|_| timeout_failure)?,
    }?;
    if expected_epoch.is_some_and(|epoch| !shared.current_request_epoch(epoch)) {
        return Err(FlowFailure::Stale);
    }
    Ok(result)
}

async fn await_channel_message(
    shared: &ConnectionShared,
    reader: &mut russh::ChannelReadHalf,
) -> Result<Option<ChannelMsg>, FlowFailure> {
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => Err(FlowFailure::Stale),
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
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        return Err(FlowFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(FlowFailure::Stale),
        _ = shared.explicit_cleanup() => Err(FlowFailure::Stale),
        message = reader.wait() => Ok(message),
    }
}

/// Run one tmux Control Mode client. The SSH channel deliberately has no PTY:
/// Control Mode is a line protocol and `%output` carries the remote panes'
/// byte stream. This is `-C`, rather than `-CC`; `-CC` additionally disables
/// tmux's client-side echo behavior for an embedded terminal and is not needed
/// when the command is executed over a non-PTY SSH channel.
async fn wait_for_start_gate(shared: &ConnectionShared, gate: oneshot::Receiver<()>) -> bool {
    tokio::select! {
        _ = shared.cancelled() => false,
        _ = shared.explicit_cleanup() => false,
        result = gate => result.is_ok() && !shared.is_cancelled() && !shared.explicit_cleanup_requested(),
    }
}

async fn run_connection(
    shared: Arc<ConnectionShared>,
    mut start: ConnectionStart,
    mut commands: mpsc::Receiver<ControlRequest>,
    start_gate: oneshot::Receiver<()>,
) {
    // A spawned actor is not an owner until its ConnectionEntry and ticket
    // commit have both been installed.  In particular, Disconnect can remove
    // an empty map slot and cancel the ticket without allowing this task to
    // open SSH channels, discover runtimes, or acquire a controller first.
    let gate_open = wait_for_start_gate(&shared, start_gate).await;
    if !gate_open {
        shared.clear_commands();
        detach_all(&shared);
        shared.clear_owned_zoom();
        shared.finish(Err(FlowFailure::Stale));
        return;
    }
    let mut retries: u32 = 0;
    let result = loop {
        let ready_epoch = shared.ready_epoch();
        let result = run_connection_flow(Arc::clone(&shared), start, &mut commands).await;
        let disposition = match result {
            Ok(()) => None,
            Err(failure) => Some(retry_disposition(&shared, failure)),
        };
        if let Some(RetryDisposition::Retry) = disposition
            && shared.has_been_ready()
            && !shared.is_cancelled()
            && !shared.explicit_cleanup_requested()
            && shared.recovery_phase() != RecoveryPhase::Stopped
        {
            let attempt = retries.saturating_add(1);
            let _ = shared.begin_recovery(
                recovery_reason_for_failure(match result {
                    Err(failure) => failure,
                    Ok(()) => unreachable!("successful flow has no retry disposition"),
                }),
                attempt,
            );
            while commands.try_recv().is_ok() {}
        }
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
        let disposition = disposition.expect("failed flow has a retry disposition");
        if !matches!(disposition, RetryDisposition::Retry) || retries >= AUTO_RECONNECT_MAX_ATTEMPTS
        {
            if shared.has_been_ready()
                && !shared.is_cancelled()
                && shared.recovery_phase() != RecoveryPhase::Stopped
            {
                let reason = match disposition {
                    RetryDisposition::Stop(reason) => reason,
                    RetryDisposition::Retry => "retry_exhausted",
                };
                shared.stop_recovery(reason);
            }
            shared.clear_owned_zoom();
            break Err(failure);
        }

        let delay = reconnect_delay(retries);
        if !wait_for_reconnect(&shared, delay).await {
            if shared.has_been_ready()
                && !shared.is_cancelled()
                && shared.recovery_phase() != RecoveryPhase::Stopped
            {
                shared.stop_recovery("automatic_reconnect_disabled");
            }
            shared.clear_owned_zoom();
            break Err(failure);
        }
        let Some(profile) = retained_profile(&shared) else {
            shared.clear_owned_zoom();
            break Err(failure);
        };
        retries = retries.saturating_add(1);
        start = ConnectionStart::AutomaticReconnect(profile);
    };
    shared.clear_commands();
    detach_all(&shared);
    shared.clear_owned_zoom();
    // Explicit shutdown is a two-phase boundary. The controller/backend has
    // already had its cleanup opportunity and the authenticated session has
    // unwound by this point; hard cancellation now closes any remaining
    // russh/CancellableStream task before publishing the final state.
    if shared.explicit_cleanup_requested() {
        shared.cancel();
    }
    shared.finish(result);
}

fn retained_profile(shared: &ConnectionShared) -> Option<ConnectionProfile> {
    shared
        .session
        .lock()
        .ok()
        .and_then(|state| state.profile.clone())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryDisposition {
    Retry,
    Stop(&'static str),
}

fn retry_disposition(shared: &ConnectionShared, failure: FlowFailure) -> RetryDisposition {
    if automatic_retry_allowed(shared, failure) {
        RetryDisposition::Retry
    } else {
        RetryDisposition::Stop(
            preserved_recovery_reason(shared)
                .unwrap_or_else(|| recovery_reason_for_failure(failure)),
        )
    }
}

fn automatic_retry_allowed(shared: &ConnectionShared, failure: FlowFailure) -> bool {
    if shared.is_cancelled()
        || shared.explicit_cleanup_requested()
        || !shared.automatic_reconnect_enabled()
        || !shared.has_been_ready()
    {
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
        if shared.is_cancelled()
            || shared.explicit_cleanup_requested()
            || !shared.automatic_reconnect_enabled()
        {
            return false;
        }
        if !shared.is_foreground() {
            let notified = shared.retry_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if shared.is_cancelled()
                || shared.explicit_cleanup_requested()
                || !shared.automatic_reconnect_enabled()
            {
                return false;
            }
            if shared.is_foreground() {
                continue;
            }
            tokio::select! {
                _ = shared.cancelled() => return false,
                _ = shared.explicit_cleanup() => return false,
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
            _ = shared.explicit_cleanup() => return false,
            _ = &mut notified => {},
            _ = tokio::time::sleep_until(deadline) => return true,
        }
    }
}

async fn run_connection_flow(
    shared: Arc<ConnectionShared>,
    start: ConnectionStart,
    commands: &mut mpsc::Receiver<ControlRequest>,
) -> Result<(), FlowFailure> {
    let automatic_reconnect = start.is_automatic_reconnect();
    let picker = start.enters_picker();
    let mut profile = match start {
        ConnectionStart::AutomaticReconnect(profile)
        | ConnectionStart::ManualReconnect(profile) => profile,
        #[cfg(test)]
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
                tmux_identity: None,
                herdr_executable: None,
            };
            shared.set_profile(profile.clone());
            profile
        }
        ConnectionStart::Host(ConnectOptions {
            host,
            port,
            username,
            credentials,
            known_hosts_path,
            ..
        }) => {
            let credentials = decode_credentials(credentials)?;
            ConnectionProfile {
                host,
                port,
                username,
                known_hosts_path,
                credentials,
                backend: Backend::Tmux,
                runtime: None,
                tmux_identity: None,
                herdr_executable: None,
            }
        }
    };
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
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
        _ = shared.explicit_cleanup() => {
            // No authenticated backend exists yet, so there is no remote
            // cleanup to preserve. Cancel only this pre-auth russh stream;
            // established sessions take the two-phase path below.
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

    let result = run_authenticated_session(
        &shared,
        &mut profile,
        &mut session,
        commands,
        picker,
        automatic_reconnect,
    )
    .await;
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

fn decode_credentials(credentials: AuthOptions) -> Result<StoredCredentials, FlowFailure> {
    match credentials {
        AuthOptions::PublicKey {
            mut private_key,
            mut passphrase,
        } => {
            let decoded_key =
                keys::decode_secret_key(&private_key, passphrase.as_deref().map(String::as_str));
            // Clear the caller-provided PEM and passphrase before the first
            // await. The reconnect profile retains only the parsed key.
            private_key.zeroize();
            if let Some(passphrase) = passphrase.as_mut() {
                passphrase.zeroize();
            }
            let key = decoded_key.map_err(|_| FlowFailure::KeyFile)?;
            Ok(StoredCredentials::PublicKey { key: Arc::new(key) })
        }
        AuthOptions::Password { password } => Ok(StoredCredentials::Password {
            // Move the already zeroizing input into the reconnect profile.
            password: Arc::new(password),
        }),
    }
}

async fn authenticate_session(
    shared: &Arc<ConnectionShared>,
    profile: &ConnectionProfile,
    session: &mut client::Handle<HostKeyHandler>,
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
        // Publish the terminal credential failure before the outer transport
        // loop classifies the result. This also protects the fatal state if a
        // lower russh layer reports the refusal through a generic transport
        // error rather than `FlowFailure::Authentication`.
        shared.fail("auth_failed", "SSH authentication failed.");
        return Err(FlowFailure::Authentication);
    }
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        return Err(FlowFailure::Stale);
    }
    Ok(())
}

async fn run_authenticated_session(
    shared: &Arc<ConnectionShared>,
    profile: &mut ConnectionProfile,
    session: &mut client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
    picker: bool,
    automatic_reconnect: bool,
) -> Result<(), FlowFailure> {
    authenticate_session(shared, profile, session).await?;
    if picker {
        return run_runtime_picker(shared, profile, session, commands).await;
    }
    shared.set_profile(profile.clone());
    if automatic_reconnect && profile.backend == Backend::Herdr {
        return herdr_control::recover(shared, profile, session, commands).await;
    }
    let result = run_selected_backend(shared, profile, session, commands).await;
    if let Err(failure) = result {
        if !automatic_reconnect && should_return_to_runtime_picker(shared, failure) {
            return run_runtime_picker(shared, profile, session, commands).await;
        }
        // A selected runtime disappearing during automatic recovery is a
        // retained-screen stop. Fresh/manual picker flows are the only place
        // where a local runtime failure may return to runtime selection.
        return Err(failure);
    }
    Ok(())
}

async fn run_selected_backend(
    shared: &Arc<ConnectionShared>,
    profile: &mut ConnectionProfile,
    session: &mut client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
) -> Result<(), FlowFailure> {
    match profile.backend {
        Backend::Tmux => control::run(shared, profile, session, commands).await,
        Backend::Herdr => herdr_control::run(shared, profile, session, commands).await,
    }
}

fn discovery_section_error(failure: FlowFailure) -> RuntimeSection {
    let (code, message) = failure.details();
    RuntimeSection {
        state: RuntimeSectionState::Error,
        candidates: Vec::new(),
        error_code: Some(code.to_owned()),
        error_message: Some(message.to_owned()),
    }
}

fn discovery_section<T>(
    result: Result<Vec<T>, FlowFailure>,
    backend: Backend,
    generation: u64,
    revision: u64,
    bindings: &mut HashMap<String, RuntimeBinding>,
) -> Result<RuntimeSection, FlowFailure>
where
    T: RuntimeDiscoveryItem,
{
    match result {
        Ok(items) => {
            if items.len() > tmux::MAX_RUNTIME_SESSIONS {
                return Ok(discovery_section_error(match backend {
                    Backend::Tmux => FlowFailure::TmuxDiscoveryMalformed,
                    Backend::Herdr => FlowFailure::HerdrDiscoveryMalformed,
                }));
            }
            let mut candidates = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                let id = format!(
                    "runtime-{generation}-{revision}-{}-{index}",
                    match backend {
                        Backend::Tmux => "tmux",
                        Backend::Herdr => "herdr",
                    }
                );
                let (name, state, selectable, suggested, binding) = item.into_picker_parts();
                candidates.push(RuntimeCandidate {
                    id: id.clone(),
                    backend,
                    name,
                    state,
                    selectable,
                    suggested,
                    error_code: None,
                    error_message: None,
                });
                bindings.insert(id, binding);
            }
            Ok(RuntimeSection {
                state: if candidates.is_empty() {
                    RuntimeSectionState::Empty
                } else {
                    RuntimeSectionState::Success
                },
                candidates,
                error_code: None,
                error_message: None,
            })
        }
        Err(failure) if matches!(failure, FlowFailure::Stale) => Err(failure),
        Err(failure) => Ok(discovery_section_error(failure)),
    }
}

trait RuntimeDiscoveryItem {
    fn into_picker_parts(self) -> (String, RuntimeState, bool, bool, RuntimeBinding);
}

impl RuntimeDiscoveryItem for tmux::SessionIdentity {
    fn into_picker_parts(self) -> (String, RuntimeState, bool, bool, RuntimeBinding) {
        let suggested = self.name == tmux::SESSION_NAME;
        let name = self.name.clone();
        (
            name,
            RuntimeState::Running,
            true,
            suggested,
            RuntimeBinding::Tmux(self),
        )
    }
}

impl RuntimeDiscoveryItem for herdr_control::DiscoveredSession {
    fn into_picker_parts(self) -> (String, RuntimeState, bool, bool, RuntimeBinding) {
        let suggested = self.default;
        let state = if self.running {
            RuntimeState::Running
        } else {
            RuntimeState::Stopped
        };
        let name = self.name.clone();
        (
            name.clone(),
            state,
            self.running,
            suggested,
            RuntimeBinding::Herdr {
                name,
                default: self.default,
                executable: self.executable,
            },
        )
    }
}

fn clear_runtime_binding(shared: &ConnectionShared) -> Result<(), FlowFailure> {
    if shared.has_been_ready() {
        // Picker reset is only a pre-Ready transition. Keeping this defensive
        // boundary here as well as at its call sites prevents a post-Ready
        // runtime-local failure from destroying the retained Term if a future
        // lifecycle path accidentally reuses the picker helper.
        return Err(FlowFailure::Stale);
    }
    let stale_terminals = {
        let mut state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        if state.generation != shared.generation {
            return Err(FlowFailure::Stale);
        }
        state.profile = None;
        let stale_terminals = state
            .pane_terminals
            .drain()
            .filter_map(|(_, id)| (id != shared.terminal_id).then_some(id))
            .collect::<Vec<_>>();
        state.snapshot = SessionSnapshot::default();
        state.herdr = herdr_control::Metadata::default();
        state.selected_pane = None;
        state.meeterm_zoomed = false;
        state.meeterm_zoomed_window = None;
        state.meeterm_zoomed_pane = None;
        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        state.recovery = RecoverySnapshot::default();
        state.pending_confirmation_token = None;
        state.recovery_terminal_id = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        stale_terminals
    };

    // The old actor has already returned and dropped its backend client when
    // this helper runs. Revoke the owner transport and reset its native Term
    // before a new backend can attach in the same SSH generation.
    registry::detach_transport(shared.terminal_id, shared.generation);
    registry::reset_remote_binding(shared.terminal_id, shared.generation)
        .map_err(|_| FlowFailure::Stale)?;
    for id in stale_terminals {
        registry::detach_transport(id, shared.generation);
        registry::destroy_terminal(id);
    }
    Ok(())
}

fn mark_runtime_failure(shared: &ConnectionShared, candidate_id: &str, failure: FlowFailure) {
    let (code, message) = failure.details();
    if let Ok(mut state) = shared.session.lock()
        && state.generation == shared.generation
    {
        if let Some(candidate) = state
            .runtime_discovery
            .tmux
            .candidates
            .iter_mut()
            .find(|candidate| candidate.id == candidate_id)
        {
            candidate.selectable = false;
            candidate.error_code = Some(code.to_owned());
            candidate.error_message = Some(message.to_owned());
        } else if let Some(candidate) = state
            .runtime_discovery
            .herdr
            .candidates
            .iter_mut()
            .find(|candidate| candidate.id == candidate_id)
        {
            candidate.selectable = false;
            candidate.error_code = Some(code.to_owned());
            candidate.error_message = Some(message.to_owned());
        }
    }
}

fn mark_runtime_section_failure(shared: &ConnectionShared, backend: Backend, failure: FlowFailure) {
    // Runtime creation is currently a tmux-only operation. In particular, a
    // tmux create result must never replace or annotate the independent
    // Herdr discovery section.
    if backend != Backend::Tmux {
        return;
    }
    if let Ok(mut state) = shared.session.lock()
        && state.generation == shared.generation
    {
        let section = &mut state.runtime_discovery.tmux;
        // Keep a successful/empty discovery visible while reporting the
        // operation result beside it. A genuine discovery error stays
        // authoritative and is not overwritten by a later create failure.
        if matches!(
            section.state,
            RuntimeSectionState::Success | RuntimeSectionState::Empty
        ) {
            let (code, message) = failure.details();
            section.error_code = Some(code.to_owned());
            section.error_message = Some(message.to_owned());
        }
    }
}

fn clear_tmux_create_error(shared: &ConnectionShared) -> Result<(), ConnectionError> {
    let mut state = shared
        .session
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    if state.generation != shared.generation {
        return Err(ConnectionError::RuntimeSelectionUnavailable);
    }
    let section = &mut state.runtime_discovery.tmux;
    if matches!(
        section.state,
        RuntimeSectionState::Success | RuntimeSectionState::Empty
    ) {
        section.error_code = None;
        section.error_message = None;
    }
    Ok(())
}

async fn discover_and_publish(
    shared: &Arc<ConnectionShared>,
    base: &ConnectionProfile,
    session: &client::Handle<HostKeyHandler>,
) -> Result<(), FlowFailure> {
    let revision = {
        let mut state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        if state.generation != shared.generation || shared.is_cancelled() {
            return Err(FlowFailure::Stale);
        }
        let revision = state
            .runtime_discovery
            .discovery_revision
            .saturating_add(1)
            .max(1);
        state.runtime_candidates.clear();
        state.runtime_discovery = RuntimeDiscoverySnapshot::loading(shared.generation, revision);
        revision
    };
    shared.set_state(ConnectionState::DiscoveringRuntimes);

    // The two commands use separate SSH channels and have independent bounds
    // and result mapping. A missing/broken backend therefore cannot hide the
    // other backend's candidates.
    let expected_herdr_executable = {
        let state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
        base.herdr_executable
            .as_deref()
            .or(state.herdr_executable.as_deref())
            .map(str::to_owned)
    };
    let (tmux_result, herdr_result) = tokio::join!(
        control::discover(shared, session),
        herdr_control::discover(shared, session, expected_herdr_executable.as_deref()),
    );
    let mut bindings = HashMap::new();
    let tmux = discovery_section(
        tmux_result,
        Backend::Tmux,
        shared.generation,
        revision,
        &mut bindings,
    )?;
    let herdr = match herdr_result {
        Ok(discovered) => {
            let executable = discovered.executable.clone();
            let section = discovery_section(
                Ok(discovered.sessions),
                Backend::Herdr,
                shared.generation,
                revision,
                &mut bindings,
            )?;
            if let Ok(mut state) = shared.session.lock()
                && state.generation == shared.generation
            {
                state.herdr_executable = Some(executable);
            }
            section
        }
        Err(failure) if matches!(failure, FlowFailure::Stale) => return Err(failure),
        Err(failure) => discovery_section::<herdr_control::DiscoveredSession>(
            Err(failure),
            Backend::Herdr,
            shared.generation,
            revision,
            &mut bindings,
        )?,
    };
    let mut state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
    if state.generation != shared.generation || shared.is_cancelled() {
        return Err(FlowFailure::Stale);
    }
    state.runtime_candidates = bindings;
    state.runtime_discovery = RuntimeDiscoverySnapshot {
        connection_generation: shared.generation,
        discovery_revision: revision,
        tmux,
        herdr,
    };
    Ok(())
}

fn profile_for_binding(base: &ConnectionProfile, binding: &RuntimeBinding) -> ConnectionProfile {
    let mut profile = base.clone();
    profile.tmux_identity = None;
    profile.herdr_executable = None;
    match binding {
        RuntimeBinding::Tmux(identity) => {
            profile.backend = Backend::Tmux;
            profile.runtime = Some(identity.name.clone());
            profile.tmux_identity = Some(identity.clone());
        }
        RuntimeBinding::Herdr {
            name,
            default,
            executable,
        } => {
            profile.backend = Backend::Herdr;
            profile.runtime = (!*default).then(|| name.clone());
            profile.herdr_executable = Some(executable.clone());
        }
    }
    profile
}

fn is_runtime_local_failure(failure: FlowFailure) -> bool {
    matches!(
        failure,
        FlowFailure::TmuxRuntimeMissing
            | FlowFailure::TmuxRuntimeCollision
            | FlowFailure::TmuxRuntimeUnknown
            | FlowFailure::TmuxDiscoveryMissing
            | FlowFailure::TmuxDiscoveryPermission
            | FlowFailure::TmuxDiscoveryMalformed
            | FlowFailure::TmuxDiscoveryTimeout
            | FlowFailure::HerdrMissing
            | FlowFailure::HerdrSessionMissing
            | FlowFailure::HerdrIncompatible
            | FlowFailure::HerdrUnsupported
            | FlowFailure::HerdrForwarding
            | FlowFailure::HerdrProtocol
            | FlowFailure::HerdrDiscoveryMissing
            | FlowFailure::HerdrDiscoveryIncompatible
            | FlowFailure::HerdrDiscoveryMalformed
            | FlowFailure::HerdrDiscoveryPermission
            | FlowFailure::HerdrDiscoveryTimeout
            | FlowFailure::RuntimeSelection
    )
}

/// Only a runtime that has never published Ready may return to the picker.
/// Once the selected backend has become durable, a local runtime failure is a
/// retained-work recovery/stop result; the picker path would clear the native
/// Term and selected binding before the user can recover it.
fn should_return_to_runtime_picker(shared: &ConnectionShared, failure: FlowFailure) -> bool {
    is_runtime_local_failure(failure)
        && !shared.has_been_ready()
        && !shared.explicit_cleanup_requested()
}

async fn run_runtime_picker(
    shared: &Arc<ConnectionShared>,
    base: &ConnectionProfile,
    session: &mut client::Handle<HostKeyHandler>,
    commands: &mut mpsc::Receiver<ControlRequest>,
) -> Result<(), FlowFailure> {
    // This helper is intentionally pre-Ready only. A defensive guard keeps a
    // future call site from turning a post-Ready runtime-local error into the
    // destructive picker reset below.
    if shared.has_been_ready() || shared.explicit_cleanup_requested() {
        return Err(FlowFailure::Stale);
    }
    clear_runtime_binding(shared)?;
    discover_and_publish(shared, base, session).await?;
    loop {
        shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        let command = tokio::select! {
            _ = shared.cancelled() => return Err(FlowFailure::Stale),
            _ = shared.explicit_cleanup() => return Err(FlowFailure::Stale),
            command = commands.recv() => command,
        };
        let Some(request) = command else {
            return Err(FlowFailure::Stale);
        };
        if !shared.current_request_epoch(request.epoch) {
            continue;
        }
        match request.command {
            ControlCommand::RefreshRuntimes => {
                discover_and_publish(shared, base, session).await?;
            }
            ControlCommand::SelectRuntime { candidate_id } => {
                let binding = {
                    let state = shared.session.lock().map_err(|_| FlowFailure::Stale)?;
                    let selectable = state
                        .runtime_discovery
                        .tmux
                        .candidates
                        .iter()
                        .chain(state.runtime_discovery.herdr.candidates.iter())
                        .any(|candidate| candidate.id == candidate_id && candidate.selectable);
                    selectable
                        .then(|| state.runtime_candidates.get(&candidate_id).cloned())
                        .flatten()
                };
                let Some(binding) = binding else {
                    mark_runtime_failure(shared, &candidate_id, FlowFailure::RuntimeSelection);
                    continue;
                };
                let mut selected = profile_for_binding(base, &binding);
                if !shared.begin_binding_transition() {
                    return Err(FlowFailure::Stale);
                }
                shared.set_state(ConnectionState::AttachingRuntime);
                shared.set_profile(selected.clone());
                match run_selected_backend(shared, &mut selected, session, commands).await {
                    Ok(()) => return Ok(()),
                    Err(failure) if is_runtime_local_failure(failure) => {
                        if !should_return_to_runtime_picker(shared, failure) {
                            // The selected runtime has become the durable
                            // owner. Let the outer connection lifecycle
                            // retain its snapshot and stop/recover it; a
                            // picker retry here would destroy the handoff
                            // screen and native Term.
                            return Err(failure);
                        }
                        clear_runtime_binding(shared)?;
                        mark_runtime_failure(shared, &candidate_id, failure);
                    }
                    Err(failure) => return Err(failure),
                }
            }
            ControlCommand::CreateRuntime { backend, name } => {
                if backend != Backend::Tmux {
                    mark_runtime_section_failure(shared, backend, FlowFailure::RuntimeSelection);
                    continue;
                }
                shared.set_state(ConnectionState::CreatingRuntime);
                let identity = match control::create_runtime(shared, session, &name).await {
                    Ok(identity) => identity,
                    Err(failure) if is_runtime_local_failure(failure) => {
                        mark_runtime_section_failure(shared, Backend::Tmux, failure);
                        continue;
                    }
                    Err(failure) => return Err(failure),
                };
                let binding = RuntimeBinding::Tmux(identity);
                let mut selected = profile_for_binding(base, &binding);
                if !shared.begin_binding_transition() {
                    return Err(FlowFailure::Stale);
                }
                shared.set_state(ConnectionState::AttachingRuntime);
                shared.set_profile(selected.clone());
                match run_selected_backend(shared, &mut selected, session, commands).await {
                    Ok(()) => return Ok(()),
                    Err(failure) if is_runtime_local_failure(failure) => {
                        if !should_return_to_runtime_picker(shared, failure) {
                            return Err(failure);
                        }
                        clear_runtime_binding(shared)?;
                        mark_runtime_section_failure(shared, Backend::Tmux, failure);
                    }
                    Err(failure) => return Err(failure),
                }
            }
            // Pane/topology commands can be queued by a stale view while the
            // picker is visible. They have no valid runtime target yet.
            _ => {}
        }
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

fn invalid_runtime_name(value: &str) -> bool {
    value.chars().any(char::is_control)
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
    use crate::input::Modifiers;
    use crate::terminal::SemanticInput;
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const KEY_ONE: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_TWO: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    enum FixtureInputReceiver {
        Bytes(mpsc::Receiver<Vec<u8>>),
        Semantic(mpsc::Receiver<SemanticInput>),
    }

    struct BoundedControlFixture {
        owner: TerminalId,
        target: TerminalId,
        shared: Arc<ConnectionShared>,
        generation: u64,
        command_receiver: mpsc::Receiver<ControlRequest>,
        input_receiver: FixtureInputReceiver,
    }

    impl BoundedControlFixture {
        fn fill_command_queue(&self) {
            let epoch = self.shared.operation_epoch();
            for _ in 0..32 {
                self.shared
                    .command_sender()
                    .expect("bounded fixture command sender")
                    .try_send(ControlRequest {
                        epoch,
                        command: ControlCommand::RefreshTerminal,
                    })
                    .expect("test command queue capacity");
            }
        }
    }

    impl Drop for BoundedControlFixture {
        fn drop(&mut self) {
            if let Some(entry) = connections()
                .lock()
                .expect("bounded fixture connection registry")
                .remove(&self.owner)
            {
                entry.abort.abort();
            }
            registry::destroy_terminal(self.target);
            registry::destroy_terminal(self.owner);
        }
    }

    fn bounded_control_fixture(backend: Backend) -> BoundedControlFixture {
        let owner = registry::create_terminal(80, 24).expect("bounded owner terminal");
        let target = registry::create_terminal(80, 24).expect("bounded target terminal");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "bounded-control.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/bounded-control-known-hosts"),
        ));
        let old = PaneSnapshot {
            window_id: 1,
            pane_id: owner,
            terminal_id: owner,
            window_name: "bounded".to_owned(),
            active: true,
            selected: true,
            index: 0,
            columns: 80,
            rows: 24,
            pane_name: "old".to_owned(),
            title: "old".to_owned(),
        };
        let new = PaneSnapshot {
            window_id: 1,
            pane_id: target,
            terminal_id: target,
            window_name: "bounded".to_owned(),
            active: false,
            selected: false,
            index: 1,
            columns: 80,
            rows: 24,
            pane_name: "new".to_owned(),
            title: "new".to_owned(),
        };
        {
            let mut state = shared.session.lock().expect("bounded session state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "bounded-control.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/bounded-control-known-hosts"),
                backend,
                runtime: Some(
                    match backend {
                        Backend::Tmux => "meeterm",
                        Backend::Herdr => "default",
                    }
                    .to_owned(),
                ),
            });
            state.pane_terminals.insert(owner, owner);
            state.pane_terminals.insert(target, target);
            state.selected_pane = Some(owner);
            state.snapshot = SessionSnapshot {
                windows: vec![WindowSnapshot {
                    window_id: 1,
                    name: "bounded".to_owned(),
                    panes: vec![old.clone(), new.clone()],
                    selected: true,
                    zoomed: false,
                }],
                panes: vec![old, new],
                selected_pane: Some(owner),
            };
            state.runtime_operations_ready = true;
            state.terminal_input_ready = true;
        }

        let input_receiver = match backend {
            Backend::Tmux => {
                let (input, receiver) = mpsc::channel(8);
                let (resize, _sizes) = watch::channel((80, 24));
                registry::prepare_pane_transport(owner, generation, (80, 24), input, resize)
                    .expect("bounded tmux transport");
                assert!(registry::mark_transport_ready(owner, generation));
                FixtureInputReceiver::Bytes(receiver)
            }
            Backend::Herdr => {
                registry::begin_remote(owner, generation).expect("bounded Herdr remote terminal");
                let (input, receiver) = mpsc::channel(8);
                let (resize, _sizes) = watch::channel((80, 24));
                registry::with_terminal_for_test(owner, |terminal| {
                    terminal
                        .attach_semantic_transport(generation, input, resize)
                        .expect("bounded Herdr semantic transport");
                })
                .expect("bounded Herdr terminal");
                assert!(registry::mark_transport_ready(owner, generation));
                FixtureInputReceiver::Semantic(receiver)
            }
        };

        let (sender, command_receiver) = mpsc::channel(32);
        shared.set_commands(sender);
        let abort = runtime()
            .expect("bounded fixture native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections()
            .lock()
            .expect("bounded fixture connection registry")
            .insert(
                owner,
                ConnectionEntry {
                    shared: Arc::clone(&shared),
                    abort,
                },
            );

        BoundedControlFixture {
            owner,
            target,
            shared,
            generation,
            command_receiver,
            input_receiver,
        }
    }

    #[test]
    fn full_control_queue_rejects_selection_without_changing_backend_target() {
        for backend in [Backend::Tmux, Backend::Herdr] {
            let fixture = bounded_control_fixture(backend);
            fixture.fill_command_queue();
            let before_snapshot = session_snapshot(fixture.owner).expect("selection snapshot");
            let before_epoch = registry::operation_epoch(fixture.owner).expect("selection epoch");

            assert!(select_pane(fixture.owner, fixture.target).is_err());

            let state = fixture
                .shared
                .session
                .lock()
                .expect("selection rejection state");
            assert_eq!(state.selected_pane, Some(fixture.owner));
            assert_eq!(state.snapshot, before_snapshot);
            assert!(state.terminal_input_ready);
            drop(state);
            assert!(registry::transport_ready(fixture.owner, fixture.generation));
            assert!(!registry::transport_ready(
                fixture.target,
                fixture.generation
            ));
            assert_eq!(
                registry::operation_epoch(fixture.owner).expect("selection epoch after reject"),
                before_epoch
            );
        }
    }

    #[test]
    fn full_control_queue_rejects_hide_without_revoking_visibility_or_transport() {
        for backend in [Backend::Tmux, Backend::Herdr] {
            let fixture = bounded_control_fixture(backend);
            fixture.fill_command_queue();
            let before_snapshot = session_snapshot(fixture.owner).expect("visibility snapshot");
            let before_epoch = registry::operation_epoch(fixture.owner).expect("visibility epoch");

            assert!(set_terminal_visible(fixture.owner, false).is_err());

            let state = fixture
                .shared
                .session
                .lock()
                .expect("visibility rejection state");
            assert!(state.terminal_visible);
            assert!(state.terminal_input_ready);
            assert_eq!(state.snapshot, before_snapshot);
            drop(state);
            assert!(registry::transport_ready(fixture.owner, fixture.generation));
            assert_eq!(
                registry::operation_epoch(fixture.owner).expect("visibility epoch after reject"),
                before_epoch
            );
        }
    }

    #[test]
    fn accepted_hide_show_keeps_session_and_registry_gates_coherent_for_both_backends() {
        for backend in [Backend::Tmux, Backend::Herdr] {
            let mut fixture = bounded_control_fixture(backend);
            let before_epoch =
                registry::operation_epoch(fixture.owner).expect("initial input epoch");

            assert_eq!(set_terminal_visible(fixture.owner, false), Ok(()));
            {
                let state = fixture
                    .shared
                    .session
                    .lock()
                    .expect("hidden visibility state");
                assert!(!state.terminal_visible);
                assert!(!state.terminal_input_ready);
            }
            match backend {
                Backend::Tmux => {
                    assert!(registry::transport_ready(fixture.owner, fixture.generation));
                    assert_eq!(
                        registry::operation_epoch(fixture.owner).expect("tmux hidden epoch"),
                        before_epoch
                    );
                }
                Backend::Herdr => {
                    assert!(!registry::transport_ready(
                        fixture.owner,
                        fixture.generation
                    ));
                    assert!(
                        registry::operation_epoch(fixture.owner).expect("Herdr hidden epoch")
                            > before_epoch
                    );
                }
            }

            assert_eq!(set_terminal_visible(fixture.owner, true), Ok(()));
            {
                let state = fixture
                    .shared
                    .session
                    .lock()
                    .expect("shown visibility state");
                assert!(state.terminal_visible);
                assert_eq!(state.selected_pane, Some(fixture.owner));
                if backend == Backend::Tmux {
                    assert!(state.terminal_input_ready);
                } else {
                    // Herdr opens a new controller only after its first full
                    // frame; the public show edge must not fabricate Ready.
                    assert!(!state.terminal_input_ready);
                }
            }
            assert!(matches!(
                fixture
                    .command_receiver
                    .try_recv()
                    .expect("accepted hide command")
                    .command,
                ControlCommand::SetTerminalVisible { visible: false }
            ));
            assert!(matches!(
                fixture
                    .command_receiver
                    .try_recv()
                    .expect("accepted show command")
                    .command,
                ControlCommand::SetTerminalVisible { visible: true }
            ));

            if backend == Backend::Herdr {
                // The stopped receiver above models an actor between command
                // acceptance and its next wake. Rebind the selected semantic
                // transport exactly as the actor does after a fresh full
                // frame, then verify that only this fresh binding accepts
                // input.
                registry::begin_remote(fixture.owner, fixture.generation)
                    .expect("Herdr show remote binding");
                let (input, receiver) = mpsc::channel(8);
                let (resize, _sizes) = watch::channel((80, 24));
                registry::with_terminal_for_test(fixture.owner, |terminal| {
                    terminal
                        .attach_semantic_transport(fixture.generation, input, resize)
                        .expect("Herdr show semantic binding");
                })
                .expect("Herdr show terminal");
                assert!(registry::mark_transport_ready(
                    fixture.owner,
                    fixture.generation
                ));
                fixture.input_receiver = FixtureInputReceiver::Semantic(receiver);
                fixture.shared.refresh_terminal_input_ready();
            }

            let fresh_epoch = registry::operation_epoch(fixture.owner).expect("fresh input epoch");
            assert!(registry::transport_ready(fixture.owner, fixture.generation));
            assert!(
                fixture
                    .shared
                    .current_terminal_input_is_ready(fixture.shared.operation_epoch())
            );
            match &mut fixture.input_receiver {
                FixtureInputReceiver::Bytes(receiver) => {
                    assert_eq!(registry::send_bytes(fixture.owner, b"fresh").unwrap(), 5);
                    assert_eq!(receiver.try_recv().expect("fresh tmux input"), b"fresh");
                }
                FixtureInputReceiver::Semantic(receiver) => {
                    assert!(
                        registry::commit_utf8_at_epoch(fixture.owner, fresh_epoch, b"fresh")
                            .is_ok()
                    );
                    assert!(matches!(
                        receiver.try_recv().expect("fresh Herdr input"),
                        SemanticInput::Text(text, modifiers)
                            if text == "fresh" && modifiers == Modifiers::NONE
                    ));
                }
            }
        }
    }

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
        assert!(options(Backend::Tmux, Some("dev")).validate().is_ok());
        assert!(
            options(Backend::Tmux, Some("日本語 ; $HOME"))
                .validate()
                .is_ok()
        );
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
    fn runtime_picker_policy_distinguishes_fresh_and_retained_flows() {
        let options = || ConnectOptions {
            host: "example.test".into(),
            port: 22,
            username: "fixture".into(),
            credentials: AuthOptions::password("fixture-only".into()),
            known_hosts_path: PathBuf::from("/tmp/fixture-known-hosts"),
            backend: Backend::Tmux,
            runtime: None,
        };
        assert!(!ConnectionStart::Options(options()).enters_picker());
        assert!(ConnectionStart::Host(options()).enters_picker());

        let profile = ConnectionProfile {
            host: "example.test".into(),
            port: 22,
            username: "fixture".into(),
            known_hosts_path: PathBuf::from("/tmp/fixture-known-hosts"),
            credentials: StoredCredentials::Password {
                password: Arc::new(Zeroizing::new("fixture-only".into())),
            },
            backend: Backend::Tmux,
            runtime: Some("meeterm".into()),
            tmux_identity: None,
            herdr_executable: None,
        };
        let automatic = ConnectionStart::AutomaticReconnect(profile.clone());
        assert!(!automatic.enters_picker());
        assert!(automatic.is_automatic_reconnect());

        let mut herdr_profile = profile.clone();
        herdr_profile.backend = Backend::Herdr;
        herdr_profile.runtime = None;
        herdr_profile.herdr_executable = Some("/home/fixture/.local/bin/herdr".into());
        let herdr_automatic = ConnectionStart::AutomaticReconnect(herdr_profile);
        assert!(!herdr_automatic.enters_picker());
        assert!(herdr_automatic.is_automatic_reconnect());

        let manual = ConnectionStart::ManualReconnect(profile);
        assert!(manual.enters_picker());
        assert!(manual.is_manual_reconnect());
        assert!(!manual.is_automatic_reconnect());
    }

    #[test]
    fn manual_reconnect_clears_binding_metadata_before_authentication() {
        let owner = registry::create_terminal(80, 24).expect("owner terminal");
        let stale_pane = registry::create_terminal(80, 24).expect("stale pane terminal");
        let profile = ConnectionProfile {
            host: "example.test".into(),
            port: 22,
            username: "fixture".into(),
            known_hosts_path: PathBuf::from("/tmp/fixture-known-hosts"),
            credentials: StoredCredentials::Password {
                password: Arc::new(Zeroizing::new("fixture-only".into())),
            },
            backend: Backend::Tmux,
            runtime: Some("meeterm".into()),
            tmux_identity: None,
            herdr_executable: None,
        };
        {
            let state = session_state(owner);
            let mut state = state.lock().expect("session state");
            state.endpoint = Some(SessionEndpoint::from_profile(&profile));
            state.profile = Some(profile.clone());
            state.pane_terminals.insert(42, stale_pane);
            state.snapshot.selected_pane = Some(42);
            state.selected_pane = Some(42);
            state.runtime_candidates.insert(
                "stale".into(),
                RuntimeBinding::Tmux(tmux::SessionIdentity {
                    session_id: "$7".into(),
                    name: "meeterm".into(),
                    server_pid: 7,
                    server_start_time: 1700000000,
                }),
            );
        }

        let stale = prepare_manual_reconnect(owner, &profile).expect("prepare manual reconnect");
        assert_eq!(stale, vec![stale_pane]);
        {
            let state = session_state(owner);
            let state = state.lock().expect("session state");
            assert!(state.profile.is_none());
            assert!(state.runtime_candidates.is_empty());
            assert_eq!(state.runtime_discovery, RuntimeDiscoverySnapshot::default());
            assert!(state.pane_terminals.is_empty());
            assert_eq!(state.snapshot, SessionSnapshot::default());
            assert_eq!(state.selected_pane, None);
            assert_eq!(
                state.endpoint.as_ref().map(|e| e.backend),
                Some(Backend::Tmux)
            );
            assert_eq!(
                state.endpoint.as_ref().and_then(|e| e.runtime.as_deref()),
                None
            );
        }

        // The caller-owned start value still contains the credentials after
        // the selected runtime metadata has been removed from session state.
        assert!(matches!(
            profile.credentials,
            StoredCredentials::Password { .. }
        ));
        registry::destroy_terminal(stale_pane);
        registry::destroy_terminal(owner);
    }

    #[test]
    fn tmux_create_failure_preserves_candidates_and_herdr_discovery() {
        let owner = registry::create_terminal(80, 24).expect("owner terminal");
        let shared = ConnectionShared::new(
            owner,
            1,
            "example.test".to_owned(),
            22,
            PathBuf::from("/tmp/example-known-hosts"),
        );
        let tmux_candidate = RuntimeCandidate {
            id: "runtime-tmux".into(),
            backend: Backend::Tmux,
            name: "meeterm".into(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: true,
            ..RuntimeCandidate::default()
        };
        let herdr_candidate = RuntimeCandidate {
            id: "runtime-herdr".into(),
            backend: Backend::Herdr,
            name: "default".into(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: true,
            ..RuntimeCandidate::default()
        };
        {
            let mut state = shared.session.lock().expect("session state");
            state.generation = shared.generation;
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: shared.generation,
                discovery_revision: 4,
                tmux: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![tmux_candidate],
                    ..RuntimeSection::default()
                },
                herdr: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![herdr_candidate],
                    ..RuntimeSection::default()
                },
            };
        }
        let (tmux_before, herdr_before) = shared
            .session
            .lock()
            .map(|state| {
                (
                    state.runtime_discovery.tmux.clone(),
                    state.runtime_discovery.herdr.clone(),
                )
            })
            .expect("session state");

        mark_runtime_section_failure(&shared, Backend::Tmux, FlowFailure::TmuxRuntimeCollision);

        {
            let state = shared.session.lock().expect("session state");
            let tmux = &state.runtime_discovery.tmux;
            assert_eq!(tmux.state, RuntimeSectionState::Success);
            assert_eq!(tmux.candidates, tmux_before.candidates);
            assert_eq!(tmux.error_code.as_deref(), Some("tmux_runtime_collision"));
            assert_eq!(
                tmux.error_message.as_deref(),
                Some("A tmux session with that name already exists.")
            );
            assert_eq!(state.runtime_discovery.herdr, herdr_before);
        }

        // A discovery failure remains authoritative; a create result must
        // not replace it with an operation error either.
        let discovery_error = RuntimeSection {
            state: RuntimeSectionState::Error,
            error_code: Some("tmux_discovery_timeout".into()),
            error_message: Some("discovery failed".into()),
            ..RuntimeSection::default()
        };
        {
            let mut state = shared.session.lock().expect("session state");
            state.runtime_discovery.tmux = discovery_error.clone();
        }
        clear_tmux_create_error(&shared).expect("clear operation error");
        mark_runtime_section_failure(&shared, Backend::Tmux, FlowFailure::TmuxRuntimeUnknown);
        assert_eq!(
            shared
                .session
                .lock()
                .expect("session state")
                .runtime_discovery
                .tmux,
            discovery_error
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn accepted_tmux_create_clears_and_repeats_same_operation_error() {
        let owner = registry::create_terminal(80, 24).expect("owner terminal");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "example.test".to_owned(),
            22,
            PathBuf::from("/tmp/example-known-hosts"),
        ));
        let candidate = RuntimeCandidate {
            id: "runtime-tmux".into(),
            backend: Backend::Tmux,
            name: "meeterm".into(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: true,
            ..RuntimeCandidate::default()
        };
        {
            let mut state = shared.session.lock().expect("session state");
            state.generation = generation;
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: generation,
                discovery_revision: 8,
                tmux: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![candidate],
                    error_code: Some("tmux_runtime_collision".into()),
                    error_message: Some("A tmux session with that name already exists.".into()),
                },
                ..RuntimeDiscoverySnapshot::default()
            };
        }
        let (sender, mut receiver) = mpsc::channel(4);
        shared.set_commands(sender);
        let test_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let abort = test_runtime
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort,
            },
        );

        // A request outside the picker is not accepted and must not clear
        // the previous operation result or enqueue a command.
        assert_eq!(
            create_runtime(owner, Backend::Tmux, "desk"),
            Err(ConnectionError::RuntimeSelectionUnavailable)
        );
        assert_eq!(
            shared
                .session
                .lock()
                .expect("session state")
                .runtime_discovery
                .tmux
                .error_code
                .as_deref(),
            Some("tmux_runtime_collision")
        );
        assert!(receiver.try_recv().is_err());

        shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        assert_eq!(create_runtime(owner, Backend::Tmux, "desk"), Ok(()));
        assert_eq!(
            shared
                .session
                .lock()
                .expect("session state")
                .runtime_discovery
                .tmux
                .error_code,
            None
        );
        assert!(matches!(
            receiver.try_recv().expect("first create command").command,
            ControlCommand::CreateRuntime { .. }
        ));

        mark_runtime_section_failure(&shared, Backend::Tmux, FlowFailure::TmuxRuntimeCollision);
        assert_eq!(
            shared
                .session
                .lock()
                .expect("session state")
                .runtime_discovery
                .tmux
                .error_code
                .as_deref(),
            Some("tmux_runtime_collision")
        );

        // The second accepted request gets the same synchronous clear, so a
        // same-code failure produces a new clear -> error transition instead
        // of leaving mobile with an unchanged snapshot.
        assert_eq!(create_runtime(owner, Backend::Tmux, "desk"), Ok(()));
        assert_eq!(
            shared
                .session
                .lock()
                .expect("session state")
                .runtime_discovery
                .tmux
                .error_code,
            None
        );
        assert!(matches!(
            receiver.try_recv().expect("second create command").command,
            ControlCommand::CreateRuntime { .. }
        ));
        mark_runtime_section_failure(&shared, Backend::Tmux, FlowFailure::TmuxRuntimeCollision);
        assert_eq!(
            shared
                .session
                .lock()
                .expect("session state")
                .runtime_discovery
                .tmux
                .error_code
                .as_deref(),
            Some("tmux_runtime_collision")
        );

        registry::destroy_terminal(owner);
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
    fn explicit_shutdown_revokes_gates_before_hard_cancellation() {
        let owner = registry::create_terminal(80, 24).expect("explicit shutdown terminal");
        let shared = Arc::new(ConnectionShared::new(
            owner,
            next_generation(),
            "explicit-shutdown.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/explicit-shutdown-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("explicit shutdown session")
            .generation = shared.generation;
        let old_epoch = shared.operation_epoch();

        // The accepted boundary revokes normal operations and wakes the
        // actor, but leaves the SSH stream usable for the final cleanup.
        shared.invalidate_explicitly("explicit_disconnect");
        assert!(shared.explicit_cleanup_requested());
        assert!(!shared.is_cancelled());
        assert!(!shared.current_request_epoch(old_epoch));
        assert_eq!(
            shared.snapshot().expect("explicit shutdown snapshot").state,
            ConnectionState::Closing as u32
        );

        // The actor may publish completion before the caller needs to use the
        // hard fallback. Explicit completion is terminal even without cancel.
        shared.finish(Err(FlowFailure::Stale));
        assert!(!shared.is_cancelled());
        assert_eq!(
            shared.snapshot().expect("finished shutdown snapshot").state,
            ConnectionState::Disconnected as u32
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn explicit_shutdown_keeps_established_stream_writable_until_fallback() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind cleanup peer");
            let address = listener.local_addr().expect("cleanup peer address");
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept cleanup client");
                let mut bytes = [0_u8; 7];
                socket
                    .read_exact(&mut bytes)
                    .await
                    .expect("read cleanup bytes");
                assert_eq!(&bytes, b"cleanup");
            });

            let owner = registry::create_terminal(80, 24).expect("stream cleanup terminal");
            let generation = next_generation();
            let shared = Arc::new(ConnectionShared::new(
                owner,
                generation,
                "stream-cleanup.example.test".to_owned(),
                address.port(),
                PathBuf::from("/tmp/stream-cleanup-known-hosts"),
            ));
            shared
                .session
                .lock()
                .expect("stream cleanup session")
                .generation = generation;
            let socket = tokio::net::TcpStream::connect(address)
                .await
                .expect("connect cleanup peer");
            let control = Arc::new(ConnectIoControl::new(
                Instant::now() + Duration::from_secs(10),
            ));
            let mut stream = CancellableStream::new(socket, Arc::clone(&shared), control);
            shared.invalidate_explicitly("explicit_disconnect");
            stream
                .write_all(b"cleanup")
                .await
                .expect("explicit shutdown rejected established write");
            server.await.expect("cleanup peer task");
            registry::destroy_terminal(owner);
        });
    }

    #[test]
    fn explicit_shutdown_forces_cancellation_after_bounded_wait() {
        let owner = registry::create_terminal(80, 24).expect("forced shutdown terminal");
        let shared = Arc::new(ConnectionShared::new(
            owner,
            next_generation(),
            "forced-shutdown.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/forced-shutdown-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("forced shutdown session")
            .generation = shared.generation;
        shared.invalidate_explicitly("explicit_disconnect");
        let runtime = runtime().expect("native runtime");
        let abort = runtime.spawn(std::future::pending::<()>()).abort_handle();

        // Zero is a deterministic test bound for the same helper used by the
        // production three-second fallback. A non-finishing actor is hard
        // cancelled and aborted rather than leaving Disconnect blocked.
        assert!(
            !finish_or_force_explicit_shutdown_with_timeout(
                runtime,
                Arc::clone(&shared),
                abort,
                Duration::ZERO,
            )
            .finished
        );
        assert!(shared.is_cancelled());
        registry::destroy_terminal(owner);
    }

    #[test]
    fn unconfirmed_zoom_cleanup_is_visible_after_forced_disconnect() {
        let owner = registry::create_terminal(80, 24).expect("unconfirmed cleanup terminal");
        let shared = Arc::new(ConnectionShared::new(
            owner,
            next_generation(),
            "unconfirmed-cleanup.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/unconfirmed-cleanup-known-hosts"),
        ));
        {
            let mut state = shared.session.lock().expect("unconfirmed cleanup session");
            state.generation = shared.generation;
            state.meeterm_zoomed = true;
            state.meeterm_zoomed_window = Some(1);
            state.meeterm_zoomed_pane = Some(17);
        }
        shared.invalidate_explicitly("explicit_disconnect");
        let runtime = runtime().expect("native runtime");
        let abort = runtime.spawn(std::future::pending::<()>()).abort_handle();

        let result = finish_or_force_explicit_shutdown_with_timeout(
            runtime,
            Arc::clone(&shared),
            abort,
            Duration::ZERO,
        );
        assert!(!result.finished);
        assert_eq!(result.cleanup, ZoomCleanupOutcome::UnconfirmedOrFailed);
        let snapshot = shared.snapshot().expect("unconfirmed cleanup snapshot");
        assert_eq!(snapshot.state, ConnectionState::Disconnected as u32);
        assert_eq!(
            std::str::from_utf8(&snapshot.error_code[..usize::from(snapshot.error_code_len)])
                .expect("cleanup error code UTF-8"),
            "layout_restore_unconfirmed"
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn cleanup_outcome_is_sticky_across_late_success_reports() {
        let owner = registry::create_terminal(80, 24).expect("cleanup outcome terminal");
        let shared = ConnectionShared::new(
            owner,
            next_generation(),
            "cleanup-outcome.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/cleanup-outcome-known-hosts"),
        );
        shared.record_zoom_cleanup(ZoomCleanupOutcome::RestoredConfirmed);
        assert_eq!(
            shared.zoom_cleanup_outcome(),
            ZoomCleanupOutcome::RestoredConfirmed
        );
        shared.record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
        assert_eq!(
            shared.zoom_cleanup_outcome(),
            ZoomCleanupOutcome::UnconfirmedOrFailed
        );
        shared.record_zoom_cleanup(ZoomCleanupOutcome::RestoredConfirmed);
        assert_eq!(
            shared.zoom_cleanup_outcome(),
            ZoomCleanupOutcome::UnconfirmedOrFailed
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn transport_recovery_does_not_request_explicit_cleanup() {
        let owner = registry::create_terminal(80, 24).expect("recovery shutdown terminal");
        let shared = Arc::new(ConnectionShared::new(
            owner,
            next_generation(),
            "transport-recovery.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/transport-recovery-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("transport recovery session")
            .generation = shared.generation;

        shared
            .begin_recovery("transport", 1)
            .expect("transport recovery should begin");
        assert!(!shared.explicit_cleanup_requested());
        assert!(!shared.is_cancelled());
        shared.finish(Err(FlowFailure::Transport));
        assert!(!shared.explicit_cleanup_requested());
        assert!(!shared.is_cancelled());
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Stopped);
        registry::destroy_terminal(owner);
    }

    fn recovery_fixture() -> (TerminalId, Arc<ConnectionShared>) {
        let owner = registry::create_terminal(80, 24).expect("recovery owner terminal");
        let shared = Arc::new(ConnectionShared::new(
            owner,
            next_generation(),
            "recovery.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/recovery-known-hosts"),
        ));
        {
            let mut state = shared.session.lock().expect("recovery session state");
            state.generation = shared.generation;
            state.endpoint = Some(SessionEndpoint {
                host: "recovery.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/recovery-known-hosts"),
                backend: Backend::Tmux,
                runtime: Some("meeterm".to_owned()),
            });
            state.pane_terminals.insert(owner, owner);
            state.selected_pane = Some(owner);
            state.snapshot = SessionSnapshot {
                windows: vec![WindowSnapshot {
                    window_id: 1,
                    name: "retained".to_owned(),
                    panes: vec![PaneSnapshot {
                        window_id: 1,
                        pane_id: owner,
                        terminal_id: owner,
                        window_name: "retained".to_owned(),
                        active: true,
                        selected: true,
                        index: 0,
                        columns: 80,
                        rows: 24,
                        pane_name: "shell".to_owned(),
                        title: "shell".to_owned(),
                    }],
                    selected: true,
                    zoomed: false,
                }],
                panes: vec![PaneSnapshot {
                    window_id: 1,
                    pane_id: owner,
                    terminal_id: owner,
                    window_name: "retained".to_owned(),
                    active: true,
                    selected: true,
                    index: 0,
                    columns: 80,
                    rows: 24,
                    pane_name: "shell".to_owned(),
                    title: "shell".to_owned(),
                }],
                selected_pane: Some(owner),
            };
        }
        assert!(shared.mark_ready());
        (owner, shared)
    }

    #[test]
    fn post_ready_runtime_failure_keeps_retained_state_out_of_picker_reset() {
        let (owner, shared) = recovery_fixture();
        let retained_snapshot = session_snapshot(owner).expect("retained runtime snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let before_epoch = shared.operation_epoch();

        assert!(!should_return_to_runtime_picker(
            &shared,
            FlowFailure::HerdrProtocol
        ));
        assert!(matches!(
            clear_runtime_binding(&shared),
            Err(FlowFailure::Stale)
        ));

        let recovery_epoch = shared
            .begin_recovery("herdr_session_missing", 1)
            .expect("retained runtime should enter recovery");
        shared.stop_recovery("herdr_session_missing");
        assert!(recovery_epoch > before_epoch);
        assert_eq!(
            session_snapshot(owner).expect("retained snapshot after failure"),
            retained_snapshot
        );
        assert!(Arc::ptr_eq(
            &retained_terminal,
            &registry::shared_terminal(owner).expect("same native terminal after failure")
        ));
        let session = session_state(owner);
        let state = session.lock().expect("retained state");
        assert_eq!(state.selected_pane, Some(owner));
        assert_eq!(state.pane_terminals.get(&owner), Some(&owner));
        assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
        drop(state);
        registry::destroy_terminal(owner);

        let fresh_owner = registry::create_terminal(80, 24).expect("fresh owner terminal");
        let fresh_shared = ConnectionShared::new(
            fresh_owner,
            next_generation(),
            "fresh.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/fresh-known-hosts"),
        );
        assert!(should_return_to_runtime_picker(
            &fresh_shared,
            FlowFailure::HerdrProtocol
        ));
        registry::destroy_terminal(fresh_owner);
    }

    #[test]
    fn final_capture_barrier_rejects_ready_from_an_older_session_epoch() {
        let (owner, shared) = recovery_fixture();
        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let expected_epoch = shared.operation_epoch();
        let captured = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let old_shared = Arc::clone(&shared);
        let old_captured = Arc::clone(&captured);
        let old_release = Arc::clone(&release);
        let old_ready = std::thread::spawn(move || {
            // This represents the final capture response being available but
            // not yet allowed to publish the session Ready state.
            old_captured.wait();
            old_release.wait();
            old_shared.mark_ready_at_epoch(expected_epoch)
        });
        captured.wait();
        let recovery_epoch = shared
            .begin_recovery("transport", 1)
            .expect("new recovery epoch");
        release.wait();
        assert!(!old_ready.join().expect("old capture thread"));
        assert!(recovery_epoch > expected_epoch);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert_eq!(
            session_snapshot(owner).expect("last committed snapshot"),
            retained_snapshot
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn cancelled_owner_handoff_cannot_install_after_disconnect_barrier() {
        let owner = Arc::new(OwnerTransition::new());
        let reached_handoff = Arc::new(std::sync::Barrier::new(2));
        let release_handoff = Arc::new(std::sync::Barrier::new(2));
        let worker_owner = Arc::clone(&owner);
        let worker_reached = Arc::clone(&reached_handoff);
        let worker_release = Arc::clone(&release_handoff);
        let worker = std::thread::spawn(move || {
            let (_serial, ticket) = worker_owner.begin().expect("handoff ticket");
            worker_reached.wait();
            worker_release.wait();
            assert!(!worker_owner.install_allowed(ticket));
        });

        reached_handoff.wait();
        // Disconnect/change owns the commit boundary while the old map entry
        // is absent. Cancelling the ticket here is the map-remove barrier.
        owner.cancel_current();
        release_handoff.wait();
        worker.join().expect("handoff worker");

        let (_serial, replacement_ticket) = owner.begin().expect("next owner ticket");
        assert!(owner.install_allowed(replacement_ticket));
        owner.finish(replacement_ticket);
    }

    #[test]
    fn disconnect_fence_rejects_start_that_entered_before_publication() {
        let owner = Arc::new(OwnerTransition::new());
        // Hold the commit boundary while the start has already captured its
        // request fence. The release ordering below lets Disconnect acquire
        // and complete its cancellation before publication is allowed to run.
        let commit_guard = owner.commit.lock().expect("owner commit barrier");
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_owner = Arc::clone(&owner);
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            let (_serial, ticket) = worker_owner
                .begin_with_barriers(&worker_entered, &worker_release)
                .expect("fenced start");
            worker_owner.install_allowed(ticket)
        });

        entered.wait();
        drop(commit_guard);
        // The worker remains behind `release`, so this call deterministically
        // wins the commit mutex before the old start can publish its ticket.
        owner.cancel_current();
        release.wait();
        assert!(!worker.join().expect("fenced start worker"));

        let (_serial, replacement_ticket) = owner.begin().expect("replacement start");
        assert!(owner.install_allowed(replacement_ticket));
        owner.finish(replacement_ticket);
    }

    #[test]
    fn cancelled_preinstall_actor_cannot_acquire_remote_before_start_gate() {
        let owner = registry::create_terminal(80, 24).expect("preinstall owner terminal");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "preinstall.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/preinstall-known-hosts"),
        ));
        shared
            .session
            .lock()
            .expect("preinstall session state")
            .generation = generation;
        let (gate_sender, gate_receiver) = oneshot::channel();
        let remote_started = Arc::new(AtomicBool::new(false));
        let worker_shared = Arc::clone(&shared);
        let worker_started = Arc::clone(&remote_started);
        let worker = runtime().expect("native runtime").spawn(async move {
            if wait_for_start_gate(&worker_shared, gate_receiver).await {
                // This represents the first remote acquisition operation
                // after actor installation (channel/discovery/controller).
                worker_started.store(true, Ordering::Release);
            }
        });

        // Disconnect is accepted before map install/gate release. Dropping
        // the sender models the canceled pre-install start path; the actor
        // must observe cancellation and never reach the side-effect marker.
        shared.cancel();
        drop(gate_sender);
        runtime()
            .expect("native runtime")
            .block_on(async { worker.await.expect("preinstall actor") });
        assert!(!remote_started.load(Ordering::Acquire));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn hidden_herdr_selection_publishes_metadata_before_first_frame() {
        let owner = registry::create_terminal(80, 24).expect("hidden Herdr owner terminal");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "hidden-herdr.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/hidden-herdr-known-hosts"),
        ));
        {
            let mut state = shared.session.lock().expect("hidden Herdr state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "hidden-herdr.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/hidden-herdr-known-hosts"),
                backend: Backend::Herdr,
                runtime: None,
            });
            state.terminal_visible = false;
            state.pane_terminals.insert(owner, owner);
            state.selected_pane = Some(owner);
            state.snapshot = SessionSnapshot {
                windows: vec![WindowSnapshot {
                    window_id: 10,
                    name: "workspace".to_owned(),
                    panes: vec![PaneSnapshot {
                        window_id: 10,
                        pane_id: owner,
                        terminal_id: owner,
                        window_name: "workspace".to_owned(),
                        active: true,
                        selected: true,
                        index: 0,
                        columns: 80,
                        rows: 24,
                        pane_name: "shell".to_owned(),
                        title: "shell".to_owned(),
                    }],
                    selected: true,
                    zoomed: false,
                }],
                panes: vec![PaneSnapshot {
                    window_id: 10,
                    pane_id: owner,
                    terminal_id: owner,
                    window_name: "workspace".to_owned(),
                    active: true,
                    selected: true,
                    index: 0,
                    columns: 80,
                    rows: 24,
                    pane_name: "shell".to_owned(),
                    title: "shell".to_owned(),
                }],
                selected_pane: Some(owner),
            };
        }
        assert!(shared.mark_ready_at_epoch(shared.operation_epoch()));
        {
            let state = shared.session.lock().expect("metadata-ready state");
            assert!(state.runtime_operations_ready);
            assert!(!state.terminal_input_ready);
            assert_eq!(state.selected_pane, Some(owner));
        }

        let (sender, mut receiver) = mpsc::channel(2);
        shared.set_commands(sender);
        let abort = runtime()
            .expect("native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort,
            },
        );
        // This is the public call order after the picker has been published:
        // make the native view visible, then let the Herdr actor acquire a
        // controller and open input only after its first full frame.
        assert_eq!(set_terminal_visible(owner, true), Ok(()));
        assert!(matches!(
            receiver.try_recv().expect("visible command").command,
            ControlCommand::SetTerminalVisible { visible: true }
        ));
        let state = shared.session.lock().expect("visible but pre-frame state");
        assert!(state.runtime_operations_ready);
        assert!(!state.terminal_input_ready);
        drop(state);

        connections()
            .lock()
            .expect("connection registry")
            .remove(&owner)
            .expect("hidden test connection")
            .abort
            .abort();
        registry::destroy_terminal(owner);
    }

    #[test]
    fn awaiting_confirmation_foreground_loss_revokes_old_token_and_wakes_recovery() {
        let (owner, shared) = recovery_fixture();
        let recovery_epoch = shared
            .begin_recovery("transport", 1)
            .expect("recovery epoch");
        assert!(
            shared
                .publish_recovery_confirmation("old-token".to_owned())
                .is_ok()
        );
        let (sender, _receiver) = mpsc::channel(2);
        shared.set_commands(sender);
        let abort = runtime()
            .expect("native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort,
            },
        );

        assert_eq!(set_foreground(owner, false), Ok(()));
        assert!(shared.operation_epoch() > recovery_epoch);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert_eq!(
            confirm_recovery(owner, "old-token"),
            Err(ConnectionError::RecoveryUnavailable)
        );
        assert_eq!(set_foreground(owner, true), Ok(()));
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        {
            let state = shared.session.lock().expect("revoked confirmation state");
            assert!(state.recovery.confirmation_token.is_empty());
            assert!(state.pending_confirmation_token.is_none());
        }

        connections()
            .lock()
            .expect("connection registry")
            .remove(&owner)
            .expect("confirmation test connection")
            .abort
            .abort();
        registry::destroy_terminal(owner);
    }

    fn retry_profile() -> ConnectionProfile {
        ConnectionProfile {
            // Port 1 keeps the replacement actor away from any fixture
            // service. The test observes the synchronous handoff before the
            // asynchronous SSH attempt can matter.
            host: "127.0.0.1".to_owned(),
            port: 1,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/recovery-retry-known-hosts"),
            credentials: StoredCredentials::Password {
                password: Arc::new(Zeroizing::new("fixture-only".to_owned())),
            },
            backend: Backend::Tmux,
            runtime: Some("meeterm".to_owned()),
            tmux_identity: None,
            herdr_executable: None,
        }
    }

    #[test]
    fn stopped_retry_restarts_finished_actor_once_and_retains_native_term() {
        let (owner, shared) = recovery_fixture();
        shared.set_profile(retry_profile());

        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let old_generation = shared.generation;
        shared
            .begin_recovery("transport", 1)
            .expect("initial recovery");
        shared.finish(Err(FlowFailure::Transport));
        assert!(shared.info.lock().expect("old connection info").finished);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Stopped);
        let stopped_epoch = shared.operation_epoch();

        // `wait_for_generation_finish` trusts the finished marker. Abort the
        // fixture task up front so the test does not leave a pending task
        // behind when the old entry is replaced without a drain wait.
        let old_abort = runtime()
            .expect("native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        old_abort.abort();
        connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort: old_abort,
            },
        );

        retry_recovery(owner, stopped_epoch).expect("first retry starts replacement");

        let replacement = connections()
            .lock()
            .expect("connection registry")
            .get(&owner)
            .expect("replacement actor")
            .shared
            .clone();
        assert_ne!(replacement.generation, old_generation);
        assert!(!Arc::ptr_eq(&replacement, &shared));
        let (new_generation, new_epoch) = {
            let state = session_state(owner);
            let state = state.lock().expect("replacement session state");
            assert_eq!(state.generation, replacement.generation);
            assert_eq!(state.recovery.phase, RecoveryPhase::Reconnecting);
            assert_eq!(state.recovery.reason, "manual_retry");
            assert!(state.operation_epoch != stopped_epoch);
            assert!(state.has_retained_work());
            assert_eq!(state.snapshot, retained_snapshot);
            assert_eq!(state.pane_terminals.get(&owner), Some(&owner));
            (state.generation, state.operation_epoch)
        };
        assert_eq!(new_generation, replacement.generation);
        assert!(new_epoch != stopped_epoch);
        assert!(Arc::ptr_eq(
            &retained_terminal,
            &registry::shared_terminal(owner).expect("replacement terminal")
        ));

        // Race duplicate callers with the pre-handoff epoch. They must be
        // rejected before starting anything, and the first replacement must
        // remain uncancelled. Checking only the finally visible map entry
        // would miss a buggy second start that cancelled this actor first.
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let first_barrier = Arc::clone(&barrier);
            let second_barrier = Arc::clone(&barrier);
            let first = scope.spawn(move || {
                first_barrier.wait();
                retry_recovery(owner, stopped_epoch)
            });
            let second = scope.spawn(move || {
                second_barrier.wait();
                retry_recovery(owner, stopped_epoch)
            });
            [
                first.join().expect("first duplicate retry caller"),
                second.join().expect("second duplicate retry caller"),
            ]
        });
        assert_eq!(
            results,
            [
                Err(ConnectionError::RecoveryStale),
                Err(ConnectionError::RecoveryStale)
            ]
        );
        assert!(
            !replacement.is_cancelled(),
            "a duplicate Retry must not cancel the first replacement actor"
        );
        assert_eq!(
            connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .expect("single replacement actor")
                .shared
                .generation,
            new_generation
        );

        registry::destroy_terminal(owner);
    }

    #[test]
    fn retry_recovery_reports_unavailable_when_command_sender_is_gone() {
        let (owner, shared) = recovery_fixture();
        shared.set_profile(retry_profile());
        let epoch = shared
            .begin_recovery("transport", 1)
            .expect("active recovery");
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);

        let abort = runtime()
            .expect("native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort,
            },
        );
        assert_eq!(
            retry_recovery(owner, epoch),
            Err(ConnectionError::RecoveryUnavailable)
        );
        assert!(Arc::ptr_eq(
            &shared,
            &connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .expect("unchanged active actor")
                .shared
        ));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn changed_host_key_stops_retained_recovery_without_retry() {
        let (owner, shared) = recovery_fixture();
        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let before_epoch = shared.operation_epoch();

        // This is the callback state committed before russh reports the
        // refusal. The presented and known fingerprints must remain visible
        // while the retained workspace is stopped for user review.
        shared.set_changed_key(
            "SHA256/presented".to_owned(),
            "ssh-ed25519".to_owned(),
            "SHA256/known".to_owned(),
        );
        let connection = shared.snapshot().expect("changed-key snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.fingerprint[..usize::from(connection.fingerprint_len)],
            b"SHA256/presented"
        );
        assert_eq!(
            &connection.known_fingerprint[..usize::from(connection.known_fingerprint_len)],
            b"SHA256/known"
        );
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"host_key_changed"
        );

        // Classify before begin_recovery can clear the Failed state. A
        // generic Network result from russh must not enter the retry wait.
        let disposition = retry_disposition(&shared, FlowFailure::Network);
        assert_eq!(disposition, RetryDisposition::Stop("host_key_changed"));
        assert!(!automatic_retry_allowed(&shared, FlowFailure::Network));
        assert_eq!(shared.operation_epoch(), before_epoch);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::None);
        assert!(
            shared.begin_recovery("network", 1).is_none(),
            "a changed host key must reject every automatic recovery entry"
        );
        assert!(
            connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .is_none()
        );

        if let RetryDisposition::Stop(reason) = disposition {
            shared.stop_recovery(reason);
        } else {
            panic!("changed host key must not be retried");
        }

        let connection = shared.snapshot().expect("stopped changed-key snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"host_key_changed"
        );
        assert_eq!(
            &connection.fingerprint[..usize::from(connection.fingerprint_len)],
            b"SHA256/presented"
        );
        assert_eq!(
            &connection.known_fingerprint[..usize::from(connection.known_fingerprint_len)],
            b"SHA256/known"
        );
        {
            let state = session_state(owner);
            let state = state.lock().expect("changed-key recovery state");
            assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
            assert_eq!(state.recovery.reason, "host_key_changed");
            assert!(!state.runtime_operations_ready);
            assert!(!state.terminal_input_ready);
            assert_eq!(state.snapshot, retained_snapshot);
            assert_eq!(state.selected_pane, Some(owner));
            assert!(state.has_retained_work());
            assert!(state.runtime_discovery == RuntimeDiscoverySnapshot::default());
        }
        assert!(Arc::ptr_eq(
            &retained_terminal,
            &registry::shared_terminal(owner).expect("retained terminal after key failure")
        ));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn authentication_failure_stops_retained_recovery_without_retry() {
        let (owner, shared) = recovery_fixture();
        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let before_epoch = shared.operation_epoch();
        shared.set_host_key("SHA256/authenticated".to_owned(), "ssh-ed25519".to_owned());

        // A failed credential exchange is terminal. Keep the established
        // host fingerprint and publish the existing bounded auth error before
        // simulating a generic transport result from the SSH layer.
        shared.fail("auth_failed", "SSH authentication failed.");
        let connection = shared.snapshot().expect("authentication snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"auth_failed"
        );
        assert_eq!(
            &connection.fingerprint[..usize::from(connection.fingerprint_len)],
            b"SHA256/authenticated"
        );

        let disposition = retry_disposition(&shared, FlowFailure::Network);
        assert_eq!(disposition, RetryDisposition::Stop("authentication_failed"));
        assert!(!automatic_retry_allowed(&shared, FlowFailure::Network));
        assert_eq!(shared.operation_epoch(), before_epoch);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::None);
        assert!(
            shared.begin_recovery("network", 1).is_none(),
            "an authentication failure must reject every automatic recovery entry"
        );
        assert!(
            connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .is_none()
        );

        if let RetryDisposition::Stop(reason) = disposition {
            shared.stop_recovery(reason);
        } else {
            panic!("authentication failure must not be retried");
        }

        let connection = shared.snapshot().expect("stopped authentication snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        // `stop_recovery` preserves the established public error spelling and
        // message for the connection-details UI, while the recovery control
        // uses the canonical authentication_failed reason below.
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"auth_failed"
        );
        assert_eq!(
            &connection.fingerprint[..usize::from(connection.fingerprint_len)],
            b"SHA256/authenticated"
        );
        {
            let state = session_state(owner);
            let state = state.lock().expect("authentication recovery state");
            assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
            assert_eq!(state.recovery.reason, "authentication_failed");
            assert!(!state.runtime_operations_ready);
            assert!(!state.terminal_input_ready);
            assert_eq!(state.snapshot, retained_snapshot);
            assert_eq!(state.selected_pane, Some(owner));
            assert!(state.has_retained_work());
            assert!(state.runtime_discovery == RuntimeDiscoverySnapshot::default());
        }
        assert!(Arc::ptr_eq(
            &retained_terminal,
            &registry::shared_terminal(owner).expect("retained terminal after auth failure")
        ));
        registry::destroy_terminal(owner);
    }

    #[test]
    fn rapid_foreground_transition_keeps_a_live_controller_authoritative() {
        let (owner, shared) = recovery_fixture();
        let (input_sender, mut input_receiver) = mpsc::channel(8);
        let (resize_sender, _resize_receiver) = watch::channel((80, 24));
        registry::prepare_pane_transport(
            owner,
            shared.generation,
            (80, 24),
            input_sender,
            resize_sender,
        )
        .expect("live tmux fixture transport");
        assert!(registry::mark_transport_ready(owner, shared.generation));
        shared.refresh_terminal_input_ready();
        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let retained_term = registry::with_terminal_for_test(owner, |terminal| {
            terminal.term() as *const _ as usize
        })
        .expect("retained native term");
        let retained_native_snapshot = registry::snapshot(owner).expect("retained native snapshot");
        let initial_epoch = shared.operation_epoch();
        let initial_terminal_epoch =
            registry::operation_epoch(owner).expect("initial terminal epoch");
        let (sender, _receiver) = mpsc::channel(4);
        shared.set_commands(sender);
        let abort = runtime()
            .expect("native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort,
            },
        );

        // A foreground transition is not itself transport loss. Keep the
        // established controller and retained topology authoritative while
        // still revoking input during the hidden interval.
        assert_eq!(set_foreground(owner, false), Ok(()));
        assert_eq!(shared.operation_epoch(), initial_epoch);
        let suspended_terminal_epoch =
            registry::operation_epoch(owner).expect("suspended terminal epoch");
        assert!(suspended_terminal_epoch > initial_terminal_epoch);
        assert!(!registry::transport_ready(owner, shared.generation));
        assert_eq!(shared.recovery_phase(), RecoveryPhase::None);
        assert!(!shared.current_terminal_input_is_ready(initial_epoch));
        assert_eq!(
            registry::send_bytes(owner, b"hidden-input"),
            Err(crate::terminal::TerminalError::InputNotReady)
        );
        assert!(input_receiver.try_recv().is_err());
        assert_eq!(
            registry::snapshot(owner).expect("hidden retained terminal"),
            retained_native_snapshot
        );
        assert_eq!(set_foreground(owner, true), Ok(()));
        assert!(shared.is_foreground());
        assert_eq!(shared.operation_epoch(), initial_epoch);
        let foreground_terminal_epoch =
            registry::operation_epoch(owner).expect("foreground terminal epoch");
        assert!(foreground_terminal_epoch > suspended_terminal_epoch);
        assert!(registry::transport_ready(owner, shared.generation));
        assert!(shared.current_terminal_input_is_ready(initial_epoch));
        registry::send_bytes(owner, b"foreground-input").expect("fresh foreground input");
        assert_eq!(
            input_receiver.try_recv().expect("foreground input message"),
            b"foreground-input"
        );

        // A live controller remains usable after a rapid false -> true
        // transition. A real transport loss still enters recovery through the
        // backend's loss boundary, which is tested separately below.
        assert!(!shared.recovery_requires_controller_exit(Some(initial_epoch)));
        assert!(!shared.recovery_requires_controller_exit(None));
        assert_eq!(set_terminal_visible(owner, true), Ok(()));
        {
            let state = session_state(owner);
            let state = state.lock().expect("foreground recovery state");
            assert_eq!(state.recovery.phase, RecoveryPhase::None);
            assert!(state.terminal_input_ready);
            assert_eq!(state.snapshot, retained_snapshot);
            assert!(state.has_retained_work());
        }
        assert!(Arc::ptr_eq(
            &retained_terminal,
            &registry::shared_terminal(owner).expect("retained terminal after recovery")
        ));
        assert_eq!(
            registry::with_terminal_for_test(owner, |terminal| terminal.term() as *const _
                as usize)
            .expect("native term after foreground"),
            retained_term,
            "foreground transitions retain the native Term"
        );

        registry::destroy_terminal(owner);
    }

    #[test]
    fn recovery_epoch_revokes_commands_but_retains_native_topology() {
        let (owner, shared) = recovery_fixture();
        let before = shared.operation_epoch();
        let recovery_epoch = shared
            .begin_recovery("transport", 1)
            .expect("recovery should start");
        assert!(recovery_epoch > before);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert!(!shared.current_request_is_ready(recovery_epoch));
        let json: serde_json::Value =
            serde_json::from_str(&workspace_snapshot_json(owner).expect("retained workspace JSON"))
                .expect("workspace control JSON");
        assert_eq!(
            json["control"]["operationEpoch"],
            recovery_epoch.to_string()
        );
        assert_eq!(json["control"]["hasRetainedWork"], true);
        assert_eq!(json["control"]["runtimeOperationsReady"], false);
        assert_eq!(json["control"]["recovery"]["phase"], "reconnecting");
        {
            let state = shared.session.lock().expect("recovery session state");
            assert!(state.has_retained_work());
            assert_eq!(state.selected_pane, Some(owner));
            assert!(state.pane_terminals.contains_key(&owner));
        }
        assert!(matches!(
            shared.publish_recovery_confirmation("token-1".to_owned()),
            Ok(epoch) if epoch == recovery_epoch
        ));
        let state = shared.session.lock().expect("recovery session state");
        assert_eq!(state.recovery.phase, RecoveryPhase::AwaitingConfirmation);
        assert_eq!(state.recovery.confirmation_token, "token-1");
        drop(state);
        shared.invalidate_explicitly("runtime_changed");
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Stopped);
        assert!(
            !shared.mark_ready(),
            "explicit invalidation must not resurrect Ready"
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn confirmation_token_consumption_is_single_use_across_epoch_bump() {
        let (owner, shared) = recovery_fixture();
        let epoch = shared
            .begin_recovery("transport", 1)
            .expect("recovery epoch");
        shared
            .publish_recovery_confirmation("opaque".to_owned())
            .ok()
            .expect("recovery confirmation");
        let next = {
            let mut state = shared.session.lock().expect("recovery session state");
            assert_eq!(state.operation_epoch, epoch);
            state.operation_epoch = next_operation_epoch(state.operation_epoch);
            state.recovery.phase = RecoveryPhase::Resynchronizing;
            state.recovery.confirmation_token.clear();
            state.pending_confirmation_token = Some("opaque".to_owned());
            state.operation_epoch
        };
        assert!(shared.current_request_epoch(next));
        assert!(shared.take_pending_confirmation("opaque"));
        assert!(!shared.take_pending_confirmation("opaque"));
        assert!(!shared.current_request_is_ready(next));
        registry::destroy_terminal(owner);
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
            tmux_identity: None,
            herdr_executable: None,
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
