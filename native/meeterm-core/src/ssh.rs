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

use crate::attachment::{self, AttachmentBlock, AttachmentEndpoint, DestinationFence};
use crate::registry::{self, TerminalId};
use crate::terminal::{INPUT_QUEUE_CAPACITY, TerminalError};
use crate::tmux::{self, PaneSnapshot, SessionSnapshot, WindowSnapshot};
use crate::workspace::{
    self, Backend, CleanupWarning, RecoveryPhase, RecoverySnapshot, RuntimeCandidate,
    RuntimeControlSnapshot, RuntimeDiscoverySnapshot, RuntimeSection, RuntimeSectionState,
    RuntimeSnapshot, RuntimeState,
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

/// Synchronous result for operations that can retire the selected runtime.
/// `Accepted` means the next connection flow was started or the owner was
/// released; it does not mean authentication or runtime selection succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeBoundaryOutcome {
    RejectedBeforeBoundary(ConnectionError),
    Accepted,
    AcceptedAfterFailure(ConnectionError),
}

impl RuntimeBoundaryOutcome {
    /// Append-only native bridge code. Existing negative connection codes
    /// remain pre-boundary rejections; -15 means the old binding boundary was
    /// accepted but starting its replacement failed.
    pub const ACCEPTED_AFTER_FAILURE_CODE: i32 = -15;

    pub const fn bridge_code(self) -> i32 {
        match self {
            Self::RejectedBeforeBoundary(error) => error.code(),
            Self::Accepted => 0,
            Self::AcceptedAfterFailure(_) => Self::ACCEPTED_AFTER_FAILURE_CODE,
        }
    }
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
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionEndpoint {
    host: String,
    port: u16,
    username: String,
    known_hosts_path: PathBuf,
    backend: Backend,
    runtime: Option<String>,
}

/// Durable, connection-scoped authority for the tmux zoom hooks.  This is
/// deliberately kept beside the retained SessionState rather than in a
/// ControlClient: a transport-loss recovery creates a fresh client while the
/// authenticated host/runtime binding remains the same.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ZoomCleanupRecord {
    endpoint: SessionEndpoint,
    runtime: tmux::SessionEpoch,
    window: u64,
    pane: u64,
    hooks: tmux::ZoomRecoveryHookAllocation,
    /// Internal identity of one cleanup operation. It is never serialized or
    /// exposed to JavaScript; it only keeps a warning ID stable across actor
    /// replacement while allowing a later cleanup result to get a new ID.
    result_id: u64,
    unconfirmed: bool,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CleanupWarningResult {
    result_id: u64,
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

    /// Credential-free endpoint identity for attachment fencing. Reuse of a
    /// remote attachment path is allowed only while this whole identity
    /// still matches; the path never crosses to another SSH endpoint.
    fn attachment_endpoint(&self) -> AttachmentEndpoint {
        AttachmentEndpoint {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            backend: self.backend,
            runtime: self.runtime.clone(),
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
    /// The last selected Herdr stable terminal identity.  Herdr pane aliases
    /// are mutable and therefore never serve as the recovery identity.
    recovery_terminal_id: Option<String>,
    /// A selected Herdr group can legitimately have no panes. Keep that
    /// distinction separate from a previously selected terminal that has
    /// disappeared, so recovery never falls back to an unrelated pane.
    recovery_group_id: Option<u64>,
    /// Durable tmux zoom/hook cleanup authority. This survives ControlClient
    /// replacement during same-runtime recovery, but is cleared at an
    /// explicit fresh host/backend/runtime binding boundary.
    zoom_cleanup_record: Option<ZoomCleanupRecord>,
    /// Latest single low-frequency cleanup result. It is intentionally
    /// independent from ConnectionInfo so a replacement's auth/host-key
    /// failure cannot overwrite the old-layout warning.
    cleanup_warning: Option<CleanupWarning>,
    cleanup_warning_result: Option<CleanupWarningResult>,
    /// Native-issued warning IDs are owner-scoped and never derived from a
    /// connection generation or operation epoch.
    next_cleanup_warning_id: u64,
    /// Internal identity for the latest cleanup operation. This is not
    /// serialized and is only used to keep a result's warning ID stable.
    next_cleanup_result_id: u64,
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
            recovery_terminal_id: None,
            recovery_group_id: None,
            zoom_cleanup_record: None,
            cleanup_warning: None,
            cleanup_warning_result: None,
            next_cleanup_warning_id: 0,
            next_cleanup_result_id: 0,
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
            cleanup_warning: self.cleanup_warning.clone(),
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

pub(crate) struct ConnectionShared {
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
    /// Marks only the bounded delay between automatic attempts. This lets
    /// explicit Retry, foreground return, and network changes wake that wait
    /// without creating a parallel attempt or leaving a stale wake for a later
    /// backoff. A waker clears it to record the wake; the waiter treats a
    /// cleared flag during its wait as woken.
    reconnect_waiting: AtomicBool,
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
    /// The cleanup result identity owned by this connection actor. A record
    /// backed result remains identifiable after its record is discarded, and
    /// a no-record retirement result is minted only once for this actor.
    cleanup_warning_result: Mutex<Option<CleanupWarningResult>>,
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
            reconnect_waiting: AtomicBool::new(false),
            foreground_transition: Mutex::new(()),
            foreground: AtomicBool::new(foreground),
            automatic_reconnect: AtomicBool::new(automatic_reconnect),
            ready_once: AtomicBool::new(false),
            ready_epoch: AtomicU64::new(0),
            recovery_starting: AtomicBool::new(false),
            zoom_cleanup_outcome: Mutex::new(ZoomCleanupOutcome::NotNeeded),
            cleanup_warning_result: Mutex::new(None),
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
        // Do not clear the durable hook record here. A transport-loss actor
        // may have already lost active ownership while its indexed hooks (or
        // an unknown zoom mutation) still need same-runtime reconciliation.
        self.zoom_cleanup_pending.store(false, Ordering::Release);
    }

    fn mark_zoom_cleanup_pending(&self) {
        self.zoom_cleanup_pending.store(true, Ordering::Release);
    }

    fn clear_zoom_cleanup_pending(&self) {
        self.zoom_cleanup_pending.store(false, Ordering::Release);
    }

    /// Mark a surviving cleanup record as unconfirmed before its detailed
    /// target is discarded. The owner-transition caller publishes the warning
    /// while the record is still present, then calls the discard half below.
    fn mark_zoom_cleanup_record_unconfirmed(&self) -> Option<CleanupWarningResult> {
        let Ok(mut state) = self.session.lock() else {
            return None;
        };
        if state.generation != self.generation {
            return None;
        }
        let record = state.zoom_cleanup_record.as_mut()?;
        record.unconfirmed = true;
        Some(CleanupWarningResult {
            result_id: record.result_id,
        })
    }

    /// Discard an already-published old-binding cleanup record. This is only
    /// called at explicit retirement, never for automatic same-runtime
    /// recovery, and therefore cannot transfer a target to a new binding.
    fn discard_zoom_cleanup_record(&self) -> bool {
        let Ok(mut state) = self.session.lock() else {
            return false;
        };
        if state.generation != self.generation || state.zoom_cleanup_record.is_none() {
            return false;
        }
        state.zoom_cleanup_record = None;
        state.meeterm_zoomed = false;
        state.meeterm_zoomed_window = None;
        state.meeterm_zoomed_pane = None;
        self.zoom_cleanup_pending.store(false, Ordering::Release);
        true
    }

    fn record_zoom_cleanup_intent(
        &self,
        runtime: tmux::SessionEpoch,
        window: u64,
        pane: u64,
        hooks: tmux::ZoomRecoveryHookAllocation,
    ) -> bool {
        let Ok(mut state) = self.session.lock() else {
            return false;
        };
        if state.generation != self.generation {
            return false;
        }
        let Some(endpoint) = state.endpoint.clone() else {
            return false;
        };
        if let Some(existing) = state.zoom_cleanup_record.as_ref()
            && (existing.endpoint != endpoint
                || existing.runtime != runtime
                || existing.window != window
                || existing.pane != pane
                || existing.hooks != hooks)
        {
            // An unconfirmed allocation is never overwritten by a new slot or
            // a new target. The caller must reconcile/release it first.
            return false;
        }
        let result_id = state
            .zoom_cleanup_record
            .as_ref()
            .map(|record| record.result_id)
            .filter(|result_id| *result_id != 0)
            .unwrap_or_else(|| {
                state.next_cleanup_result_id = next_nonzero_counter(state.next_cleanup_result_id);
                state.next_cleanup_result_id
            });
        state.zoom_cleanup_record = Some(ZoomCleanupRecord {
            endpoint,
            runtime,
            window,
            pane,
            hooks,
            result_id,
            unconfirmed: true,
            generation: self.generation,
        });
        self.zoom_cleanup_pending.store(true, Ordering::Release);
        true
    }

    fn confirm_zoom_cleanup_intent(
        &self,
        runtime: &tmux::SessionEpoch,
        window: u64,
        pane: u64,
        hooks: tmux::ZoomRecoveryHookAllocation,
    ) {
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
            && let Some(record) = state.zoom_cleanup_record.as_mut()
            && record.generation == self.generation
            && record.runtime == *runtime
            && record.window == window
            && record.pane == pane
            && record.hooks == hooks
        {
            record.unconfirmed = false;
        }
    }

    fn zoom_cleanup_record_for(
        &self,
        runtime: &tmux::SessionEpoch,
    ) -> Result<Option<ZoomCleanupRecord>, FlowFailure> {
        let state = self.session.lock().map_err(|_| FlowFailure::Stale)?;
        if state.generation != self.generation {
            return Err(FlowFailure::Stale);
        }
        let Some(record) = state.zoom_cleanup_record.clone() else {
            return Ok(None);
        };
        let endpoint = state.endpoint.clone().ok_or(FlowFailure::Stale)?;
        if record.generation != self.generation
            || record.endpoint != endpoint
            || record.runtime != *runtime
        {
            return Err(FlowFailure::TmuxRuntimeMissing);
        }
        Ok(Some(record))
    }

    fn clear_zoom_cleanup_record(
        &self,
        runtime: &tmux::SessionEpoch,
        window: u64,
        hooks: tmux::ZoomRecoveryHookAllocation,
    ) -> bool {
        let Ok(mut state) = self.session.lock() else {
            return false;
        };
        let matches = state.generation == self.generation
            && state.zoom_cleanup_record.as_ref().is_some_and(|record| {
                record.generation == self.generation
                    && record.runtime == *runtime
                    && record.window == window
                    && record.hooks == hooks
            });
        if matches {
            state.zoom_cleanup_record = None;
            self.zoom_cleanup_pending.store(false, Ordering::Release);
        }
        matches
    }

    fn has_zoom_cleanup_intent(&self) -> bool {
        self.zoom_cleanup_pending.load(Ordering::Acquire)
            || self
                .session
                .lock()
                .map(|state| {
                    state.generation == self.generation
                        && (state.meeterm_zoomed || state.zoom_cleanup_record.is_some())
                })
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
        self.publish_layout_restore_warning_for(None);
    }

    fn publish_layout_restore_warning_for(&self, result: Option<CleanupWarningResult>) {
        let Ok(mut owned_result) = self.cleanup_warning_result.lock() else {
            return;
        };
        let Ok(mut state) = self.session.lock() else {
            return;
        };
        if state.generation != self.generation {
            return;
        }
        let result = match (result, *owned_result) {
            (Some(candidate), Some(current)) if candidate == current => current,
            (Some(candidate), _) => candidate,
            (None, Some(current)) => current,
            (None, None) => {
                state.next_cleanup_result_id = next_nonzero_counter(state.next_cleanup_result_id);
                CleanupWarningResult {
                    result_id: state.next_cleanup_result_id,
                }
            }
        };
        *owned_result = Some(result);
        let replace =
            state.cleanup_warning.is_none() || state.cleanup_warning_result != Some(result);
        if replace {
            state.next_cleanup_warning_id = next_nonzero_counter(state.next_cleanup_warning_id);
            state.cleanup_warning = Some(CleanupWarning {
                id: state.next_cleanup_warning_id.to_string(),
                code: workspace::CLEANUP_WARNING_CODE.to_owned(),
                message: workspace::CLEANUP_WARNING_MESSAGE.to_owned(),
            });
            state.cleanup_warning_result = Some(result);
        }
        drop(state);
        drop(owned_result);

        // Keep the legacy fixed C snapshot useful as a compatibility fallback,
        // but never overwrite a newer binding's own auth/host-key diagnostic.
        if let Ok(mut info) = self.info.lock()
            && (info.error_code.is_empty() || info.error_code == workspace::CLEANUP_WARNING_CODE)
        {
            info.error_code = workspace::CLEANUP_WARNING_CODE.to_owned();
            info.error_message = recovery_reason_message("layout_restore_unconfirmed");
        }
    }

    fn command_sender(&self) -> Option<mpsc::Sender<ControlRequest>> {
        self.commands
            .lock()
            .ok()
            .and_then(|commands| commands.clone())
    }

    /// Capture the current insertion destination for a new attachment op.
    /// The fence binds stable identities only — connection generation,
    /// session operation epoch, the selected remote pane, the native
    /// terminal mapped to it, Herdr's stable terminal id, and the
    /// credential-free endpoint — never a display label or array index.
    pub(crate) fn attachment_capture_fence(&self) -> Result<DestinationFence, AttachmentBlock> {
        let state = self.session.lock().map_err(|_| AttachmentBlock::Internal)?;
        if state.generation != self.generation || self.is_cancelled() {
            return Err(AttachmentBlock::StaleConnection);
        }
        let pane_id = state
            .selected_pane
            .ok_or(AttachmentBlock::DestinationMissing)?;
        let native_terminal = state
            .pane_terminals
            .get(&pane_id)
            .copied()
            .ok_or(AttachmentBlock::DestinationMissing)?;
        let endpoint = state
            .endpoint
            .as_ref()
            .ok_or(AttachmentBlock::NotReady)?
            .attachment_endpoint();
        Ok(DestinationFence {
            generation: state.generation,
            operation_epoch: state.operation_epoch,
            pane_id,
            native_terminal,
            herdr_terminal_id: state
                .herdr
                .panes
                .get(&pane_id)
                .map(|pane| pane.terminal_id.clone()),
            endpoint,
        })
    }

    /// Re-bind an operation's destination pane to the *current* actor state
    /// for an explicit transfer retry. The same pane identity must still
    /// exist on the same endpoint; the pane need not be selected to upload.
    pub(crate) fn attachment_recheck_fence(
        &self,
        fence: &DestinationFence,
    ) -> Result<DestinationFence, AttachmentBlock> {
        let state = self.session.lock().map_err(|_| AttachmentBlock::Internal)?;
        if state.generation != self.generation || self.is_cancelled() {
            return Err(AttachmentBlock::StaleConnection);
        }
        let endpoint = state
            .endpoint
            .as_ref()
            .ok_or(AttachmentBlock::NotReady)?
            .attachment_endpoint();
        if endpoint != fence.endpoint {
            return Err(AttachmentBlock::StaleConnection);
        }
        let native_terminal = state
            .pane_terminals
            .get(&fence.pane_id)
            .copied()
            .ok_or(AttachmentBlock::DestinationMissing)?;
        if !state
            .snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == fence.pane_id)
        {
            return Err(AttachmentBlock::DestinationMissing);
        }
        let herdr_terminal_id = state
            .herdr
            .panes
            .get(&fence.pane_id)
            .map(|pane| pane.terminal_id.clone());
        if herdr_terminal_id != fence.herdr_terminal_id {
            return Err(AttachmentBlock::DestinationChanged);
        }
        Ok(DestinationFence {
            generation: state.generation,
            operation_epoch: state.operation_epoch,
            pane_id: fence.pane_id,
            native_terminal,
            herdr_terminal_id,
            endpoint,
        })
    }

    /// Validate the whole destination fence under the session lock and, only
    /// while every captured identity still matches, enqueue exactly one line
    /// through `paste_utf8_at_epoch`. The native terminal epoch is re-read
    /// inside the same lock window so a suspended→resumed binding of the
    /// same pane is accepted, while a concurrently replaced binding fails
    /// closed inside the epoch check. Selection changes are serialized by
    /// the session lock, so an accepted paste cannot land on another pane.
    pub(crate) fn attachment_insert_line(
        &self,
        fence: &DestinationFence,
        line: &[u8],
    ) -> Result<usize, AttachmentBlock> {
        let state = self.session.lock().map_err(|_| AttachmentBlock::Internal)?;
        if state.generation != self.generation
            || state.generation != fence.generation
            || self.is_cancelled()
            || self.explicit_cleanup_requested()
        {
            return Err(AttachmentBlock::StaleConnection);
        }
        if state.operation_epoch != fence.operation_epoch {
            return Err(AttachmentBlock::StaleOperation);
        }
        if state.recovery.phase != RecoveryPhase::None || !state.runtime_operations_ready {
            return Err(AttachmentBlock::NotReady);
        }
        let endpoint = state
            .endpoint
            .as_ref()
            .ok_or(AttachmentBlock::NotReady)?
            .attachment_endpoint();
        if endpoint != fence.endpoint {
            return Err(AttachmentBlock::StaleConnection);
        }
        if state.selected_pane != Some(fence.pane_id) {
            return Err(AttachmentBlock::DestinationChanged);
        }
        if state.pane_terminals.get(&fence.pane_id).copied() != Some(fence.native_terminal) {
            return Err(AttachmentBlock::DestinationChanged);
        }
        if !state
            .snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == fence.pane_id)
        {
            return Err(AttachmentBlock::DestinationMissing);
        }
        if state
            .herdr
            .panes
            .get(&fence.pane_id)
            .map(|pane| pane.terminal_id.as_str())
            != fence.herdr_terminal_id.as_deref()
        {
            return Err(AttachmentBlock::DestinationChanged);
        }
        if !state.terminal_input_ready {
            return Err(AttachmentBlock::InputNotReady);
        }
        // Lock order session -> registry -> Terminal is the established
        // order; refresh_terminal_input_ready already reads the registry
        // under this same lock.
        let epoch = registry::operation_epoch(fence.native_terminal)
            .map_err(|_| AttachmentBlock::StaleTerminal)?;
        registry::paste_utf8_at_epoch(fence.native_terminal, epoch, line)
            .map_err(attachment_paste_block)
    }

    /// Re-check only the endpoint half of a destination fence for the
    /// explicit remote-delete path: the pane may be gone, but the SSH
    /// endpoint and connection generation must still match so deletion
    /// runs on the same authenticated host that received the upload.
    pub(crate) fn attachment_recheck_endpoint(
        &self,
        endpoint: &AttachmentEndpoint,
    ) -> Result<(), AttachmentBlock> {
        let state = self.session.lock().map_err(|_| AttachmentBlock::Internal)?;
        if state.generation != self.generation || self.is_cancelled() {
            return Err(AttachmentBlock::StaleConnection);
        }
        let current = state
            .endpoint
            .as_ref()
            .ok_or(AttachmentBlock::NotReady)?
            .attachment_endpoint();
        if current != *endpoint {
            return Err(AttachmentBlock::StaleConnection);
        }
        Ok(())
    }

    /// Enqueue this operation's SFTP command on the actor's serialized
    /// command channel, tagged with the current operation epoch. The actor
    /// re-validates the epoch and readiness gates before opening a channel.
    pub(crate) fn attachment_enqueue_command(
        &self,
        command: ControlCommand,
    ) -> Result<(), AttachmentBlock> {
        let epoch = self.operation_epoch();
        let sender = self.command_sender().ok_or(AttachmentBlock::NotReady)?;
        sender
            .try_send(ControlRequest { epoch, command })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => AttachmentBlock::Busy,
                mpsc::error::TrySendError::Closed(_) => AttachmentBlock::NotReady,
            })
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
            if recovery_phase == RecoveryPhase::Stopped {
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
        };
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

    /// Re-open a stopped recovery at the displayed epoch. This is a deliberate
    /// handoff boundary rather than a normal loss transition: `begin_recovery`
    /// rejects finished actors so late transport callbacks cannot mutate
    /// their state. `Stopped` is published only with actor finish, so Retry
    /// can retain that finished map owner until its replacement commits.
    fn begin_stopped_recovery(&self, expected_epoch: u64) -> Result<u64, ConnectionError> {
        let mut info = self.info.lock().map_err(|_| ConnectionError::Internal)?;
        if self.is_cancelled() || self.explicit_cleanup_requested() {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        let mut state = self.session.lock().map_err(|_| ConnectionError::Internal)?;
        if state.generation != self.generation || state.recovery.phase != RecoveryPhase::Stopped {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        if !info.finished {
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
        };
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
        state.recovery_terminal_id = None;
        state.recovery_group_id = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        info.state = ConnectionState::AttachingRuntime;
        true
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
        if state.recovery.phase == RecoveryPhase::Stopped {
            return Err(FlowFailure::Stale);
        }
        commit(&mut state)?;
        state.recovery = RecoverySnapshot::default();
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

    fn stop_recovery(&self, reason: &str) {
        let Ok(mut info) = self.info.lock() else {
            return;
        };
        if self.is_cancelled() || self.explicit_cleanup_requested() || info.finished {
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
        let reason = sanitize_recovery_reason(reason);
        if info.state == ConnectionState::Failed
            && state.recovery.reason == reason
            && !state.runtime_operations_ready
            && !state.terminal_input_ready
        {
            return;
        }
        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        if state.recovery.phase == RecoveryPhase::None {
            state.recovery.phase = RecoveryPhase::Reconnecting;
        }
        // Keep the recovery phase in progress until `finish` commits the
        // actor's exit. The reason is already authoritative, and the epoch
        // plus both gates are revoked synchronously at failure detection.
        state.recovery.reason = reason.clone();
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        if !preserve_security_failure {
            info.state = ConnectionState::Failed;
            info.error_code = reason.clone();
            info.error_message = recovery_reason_message(&reason);
        }
        info.pending = None;
        drop(state);
        drop(info);
        // Herdr recovery can detect a local identity/controller failure
        // before the enclosing SSH flow returns. Revoke the native transport
        // immediately so the input path is closed during that unwind too.
        detach_all(self);
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
        let finished = info.finished;
        let first_request = !self.explicit_cleanup_requested();
        if first_request {
            self.explicit_cleanup_requested
                .store(true, Ordering::Release);
        }
        if let Ok(mut state) = self.session.lock()
            && state.generation == self.generation
        {
            // Publish the explicit lifecycle intent before changing the
            // recovery phase. The actor can then choose its cleanup branch
            // while the transport remains usable. Repeated callers (for
            // example Change followed by replacement) keep the first epoch
            // boundary and only re-wake the same actor.
            if first_request {
                state.operation_epoch = next_operation_epoch(state.operation_epoch);
                state.recovery.phase = RecoveryPhase::Stopped;
                state.recovery.reason = sanitize_recovery_reason(reason);
            }
            state.runtime_operations_ready = false;
            state.terminal_input_ready = false;
        }
        // A finished owner can still be the current map entry during a Retry
        // handoff. Revoke its intent, but never republish it as Closing after
        // actor finish or let a later Retry reopen it.
        info.state = if finished {
            ConnectionState::Disconnected
        } else {
            ConnectionState::Closing
        };
        info.pending = None;
        self.explicit_cleanup_notify.notify_waiters();
    }

    fn restore_stopped_recovery_after_start_failure(&self, error: ConnectionError) {
        let Ok(mut info) = self.info.lock() else {
            return;
        };
        if !info.finished || self.is_cancelled() || self.explicit_cleanup_requested() {
            return;
        }
        let Ok(mut state) = self.session.lock() else {
            return;
        };
        if state.generation != self.generation
            || state.recovery.phase != RecoveryPhase::Reconnecting
            || state.recovery.reason != "manual_retry"
        {
            return;
        }
        state.recovery.phase = RecoveryPhase::Stopped;
        state.recovery.reason = error.error_code().to_owned();
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        info.state = ConnectionState::Failed;
        info.error_code = error.error_code().to_owned();
        info.error_message = error.to_string();
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

    /// Wake only an actor that is already inside the automatic retry wait.
    /// Consuming the waiter's registration records the wake even if the
    /// waiter has not reached its next check yet, and a wake outside a wait
    /// leaves no permit that could skip a later attempt's backoff.
    fn wake_reconnect_wait(&self) -> bool {
        if self.is_cancelled()
            || self.explicit_cleanup_requested()
            || !self.is_foreground()
            || !self.automatic_reconnect_enabled()
            || !self.reconnect_waiting.load(Ordering::Acquire)
        {
            return false;
        }
        let recovering_retained_work = self
            .session
            .lock()
            .map(|state| {
                state.generation == self.generation
                    && state.recovery.phase == RecoveryPhase::Reconnecting
                    && state.has_retained_work()
            })
            .unwrap_or(false);
        if !recovering_retained_work
            || self
                .reconnect_waiting
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        self.retry_notify.notify_waiters();
        true
    }

    fn set_foreground(&self, foreground: bool) {
        let _transition = self.foreground_transition_lock();
        self.foreground.store(foreground, Ordering::Release);

        let (generation, backend, terminal_ids) = match self.session.lock() {
            Ok(mut state) if state.generation == self.generation => {
                state.foreground = foreground;
                if !foreground {
                    state.terminal_input_ready = false;
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
        if foreground {
            self.wake_reconnect_wait();
        }
        // Retain one wake permit for the live Herdr controller, which may be
        // between awaits while processing a frame/input request.
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
            let failed_before_finish = info.state == ConnectionState::Failed;
            let security_reason = preserved_recovery_reason_from_error_code(&info.error_code);
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
                let reason = security_reason
                    .map(str::to_owned)
                    .or_else(|| {
                        (failed_before_finish && !state.recovery.reason.is_empty())
                            .then(|| state.recovery.reason.clone())
                    })
                    .unwrap_or_else(|| recovery_reason_for_failure(failure).to_owned());
                state.operation_epoch = next_operation_epoch(state.operation_epoch);
                state.recovery.phase = RecoveryPhase::Stopped;
                state.recovery.reason = reason.clone();
                state.runtime_operations_ready = false;
                state.terminal_input_ready = false;
                if info.state != ConnectionState::Disconnected && security_reason.is_none() {
                    info.state = ConnectionState::Failed;
                    info.error_code = reason.clone();
                    info.error_message = recovery_reason_message(&reason);
                }
            }
            info.pending = None;
        }
        self.finished_notify.notify_waiters();
    }

    fn snapshot(&self) -> Result<ConnectionSnapshot, ConnectionError> {
        let mut snapshot = self
            .info
            .lock()
            .map(|info| info.snapshot())
            .map_err(|_| ConnectionError::Internal)?;
        let warning = self
            .session
            .lock()
            .map(|state| state.cleanup_warning.is_some())
            .unwrap_or(false);
        // Preserve a new binding's own host/auth diagnostic if it has already
        // become authoritative. The old layout result is kept separately in
        // SessionState and was already surfaced through the warning boundary
        // while the replacement was being prepared.
        if warning && snapshot.error_code_len == 0 {
            apply_layout_restore_warning(&mut snapshot);
        }
        Ok(snapshot)
    }
}

/// A command is tagged at acceptance time.  The actor checks the tag again
/// immediately before any remote operation, so a command accepted just
/// before a transport loss can never be replayed in a later operation epoch.
struct ControlRequest {
    epoch: u64,
    command: ControlCommand,
}

pub(crate) enum ControlCommand {
    RefreshRuntimes,
    SelectRuntime {
        candidate_id: String,
    },
    CreateRuntime {
        backend: Backend,
        name: String,
    },
    SelectPane {
        window_id: u64,
        pane_id: u64,
    },
    CreateWorkspace {
        name: String,
    },
    RenameWorkspace {
        window_id: u64,
        name: String,
    },
    CloseWorkspace {
        window_id: u64,
    },
    CreatePane {
        window_id: u64,
    },
    RenamePane {
        pane_id: u64,
        name: String,
    },
    ClosePane {
        pane_id: u64,
    },
    RefreshTerminal,
    CreateGroup {
        window_id: u64,
        name: String,
    },
    RenameGroup {
        group_id: u64,
        name: String,
    },
    CloseGroup {
        group_id: u64,
    },
    SelectGroup {
        group_id: u64,
    },
    SetTerminalVisible {
        visible: bool,
    },
    /// Upload one attachment file over a second SFTP channel on this same
    /// authenticated session. The actor only opens the channel; the bounded
    /// byte streaming runs in a detached task so the interactive loop never
    /// stalls on a large image.
    SftpUpload {
        attachment_id: u64,
    },
    /// Explicit remote deletion of the names this attachment operation
    /// generated, on the same authenticated session only.
    SftpRemove {
        attachment_id: u64,
    },
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

    #[cfg(test)]
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

/// Retry the retained recovery intent for the current owner. During an active
/// attempt this is an accepted no-op; during backoff it wakes the existing
/// actor, and only a stopped actor starts a replacement generation.
pub fn retry_recovery(terminal_id: TerminalId, expected_epoch: u64) -> Result<(), ConnectionError> {
    retry_recovery_with_start(terminal_id, expected_epoch, start_connection)
}

fn retry_recovery_with_start(
    terminal_id: TerminalId,
    expected_epoch: u64,
    start: impl FnOnce(TerminalId, ConnectionStart) -> Result<(), ConnectionError>,
) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    let shared = current_connection(terminal_id)?;
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        // Explicit disconnect is a one-way lifecycle boundary.  A later
        // foreground notification or stale Retry button must not resurrect
        // the cancelled actor; a fresh connect is required instead.
        return Err(ConnectionError::RecoveryUnavailable);
    }
    let (phase, profile, epoch, has_retained_work) = {
        let state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        (
            state.recovery.phase,
            state.profile.clone(),
            state.operation_epoch,
            state.has_retained_work(),
        )
    };
    if epoch != expected_epoch {
        return Err(ConnectionError::RecoveryStale);
    }
    if !shared.has_been_ready() || !has_retained_work || phase == RecoveryPhase::None {
        return Err(ConnectionError::RecoveryUnavailable);
    }

    if phase != RecoveryPhase::Stopped {
        shared.wake_reconnect_wait();
        return Ok(());
    }
    let profile = profile.ok_or(ConnectionError::ReconnectUnavailable)?;
    // The finished old entry remains current while the replacement is
    // prepared. Retained SessionState and the native Term are preserved, and
    // the map slot changes only when the new generation is ready to install.
    if shared.recovery_starting.swap(true, Ordering::AcqRel) {
        // The first replacement attempt already owns this Stopped handoff.
        return Ok(());
    }
    // Move the public state to Reconnecting before preparing the replacement.
    // A second caller with this new epoch sees an active same-intent handoff
    // and returns a no-op; it cannot cancel or duplicate the first attempt.
    if let Err(error) = shared.begin_stopped_recovery(expected_epoch) {
        shared.recovery_starting.store(false, Ordering::Release);
        return Err(error);
    }
    let result = start(terminal_id, ConnectionStart::AutomaticReconnect(profile));
    if result.is_err() {
        shared.recovery_starting.store(false, Ordering::Release);
        let still_current = connections()
            .lock()
            .map(|entries| {
                entries
                    .get(&terminal_id)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.shared, &shared))
            })
            .unwrap_or(false);
        if still_current {
            shared.restore_stopped_recovery_after_start_failure(
                result.expect_err("checked failed replacement startup"),
            );
        }
    }
    result
}

/// Explicitly leave the retained runtime and enter the existing authenticated
/// picker path.  This is the only recovery API that clears the cached runtime
/// binding/native view, and it does so before a new runtime can be acquired.
pub fn change_runtime(terminal_id: TerminalId, expected_epoch: u64) -> RuntimeBoundaryOutcome {
    change_runtime_with_start(terminal_id, expected_epoch, start_connection)
}

fn change_runtime_with_start(
    terminal_id: TerminalId,
    expected_epoch: u64,
    start: impl FnOnce(TerminalId, ConnectionStart) -> Result<(), ConnectionError>,
) -> RuntimeBoundaryOutcome {
    if let Err(error) = registry::shared_terminal(terminal_id).map_err(map_terminal_error) {
        return RuntimeBoundaryOutcome::RejectedBeforeBoundary(error);
    }
    let shared = match current_connection(terminal_id) {
        Ok(shared) => shared,
        Err(error) => return RuntimeBoundaryOutcome::RejectedBeforeBoundary(error),
    };
    let profile = {
        let state = match shared.session.lock() {
            Ok(state) => state,
            Err(_) => {
                return RuntimeBoundaryOutcome::RejectedBeforeBoundary(ConnectionError::Internal);
            }
        };
        if state.operation_epoch != expected_epoch {
            return RuntimeBoundaryOutcome::RejectedBeforeBoundary(ConnectionError::RecoveryStale);
        }
        match state.profile.clone() {
            Some(profile) => profile,
            None => {
                return RuntimeBoundaryOutcome::RejectedBeforeBoundary(
                    ConnectionError::RecoveryUnavailable,
                );
            }
        }
    };
    // Cancel a possible in-flight replacement before starting the explicit
    // runtime change. The owner ticket is checked again by start_connection,
    // so a late handoff cannot install the old actor after this boundary.
    let owner = match owner_transition(terminal_id) {
        Ok(owner) => owner,
        Err(error) => return RuntimeBoundaryOutcome::RejectedBeforeBoundary(error),
    };
    let commit = match owner.commit.lock() {
        Ok(commit) => commit,
        Err(_) => {
            return RuntimeBoundaryOutcome::RejectedBeforeBoundary(ConnectionError::Internal);
        }
    };
    owner.cancel_current_locked();
    // Publish explicit intent under the same short owner boundary as ticket
    // cancellation. A Retry that begins after this lock is released sees the
    // finished owner as revoked and cannot restore Stopped or install it.
    shared.invalidate_explicitly("runtime_changed");
    drop(commit);
    detach_all(&shared);
    attachment::generation_finished(shared.terminal_id, shared.generation);
    match start(terminal_id, ConnectionStart::ManualReconnect(profile)) {
        Ok(()) => RuntimeBoundaryOutcome::Accepted,
        Err(error) => RuntimeBoundaryOutcome::AcceptedAfterFailure(error),
    }
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

/// Wake every retained recovery actor that is currently waiting between
/// automatic attempts. This is a one-shot signal from the platform's normal
/// network-change callback; it does not probe or replace healthy connections.
pub fn network_changed() {
    let shared_connections = connections()
        .lock()
        .map(|connections| {
            connections
                .values()
                .map(|entry| Arc::clone(&entry.shared))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for shared in shared_connections {
        shared.wake_reconnect_wait();
    }
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

/// Convert a surviving cleanup authority into the old-binding warning at an
/// explicit retirement boundary. A previous actor outcome is not proof that a
/// still-present record was released: the detailed target is retired only
/// after this conversion has been published into SessionState/UI state.
fn retire_explicit_cleanup_result(
    shared: &ConnectionShared,
    shutdown: ExplicitShutdownResult,
    automatic_reconnect: bool,
) -> bool {
    if automatic_reconnect {
        // Same-runtime recovery keeps the record for the replacement actor;
        // transport loss alone is not an explicit retirement result.
        return false;
    }
    let surviving_record = shared.mark_zoom_cleanup_record_unconfirmed();
    if let Some(result) = surviving_record {
        shared.record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
        shared.publish_layout_restore_warning_for(Some(result));
        shared.discard_zoom_cleanup_record();
        return true;
    }
    if shutdown.cleanup == ZoomCleanupOutcome::UnconfirmedOrFailed {
        shared.publish_layout_restore_warning();
        return true;
    }
    false
}

fn finish_or_force_explicit_shutdown(
    runtime: &'static Runtime,
    shared: Arc<ConnectionShared>,
    abort: tokio::task::AbortHandle,
) -> ExplicitShutdownResult {
    let finished = wait_for_generation_finish(runtime, Arc::clone(&shared));
    if finished {
        if shared.explicit_cleanup_requested() {
            shared.cancel();
        }
    } else {
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
    if finished {
        if shared.explicit_cleanup_requested() {
            shared.cancel();
        }
    } else {
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

    let (old, recovery_owner) = {
        let _commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
        if !owner.install_allowed(ticket) {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        let mut entries = connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if reconnecting {
            let current = entries
                .get(&terminal_id)
                .map(|entry| Arc::clone(&entry.shared))
                .ok_or(ConnectionError::RecoveryUnavailable)?;
            let finished = current
                .info
                .lock()
                .map(|info| info.finished)
                .unwrap_or(false);
            if !finished || current.is_cancelled() || current.explicit_cleanup_requested() {
                return Err(ConnectionError::RecoveryUnavailable);
            }
            // A Retry keeps the finished same-intent owner referenceable while
            // the replacement generation is prepared. The map slot is swapped
            // only in the final owner commit below.
            (None, Some(current))
        } else {
            (entries.remove(&terminal_id), None)
        }
    };
    if let Some(old) = old {
        let old_shared = Arc::clone(&old.shared);
        old.shared.invalidate_explicitly("runtime_replaced");
        // A controller/transport that never acknowledges the explicit
        // cleanup cannot safely retain the old generation. The shared helper
        // forces both cancellation and task abort before the new generation
        // is allowed to touch SessionState.
        let shutdown = finish_or_force_explicit_shutdown(runtime, old.shared, old.abort);
        // Only an explicit binding retirement removes the old entry here, and
        // it is the last point at which the old generation may decide what to
        // do with its detailed cleanup target.
        let _ = retire_explicit_cleanup_result(&old_shared, shutdown, false);
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
    // Explicit retirement already published its result into the owner-scoped
    // SessionState. Do not ask this new binding to synthesize another
    // no-record result while inheriting that warning.
    if reconnecting {
        // `ready_once` belongs to the actor, while the retained topology and
        // recovery phase belong to SessionState.  A replacement actor must
        // inherit the fact that its intent was already Ready; otherwise its
        // first failed reconstruction would be mistaken for a pre-Ready
        // connection and native retry would stop after one attempt.
        shared.ready_once.store(true, Ordering::Release);
    }
    if !reconnecting {
        let mut state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        // Abort completion is asynchronous. Install the new generation and
        // discard the old actor's local cleanup authority in one operation.
        state.generation = generation;
        state.meeterm_zoomed = false;
        state.meeterm_zoomed_window = None;
        state.meeterm_zoomed_pane = None;
        // A manual/fresh binding is never allowed to inherit a cleanup
        // target from another host, backend, or runtime. The old shared
        // actor already had its bounded cleanup opportunity above.
        state.zoom_cleanup_record = None;
        // Candidate IDs are scoped to the connection generation. A reconnect
        // must not expose or accept the previous generation's picker IDs
        // before a fresh discovery pass publishes replacements.
        state.runtime_candidates.clear();
        state.runtime_discovery = RuntimeDiscoverySnapshot::default();
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
    let reconnect_zoom_identity = match &start {
        ConnectionStart::AutomaticReconnect(profile) => {
            profile.tmux_identity.as_ref().map(|identity| {
                (
                    SessionEndpoint::from_profile(profile),
                    identity.epoch().clone(),
                )
            })
        }
        _ => None,
    };
    let task_shared = Arc::clone(&shared);
    let join = runtime.spawn(async move {
        run_connection(task_shared, start, command_receiver, start_gate_receiver).await;
    });
    let mut start_gate_sender = Some(start_gate_sender);
    let install_result = (|| {
        let _commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
        if !owner.install_allowed(ticket) {
            return Err(ConnectionError::RecoveryUnavailable);
        }
        let mut entries = connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if reconnecting {
            let old = recovery_owner
                .as_ref()
                .ok_or(ConnectionError::RecoveryUnavailable)?;
            if old.is_cancelled()
                || old.explicit_cleanup_requested()
                || !entries
                    .get(&terminal_id)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.shared, old))
            {
                return Err(ConnectionError::RecoveryUnavailable);
            }
            let mut state = shared
                .session
                .lock()
                .map_err(|_| ConnectionError::Internal)?;
            if state.generation != old.generation
                || state.recovery.phase != RecoveryPhase::Reconnecting
            {
                return Err(ConnectionError::RecoveryUnavailable);
            }
            // Commit the generation and replace its public map entry while
            // the old finished owner is still referenceable. Public map
            // readers see either the old Reconnecting snapshot or this new
            // actor; no remove/prepare/insert absence is exposed.
            state.generation = generation;
            if let Some((endpoint, runtime)) = reconnect_zoom_identity.as_ref()
                && let Some(record) = state.zoom_cleanup_record.as_mut()
                && record.endpoint == *endpoint
                && record.runtime == *runtime
            {
                record.generation = generation;
            }
            state.runtime_candidates.clear();
            state.runtime_discovery = RuntimeDiscoverySnapshot::default();
        }
        entries.insert(
            terminal_id,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort: join.abort_handle(),
            },
        );
        owner.finish(ticket);
        // Keep the gate closed until the map install and ticket finish are
        // committed. Disconnect cannot interleave while this lock is held.
        let _ = start_gate_sender
            .take()
            .expect("replacement start gate is present")
            .send(());
        Ok(())
    })();
    if let Err(error) = install_result {
        drop(start_gate_sender);
        join.abort();
        abandon_uninstalled_connection(&shared, stale_terminals);
        return Err(error);
    }
    destroy_stale_terminals(generation, stale_terminals);
    Ok(())
}

/// Abort the session and leave the terminal in remote mode with local echo
/// disabled.  The registry entry remains available for state polling.
pub fn disconnect_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    match disconnect_with_boundary(terminal_id) {
        RuntimeBoundaryOutcome::RejectedBeforeBoundary(error)
        | RuntimeBoundaryOutcome::AcceptedAfterFailure(error) => Err(error),
        RuntimeBoundaryOutcome::Accepted => Ok(()),
    }
}

/// Owner release path for a cross-server switch. The typed result prevents a
/// caller from inferring the release boundary from a later state snapshot.
pub fn disconnect_for_switch(terminal_id: TerminalId) -> RuntimeBoundaryOutcome {
    disconnect_with_boundary(terminal_id)
}

fn disconnect_with_boundary(terminal_id: TerminalId) -> RuntimeBoundaryOutcome {
    if let Err(error) = registry::shared_terminal(terminal_id).map_err(map_terminal_error) {
        return RuntimeBoundaryOutcome::RejectedBeforeBoundary(error);
    }
    let owner = match owner_transition(terminal_id) {
        Ok(owner) => owner,
        Err(error) => return RuntimeBoundaryOutcome::RejectedBeforeBoundary(error),
    };
    let (shared, abort) = {
        let _commit = match owner.commit.lock() {
            Ok(commit) => commit,
            Err(_) => {
                return RuntimeBoundaryOutcome::RejectedBeforeBoundary(ConnectionError::Internal);
            }
        };
        // This is the boundary: any failure acquiring the connection map from
        // here on must be reported as accepted-after-failure.
        owner.cancel_current_locked();
        let entries = match connections().lock() {
            Ok(entries) => entries,
            Err(_) => {
                return RuntimeBoundaryOutcome::AcceptedAfterFailure(ConnectionError::Internal);
            }
        };
        let Some(entry) = entries.get(&terminal_id) else {
            return RuntimeBoundaryOutcome::Accepted;
        };
        entry.shared.invalidate_explicitly("explicit_disconnect");
        (Arc::clone(&entry.shared), entry.abort.clone())
    };

    detach_all(&shared);
    attachment::generation_finished(shared.terminal_id, shared.generation);
    // The normal path hard-cancels only after the controller has sent and
    // acknowledged its cleanup and the authenticated session has closed. A
    // dead transport gets the bounded force path so Disconnect cannot remain
    // pending forever.
    match runtime() {
        Ok(runtime) => {
            let shutdown = finish_or_force_explicit_shutdown(runtime, Arc::clone(&shared), abort);
            let _ = retire_explicit_cleanup_result(&shared, shutdown, false);
        }
        Err(_) => {
            // An active connection implies the native runtime exists, but a
            // poisoned/unavailable runtime must still fail closed.
            let retired_record = shared.mark_zoom_cleanup_record_unconfirmed();
            if retired_record.is_some() || shared.has_zoom_cleanup_intent() {
                shared.record_zoom_cleanup(ZoomCleanupOutcome::UnconfirmedOrFailed);
                if let Some(result) = retired_record {
                    shared.publish_layout_restore_warning_for(Some(result));
                    shared.discard_zoom_cleanup_record();
                } else {
                    shared.publish_layout_restore_warning();
                }
            }
            shared.cancel();
            abort.abort();
            shared.finish(Err(FlowFailure::Stale));
        }
    }
    RuntimeBoundaryOutcome::Accepted
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
        .unwrap_or_else(|| {
            let mut snapshot = ConnectionSnapshot::disconnected();
            if session_state(terminal_id)
                .lock()
                .map(|state| state.cleanup_warning.is_some())
                .unwrap_or(false)
            {
                apply_layout_restore_warning(&mut snapshot);
            }
            Ok(snapshot)
        })
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

pub(crate) fn current_connection(
    terminal_id: TerminalId,
) -> Result<Arc<ConnectionShared>, ConnectionError> {
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

fn next_operation_epoch(current: u64) -> u64 {
    let next = current.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

fn next_nonzero_counter(current: u64) -> u64 {
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

fn preserved_recovery_reason_from_error_code(code: &str) -> Option<&'static str> {
    match code {
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

/// Keep an already-published reason authoritative when the enclosing SSH
/// flow returns a less specific failure. Host/auth callback errors have
/// priority; otherwise a local backend recovery classification staged by
/// `stop_recovery` must survive the outer `run_connection` disposition. The
/// fixed connection snapshot keeps its existing `auth_failed` compatibility
/// code when that is what the callback published.
fn preserved_recovery_reason(shared: &ConnectionShared) -> Option<String> {
    let info = shared.info.lock().ok()?;
    if info.state != ConnectionState::Failed || info.finished {
        return None;
    }
    if let Some(reason) = preserved_recovery_reason_from_error_code(&info.error_code) {
        return Some(reason.to_owned());
    }

    let state = shared.session.lock().ok()?;
    if state.generation == shared.generation
        && matches!(
            state.recovery.phase,
            RecoveryPhase::Reconnecting | RecoveryPhase::Resynchronizing
        )
        && !state.runtime_operations_ready
        && !state.terminal_input_ready
        && !state.recovery.reason.is_empty()
        && state.recovery.reason == info.error_code
    {
        Some(state.recovery.reason.clone())
    } else {
        None
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

fn attachment_paste_block(error: TerminalError) -> AttachmentBlock {
    match error {
        TerminalError::InputNotReady => AttachmentBlock::InputNotReady,
        TerminalError::InputQueueFull => AttachmentBlock::InputQueueFull,
        TerminalError::TransportClosed => AttachmentBlock::TransportClosed,
        TerminalError::UnknownTerminal => AttachmentBlock::DestinationMissing,
        TerminalError::RemoteGenerationMismatch => AttachmentBlock::StaleTerminal,
        _ => AttachmentBlock::Internal,
    }
}

/// The command loop dropped a request before executing it (stale epoch or
/// not-ready gate). Only an attachment op records a pending reason; every
/// other command kind is already discarded silently today.
fn expire_attachment_request(command: &ControlCommand, block: AttachmentBlock) {
    match command {
        ControlCommand::SftpUpload { attachment_id } => {
            attachment::mark_pending(*attachment_id, block)
        }
        ControlCommand::SftpRemove { attachment_id } => {
            attachment::mark_remove_expired(*attachment_id, block)
        }
        _ => {}
    }
}

const SFTP_LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SftpStageFailure {
    /// The actor is shutting down; the op is retryable on a later actor.
    Stale,
    /// The remote rejected the request; retryable but likely transient.
    Failed,
    /// The remote did not answer within the bounded wait.
    Timeout,
}

/// Bounded, cancellation-aware SFTP channel setup step. `FlowFailure`
/// variants are connection-level; the launch path maps its own failure
/// kinds onto attachment pending reasons instead.
async fn sftp_stage<F, T, E>(shared: &ConnectionShared, future: F) -> Result<T, SftpStageFailure>
where
    F: Future<Output = Result<T, E>>,
{
    if shared.is_cancelled() || shared.explicit_cleanup_requested() {
        return Err(SftpStageFailure::Stale);
    }
    tokio::select! {
        _ = shared.cancelled() => Err(SftpStageFailure::Stale),
        _ = shared.explicit_cleanup() => Err(SftpStageFailure::Stale),
        result = tokio::time::timeout(SFTP_LAUNCH_TIMEOUT, future) => {
            match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) => Err(SftpStageFailure::Failed),
                Err(_) => Err(SftpStageFailure::Timeout),
            }
        }
    }
}

/// Which attachment operation an accepted SFTP channel should start.
#[derive(Clone, Copy)]
enum SftpJob {
    Upload,
    Remove,
}

/// Actor-side SFTP launcher for `ControlCommand::SftpUpload`/`SftpRemove`.
/// Channel open and the subsystem handshake stay inside the serialized
/// command dispatch (bounded, cancellation-aware); the actual SFTP work
/// moves to a detached task so a large image cannot stall the interactive
/// loop. Every failure is folded into the op's visible state — this
/// function never fails the actor itself.
async fn launch_sftp_job(
    shared: &Arc<ConnectionShared>,
    session: &client::Handle<HostKeyHandler>,
    attachment_id: u64,
    job: SftpJob,
) {
    let Some(op) = attachment::operation(attachment_id) else {
        return;
    };
    let gated = match job {
        SftpJob::Upload => attachment::gate_launch(&op, shared.terminal_id, shared.generation),
        SftpJob::Remove => attachment::gate_remove(&op, shared.terminal_id, shared.generation),
    };
    if !gated {
        return;
    }
    // A stall keeps the op retryable: upload ops pend, remove ops just
    // clear their in-flight marker while keeping the uploaded state.
    let mut channel = match sftp_stage(shared, session.channel_open_session()).await {
        Ok(channel) => channel,
        Err(failure) => {
            sftp_stall(
                job,
                &op,
                match failure {
                    SftpStageFailure::Stale => AttachmentBlock::StaleOperation,
                    SftpStageFailure::Timeout => AttachmentBlock::Timeout,
                    SftpStageFailure::Failed => AttachmentBlock::NotReady,
                },
            );
            return;
        }
    };
    if let Err(failure) = sftp_stage(shared, channel.request_subsystem(true, "sftp")).await {
        sftp_stall(
            job,
            &op,
            match failure {
                SftpStageFailure::Stale => AttachmentBlock::StaleOperation,
                SftpStageFailure::Timeout => AttachmentBlock::Timeout,
                SftpStageFailure::Failed => AttachmentBlock::StaleConnection,
            },
        );
        return;
    }
    // `request_subsystem` only sends the request; the CHANNEL_SUCCESS /
    // CHANNEL_FAILURE reply arrives on `channel.wait()` and window
    // adjustments may precede it.
    enum SubsystemReply {
        Accepted,
        Rejected,
        Stale,
        Timeout,
    }
    let reply = tokio::select! {
        _ = shared.cancelled() => SubsystemReply::Stale,
        _ = shared.explicit_cleanup() => SubsystemReply::Stale,
        result = tokio::time::timeout(SFTP_LAUNCH_TIMEOUT, async {
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Success) => break SubsystemReply::Accepted,
                    Some(ChannelMsg::Failure | ChannelMsg::Eof | ChannelMsg::Close) | None => {
                        break SubsystemReply::Rejected
                    }
                    Some(_) => {}
                }
            }
        }) => result.unwrap_or(SubsystemReply::Timeout),
    };
    match reply {
        SubsystemReply::Accepted => match job {
            SftpJob::Upload => attachment::start_transfer(op, channel.into_stream()),
            SftpJob::Remove => attachment::start_remove(op, channel.into_stream()),
        },
        SubsystemReply::Rejected => match job {
            SftpJob::Upload => attachment::launch_failed(
                &op,
                "sftp_unavailable",
                "the remote SSH server did not accept the SFTP subsystem",
            ),
            SftpJob::Remove => attachment::remove_failed(
                &op,
                "sftp_unavailable",
                "the remote SSH server did not accept the SFTP subsystem",
            ),
        },
        SubsystemReply::Stale => sftp_stall(job, &op, AttachmentBlock::StaleOperation),
        SubsystemReply::Timeout => sftp_stall(job, &op, AttachmentBlock::Timeout),
    }
}

/// Fold a pre-session SFTP stall into the op's visible state: upload ops
/// pend with their reason, remove ops clear the in-flight marker and keep
/// the reason visible without disturbing the uploaded phase.
fn sftp_stall(
    job: SftpJob,
    op: &Arc<Mutex<attachment::AttachmentOperation>>,
    block: AttachmentBlock,
) {
    match job {
        SftpJob::Upload => attachment::launch_pending(op, block),
        SftpJob::Remove => attachment::remove_stalled(op, block),
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
        attachment::generation_finished(shared.terminal_id, shared.generation);
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
        if matches!(disposition.as_ref(), Some(RetryDisposition::Retry))
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
        // Any SftpUpload request drained with the queue can never reach an
        // actor again; its op becomes an explicit-retry pending state.
        attachment::generation_finished(shared.terminal_id, shared.generation);

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
            stop_recovery_for_disposition(&shared, disposition);
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
    attachment::generation_finished(shared.terminal_id, shared.generation);
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum RetryDisposition {
    Retry,
    Stop(String),
}

fn retry_disposition(shared: &ConnectionShared, failure: FlowFailure) -> RetryDisposition {
    if automatic_retry_allowed(shared, failure) {
        RetryDisposition::Retry
    } else {
        RetryDisposition::Stop(
            preserved_recovery_reason(shared)
                .unwrap_or_else(|| recovery_reason_for_failure(failure).to_owned()),
        )
    }
}

fn stop_recovery_for_disposition(shared: &ConnectionShared, disposition: RetryDisposition) {
    if shared.has_been_ready()
        && !shared.is_cancelled()
        && shared.recovery_phase() != RecoveryPhase::Stopped
    {
        let reason = match disposition {
            RetryDisposition::Stop(reason) => reason,
            RetryDisposition::Retry => "retry_exhausted".to_owned(),
        };
        shared.stop_recovery(&reason);
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
    if retry == 0 {
        return Duration::ZERO;
    }
    let multiplier = 1_u32
        .checked_shl(retry.saturating_sub(1).min(6))
        .unwrap_or(u32::MAX);
    AUTO_RECONNECT_BASE_DELAY
        .checked_mul(multiplier)
        .unwrap_or(AUTO_RECONNECT_MAX_DELAY)
        .min(AUTO_RECONNECT_MAX_DELAY)
}

async fn wait_for_reconnect(shared: &ConnectionShared, delay: Duration) -> bool {
    // Registration is the first step: any wake after it clears the flag, so
    // it stays visible until the loop below observes it.
    shared.reconnect_waiting.store(true, Ordering::Release);
    wait_for_registered_reconnect(shared, delay).await
}

async fn wait_for_registered_reconnect(shared: &ConnectionShared, delay: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    let woken = || !shared.reconnect_waiting.load(Ordering::Acquire);
    loop {
        if shared.is_cancelled()
            || shared.explicit_cleanup_requested()
            || !shared.automatic_reconnect_enabled()
        {
            shared.reconnect_waiting.store(false, Ordering::Release);
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
                shared.reconnect_waiting.store(false, Ordering::Release);
                return false;
            }
            if shared.is_foreground() {
                continue;
            }
            tokio::select! {
                _ = shared.cancelled() => {
                    shared.reconnect_waiting.store(false, Ordering::Release);
                    return false;
                },
                _ = shared.explicit_cleanup() => {
                    shared.reconnect_waiting.store(false, Ordering::Release);
                    return false;
                },
                _ = notified => {}
            }
            continue;
        }
        if woken() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            shared.reconnect_waiting.store(false, Ordering::Release);
            return true;
        }
        let notified = shared.retry_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if shared.is_cancelled()
            || shared.explicit_cleanup_requested()
            || !shared.automatic_reconnect_enabled()
        {
            shared.reconnect_waiting.store(false, Ordering::Release);
            return false;
        }
        if !shared.is_foreground() {
            continue;
        }
        if woken() {
            return true;
        }
        tokio::select! {
            _ = shared.cancelled() => {
                shared.reconnect_waiting.store(false, Ordering::Release);
                return false;
            },
            _ = shared.explicit_cleanup() => {
                shared.reconnect_waiting.store(false, Ordering::Release);
                return false;
            },
            _ = &mut notified => {},
            _ = tokio::time::sleep_until(deadline) => {
                shared.reconnect_waiting.store(false, Ordering::Release);
                return true;
            },
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
    // Once the interactive channel has reported a transport failure, waiting
    // for an SSH disconnect response only delays the retained retry. Explicit
    // Disconnect/Change still gets its ordered graceful cleanup opportunity.
    if should_disconnect_after_flow(&shared, &result) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            session.disconnect(Disconnect::ByApplication, "meeterm", "en"),
        )
        .await;
    }
    control.cancel();
    result
}

fn should_disconnect_after_flow(
    shared: &ConnectionShared,
    result: &Result<(), FlowFailure>,
) -> bool {
    if shared.explicit_cleanup_requested() {
        return true;
    }
    !matches!(
        result,
        Err(FlowFailure::Network
            | FlowFailure::Channel
            | FlowFailure::Transport
            | FlowFailure::RemoteClosed)
    )
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
            expire_attachment_request(&request.command, AttachmentBlock::StaleOperation);
            continue;
        }
        // A queued SftpUpload can never be satisfied while the picker is
        // visible: there is no selected pane to fence. Reject it with a
        // stable pending reason instead of swallowing it silently below.
        expire_attachment_request(&request.command, AttachmentBlock::NotReady);
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

fn apply_layout_restore_warning(snapshot: &mut ConnectionSnapshot) {
    copy_string(
        &mut snapshot.error_code,
        &mut snapshot.error_code_len,
        "layout_restore_unconfirmed",
    );
    copy_string(
        &mut snapshot.error_message,
        &mut snapshot.error_message_len,
        &recovery_reason_message("layout_restore_unconfirmed"),
    );
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
    use russh::server::{self, Server as RusshServer};
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const KEY_ONE: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_TWO: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";
    const REJECTING_SERVER_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCNNbbvSY1uv05KifUyTIJTMcQmVLwLgoh4mdErq34PywAAAJj4uZ/y+Lmf
8gAAAAtzc2gtZWQyNTUxOQAAACCNNbbvSY1uv05KifUyTIJTMcQmVLwLgoh4mdErq34Pyw
AAAEAKNpCN3J9WmHgxbJaAqFwXWdMgDpg1y2YYi7bhOvXHaY01tu9JjW6/TkqJ9TJMglMx
xCZUvAuCiHiZ0Surfg/LAAAAFXNlcnZlckBzZXJ2ZXItTWFjbWluaQ==
-----END OPENSSH PRIVATE KEY-----";

    struct RejectingServer;

    impl RusshServer for RejectingServer {
        type Handler = Self;

        fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
            Self
        }
    }

    impl server::Handler for RejectingServer {
        type Error = russh::Error;
    }

    fn start_rejecting_ssh_server() -> (
        u16,
        server::RunningServerHandle,
        std::thread::JoinHandle<()>,
    ) {
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::spawn(move || {
            let runtime = Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .expect("rejecting SSH test runtime");
            runtime.block_on(async move {
                let listener = TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("bind rejecting SSH test server");
                let host_key = keys::decode_secret_key(REJECTING_SERVER_KEY, None)
                    .expect("decode rejecting SSH host key");
                let config = Arc::new(server::Config {
                    keys: vec![host_key],
                    auth_rejection_time: Duration::from_millis(0),
                    auth_rejection_time_initial: Some(Duration::from_millis(0)),
                    ..Default::default()
                });
                let mut server = RejectingServer;
                let running = server.run_on_socket(config, &listener);
                let handle = running.handle();
                ready_sender
                    .send((
                        listener.local_addr().expect("rejecting SSH address").port(),
                        handle,
                    ))
                    .expect("publish rejecting SSH server");
                let _ = running.await;
            });
        });
        let (port, handle) = ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("rejecting SSH server startup");
        (port, handle, join)
    }

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
    fn automatic_reconnect_keeps_selected_backend_without_picker_discovery() {
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
        assert_eq!(profile.backend, Backend::Tmux);

        let mut herdr_profile = profile.clone();
        herdr_profile.backend = Backend::Herdr;
        herdr_profile.runtime = None;
        herdr_profile.herdr_executable = Some("/home/fixture/.local/bin/herdr".into());
        assert_eq!(herdr_profile.backend, Backend::Herdr);
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
    fn reconnect_delay_starts_immediately_then_backs_off_exponentially() {
        assert_eq!(reconnect_delay(0), Duration::ZERO);
        assert_eq!(reconnect_delay(1), AUTO_RECONNECT_BASE_DELAY);
        assert_eq!(reconnect_delay(2), AUTO_RECONNECT_BASE_DELAY * 2);
        assert_eq!(reconnect_delay(3), AUTO_RECONNECT_BASE_DELAY * 4);
        assert_eq!(reconnect_delay(u32::MAX), AUTO_RECONNECT_MAX_DELAY);
    }

    #[test]
    fn transport_failure_skips_graceful_disconnect_but_explicit_cleanup_keeps_it() {
        let shared = ConnectionShared::new(
            9004,
            1,
            "example.test".to_owned(),
            22,
            PathBuf::from("/tmp/example-known-hosts"),
        );
        assert!(!should_disconnect_after_flow(
            &shared,
            &Err(FlowFailure::Transport)
        ));
        assert!(!should_disconnect_after_flow(
            &shared,
            &Err(FlowFailure::RemoteClosed)
        ));
        assert!(should_disconnect_after_flow(
            &shared,
            &Err(FlowFailure::Authentication)
        ));

        shared
            .explicit_cleanup_requested
            .store(true, Ordering::Release);
        assert!(should_disconnect_after_flow(
            &shared,
            &Err(FlowFailure::Transport)
        ));
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
    fn explicit_retirement_promotes_surviving_record_after_finished_actor() {
        let cases = [
            (1_021, true, ZoomCleanupOutcome::NotNeeded),
            (1_022, false, ZoomCleanupOutcome::NotNeeded),
            (1_023, false, ZoomCleanupOutcome::RestoredConfirmed),
        ];
        for (index, unconfirmed, prior_outcome) in cases {
            let owner = registry::create_terminal(80, 24).expect("retirement owner terminal");
            let generation = next_generation();
            let shared = Arc::new(ConnectionShared::new(
                owner,
                generation,
                "retirement.example.test".to_owned(),
                22,
                PathBuf::from("/tmp/retirement-known-hosts"),
            ));
            let runtime_identity = tmux::SessionEpoch {
                session_id: "$21".to_owned(),
                server_pid: 21,
                server_start_time: 2_100_000_000,
            };
            let hooks = tmux::ZoomRecoveryHookAllocation { index };
            {
                let mut state = shared.session.lock().expect("retirement state");
                state.generation = generation;
                state.endpoint = Some(SessionEndpoint {
                    host: "retirement.example.test".to_owned(),
                    port: 22,
                    username: "fixture".to_owned(),
                    known_hosts_path: PathBuf::from("/tmp/retirement-known-hosts"),
                    backend: Backend::Tmux,
                    runtime: Some("$21".to_owned()),
                });
            }
            assert!(shared.record_zoom_cleanup_intent(runtime_identity.clone(), 23, 41, hooks));
            if !unconfirmed {
                shared
                    .session
                    .lock()
                    .expect("confirmed retirement state")
                    .zoom_cleanup_record
                    .as_mut()
                    .expect("retirement record")
                    .unconfirmed = false;
            }
            shared.record_zoom_cleanup(prior_outcome);
            // The old actor has already finished, so the normal shutdown
            // helper reports its prior outcome and cannot perform cleanup.
            shared.finish(Ok(()));
            let abort = runtime()
                .expect("retirement runtime")
                .spawn(std::future::pending::<()>())
                .abort_handle();
            let shutdown = finish_or_force_explicit_shutdown(
                runtime().expect("retirement runtime"),
                Arc::clone(&shared),
                abort,
            );
            assert!(shutdown.finished);
            assert_eq!(shutdown.cleanup, prior_outcome);

            assert!(retire_explicit_cleanup_result(&shared, shutdown, false));
            assert!(
                shared
                    .zoom_cleanup_record_for(&runtime_identity)
                    .ok()
                    .flatten()
                    .is_none()
            );
            let snapshot = shared.snapshot().expect("retirement warning snapshot");
            assert_eq!(
                &snapshot.error_code[..usize::from(snapshot.error_code_len)],
                b"layout_restore_unconfirmed"
            );
            let no_binding_snapshot =
                connection_snapshot(owner).expect("warning survives missing replacement");
            assert_eq!(
                &no_binding_snapshot.error_code[..usize::from(no_binding_snapshot.error_code_len)],
                b"layout_restore_unconfirmed"
            );
            assert!(
                shared
                    .session
                    .lock()
                    .expect("retirement warning state")
                    .cleanup_warning
                    .is_some()
            );
            registry::destroy_terminal(owner);
        }
    }

    #[test]
    fn replacement_auth_failure_keeps_old_cleanup_warning_in_workspace_json() {
        let (port, server_handle, server_join) = start_rejecting_ssh_server();
        let owner = registry::create_terminal(80, 24).expect("owner transition terminal");
        let old_generation = next_generation();
        let old_shared = Arc::new(ConnectionShared::new(
            owner,
            old_generation,
            "127.0.0.1".to_owned(),
            port,
            PathBuf::from(format!("/tmp/replacement-auth-{owner}-known-hosts")),
        ));
        let runtime_identity = tmux::SessionEpoch {
            session_id: "$30".to_owned(),
            server_pid: 30,
            server_start_time: 3_000_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_034 };
        let known_hosts_path = PathBuf::from(format!("/tmp/replacement-auth-{owner}-known-hosts"));
        let host_key = keys::decode_secret_key(REJECTING_SERVER_KEY, None)
            .expect("decode rejecting host key for known-hosts");
        let public_key = host_key
            .public_key()
            .to_openssh()
            .expect("encode rejecting host key");
        fs::write(
            &known_hosts_path,
            format!("[127.0.0.1]:{port} {public_key}\n"),
        )
        .expect("write rejecting host known-hosts");
        {
            let mut state = old_shared.session.lock().expect("owner transition state");
            state.generation = old_generation;
            state.endpoint = Some(SessionEndpoint {
                host: "127.0.0.1".to_owned(),
                port,
                username: "fixture".to_owned(),
                known_hosts_path: known_hosts_path.clone(),
                backend: Backend::Tmux,
                runtime: Some(runtime_identity.session_id.clone()),
            });
        }
        assert!(old_shared.record_zoom_cleanup_intent(runtime_identity.clone(), 23, 41, hooks,));
        // The finished actor is the explicit owner-retirement precondition;
        // start_connection must now retire this record before installing the
        // replacement generation.
        old_shared.finish(Ok(()));
        let old_abort = runtime()
            .expect("owner transition runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections()
            .lock()
            .expect("owner transition registry")
            .insert(
                owner,
                ConnectionEntry {
                    shared: Arc::clone(&old_shared),
                    abort: old_abort,
                },
            );

        connect_terminal(
            owner,
            ConnectOptions {
                host: "127.0.0.1".to_owned(),
                port,
                username: "fixture".to_owned(),
                credentials: AuthOptions::password("wrong-password".to_owned()),
                known_hosts_path: known_hosts_path.clone(),
                backend: Backend::Tmux,
                runtime: None,
            },
        )
        .expect("start rejecting replacement");
        let replacement = connections()
            .lock()
            .expect("replacement registry")
            .get(&owner)
            .map(|entry| Arc::clone(&entry.shared))
            .expect("replacement shared owner");

        // Read the native actor directly until auth has failed. The public
        // snapshot APIs are intentionally not read before this point, so the
        // first published read observes both independent result channels.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let finished = replacement.info.lock().expect("replacement info").finished;
            if finished {
                break;
            }
            assert!(Instant::now() < deadline, "replacement auth did not fail");
            std::thread::sleep(Duration::from_millis(2));
        }
        let connection = connection_snapshot(owner).expect("replacement connection snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"auth_failed"
        );
        let first: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("replacement workspace snapshot"),
        )
        .expect("replacement workspace JSON");
        let warning = first["control"]["cleanupWarning"].clone();
        assert_eq!(warning["code"], "layout_restore_unconfirmed");
        assert!(warning["id"].as_str().is_some_and(|id| !id.is_empty()));
        let second: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("replacement repeated workspace snapshot"),
        )
        .expect("replacement repeated workspace JSON");
        assert_eq!(second["control"]["cleanupWarning"]["id"], warning["id"]);
        assert_eq!(
            replacement
                .info
                .lock()
                .expect("replacement auth info")
                .error_code,
            "auth_failed"
        );

        server_handle.shutdown("replacement auth test complete".to_owned());
        server_join.join().expect("rejoining rejecting SSH server");
        terminal_destroyed(owner);
        registry::destroy_terminal(owner);
        let _ = fs::remove_file(known_hosts_path);
    }

    #[test]
    fn credential_preparation_failure_keeps_old_cleanup_warning_in_workspace_json() {
        let owner = registry::create_terminal(80, 24).expect("credential-prep owner terminal");
        let known_hosts_path = PathBuf::from(format!("/tmp/credential-prep-{owner}-known-hosts"));
        let old_shared = ConnectionShared::new(
            owner,
            0,
            "credential-prep-old.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        );
        // Seed the old operation without installing an active binding. The
        // real connect_host path below must retain this warning while its
        // non-empty but invalid key fails during credential preparation.
        old_shared.publish_layout_restore_warning();
        let initial: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("credential-prep initial warning"),
        )
        .expect("credential-prep initial warning JSON");

        connect_host(
            owner,
            ConnectOptions {
                host: "credential-prep.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                credentials: AuthOptions::public_key(
                    "not-a-private-key-but-non-empty".to_owned(),
                    None,
                ),
                known_hosts_path: known_hosts_path.clone(),
                backend: Backend::Tmux,
                runtime: None,
            },
        )
        .expect("start credential-prep failure");
        let replacement = connections()
            .lock()
            .expect("credential-prep registry")
            .get(&owner)
            .map(|entry| Arc::clone(&entry.shared))
            .expect("credential-prep replacement shared");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let finished = replacement
                .info
                .lock()
                .expect("credential-prep info")
                .finished;
            if finished {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "credential preparation did not fail"
            );
            std::thread::sleep(Duration::from_millis(2));
        }

        let connection = connection_snapshot(owner).expect("credential-prep connection snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"key_file"
        );
        let first: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("credential-prep workspace snapshot"),
        )
        .expect("credential-prep workspace JSON");
        assert_eq!(
            first["control"]["cleanupWarning"]["id"],
            initial["control"]["cleanupWarning"]["id"]
        );
        let second: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("credential-prep repeated workspace snapshot"),
        )
        .expect("credential-prep repeated workspace JSON");
        assert_eq!(
            second["control"]["cleanupWarning"]["id"],
            first["control"]["cleanupWarning"]["id"]
        );
        assert_eq!(
            replacement
                .info
                .lock()
                .expect("credential-prep final info")
                .error_code,
            "key_file"
        );
        assert!(
            session_state(owner).lock().is_ok(),
            "session state was poisoned"
        );

        terminal_destroyed(owner);
        registry::destroy_terminal(owner);
        let _ = fs::remove_file(known_hosts_path);
    }

    #[test]
    fn cleanup_warning_survives_preparation_cancel_ready_and_stale_generation() {
        let owner = registry::create_terminal(80, 24).expect("warning lifecycle terminal");
        let generation = next_generation();
        let known_hosts_path = PathBuf::from(format!("/tmp/warning-lifecycle-{owner}"));
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "warning-lifecycle.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        ));
        shared
            .session
            .lock()
            .expect("warning lifecycle session")
            .generation = generation;
        shared.publish_layout_restore_warning_for(Some(CleanupWarningResult { result_id: 7 }));
        let initial: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("initial warning workspace JSON"),
        )
        .expect("initial warning JSON");
        assert_eq!(initial["control"]["cleanupWarning"]["id"], "1");

        let options = ConnectOptions {
            host: "warning-lifecycle.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            credentials: AuthOptions::password("fixture-only".to_owned()),
            known_hosts_path: known_hosts_path.clone(),
            backend: Backend::Tmux,
            runtime: None,
        };
        let _ = prepare_host_endpoint(owner, &options).expect("prepare host endpoint");
        let after_prepare: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("prepared warning workspace JSON"),
        )
        .expect("prepared warning JSON");
        assert_eq!(
            after_prepare["control"]["cleanupWarning"]["id"],
            initial["control"]["cleanupWarning"]["id"]
        );

        // A canceled/prepared generation without a map binding must not clear
        // the owner-scoped result. This is the same fail-closed path used
        // when a replacement loses its owner ticket before map installation.
        let replacement_generation = next_generation();
        let replacement = Arc::new(ConnectionShared::new(
            owner,
            replacement_generation,
            "warning-lifecycle.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        ));
        replacement
            .session
            .lock()
            .expect("replacement warning session")
            .generation = replacement_generation;
        // Publishing the same result and reaching Ready keep one warning ID;
        // a distinct cleanup result is the only event that advances it.
        replacement.publish_layout_restore_warning_for(Some(CleanupWarningResult { result_id: 7 }));
        replacement.set_state(ConnectionState::Ready);
        let ready: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("ready warning workspace JSON"),
        )
        .expect("ready warning JSON");
        assert_eq!(ready["control"]["cleanupWarning"]["id"], "1");

        registry::begin_remote(owner, replacement_generation).expect("canceled remote binding");
        abandon_uninstalled_connection(&replacement, std::iter::empty());
        assert!(replacement.is_cancelled());
        assert!(registry::send_bytes(owner, b"cancelled-input").is_err());
        assert!(refresh_terminal(owner).is_err());
        replacement.set_state(ConnectionState::Ready);
        assert_ne!(
            replacement
                .snapshot()
                .expect("canceled replacement snapshot")
                .state,
            ConnectionState::Ready as u32
        );
        assert!(
            !session_state(owner)
                .lock()
                .expect("canceled replacement readiness")
                .runtime_operations_ready
        );
        let after_cancel: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("canceled warning workspace JSON"),
        )
        .expect("canceled warning JSON");
        assert_eq!(
            after_cancel["control"]["cleanupWarning"]["id"],
            initial["control"]["cleanupWarning"]["id"]
        );

        replacement.publish_layout_restore_warning_for(Some(CleanupWarningResult { result_id: 8 }));
        let newer: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("newer warning workspace JSON"),
        )
        .expect("newer warning JSON");
        assert_eq!(newer["control"]["cleanupWarning"]["id"], "2");

        // A completion from the old owner generation cannot overwrite the
        // newer result after the shared SessionState generation rebases. It
        // must also be rejected before a no-record result can mint an ID.
        let result_counter_before_stale = session_state(owner)
            .lock()
            .expect("stale warning counter before")
            .next_cleanup_result_id;
        shared.publish_layout_restore_warning_for(Some(CleanupWarningResult { result_id: 9 }));
        let stale_no_record = ConnectionShared::new(
            owner,
            generation,
            "warning-lifecycle.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        );
        stale_no_record.publish_layout_restore_warning();
        assert_eq!(
            session_state(owner)
                .lock()
                .expect("stale warning counter after")
                .next_cleanup_result_id,
            result_counter_before_stale
        );
        let after_stale: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("stale warning workspace JSON"),
        )
        .expect("stale warning JSON");
        assert_eq!(after_stale["control"]["cleanupWarning"]["id"], "2");
        registry::destroy_terminal(owner);
        let _ = fs::remove_file(known_hosts_path);
    }

    #[test]
    fn cleanup_warning_ids_distinguish_record_and_no_record_results() {
        let owner = registry::create_terminal(80, 24).expect("cleanup result identity terminal");
        let generation = next_generation();
        let known_hosts_path = PathBuf::from(format!("/tmp/cleanup-result-identity-{owner}"));
        let shared = ConnectionShared::new(
            owner,
            generation,
            "cleanup-result-identity.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        );
        {
            let mut state = shared
                .session
                .lock()
                .expect("cleanup result identity state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "cleanup-result-identity.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: known_hosts_path.clone(),
                backend: Backend::Tmux,
                runtime: Some("meeterm".to_owned()),
            });
        }

        // No-record result A owns one identity and every re-publication from
        // this actor keeps it.
        shared.publish_layout_restore_warning();
        let first: serde_json::Value =
            serde_json::from_str(&workspace_snapshot_json(owner).expect("first no-record warning"))
                .expect("first no-record warning JSON");
        shared.publish_layout_restore_warning();
        let first_republished: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("republished no-record warning"),
        )
        .expect("republished no-record warning JSON");
        assert_eq!(
            first["control"]["cleanupWarning"]["id"],
            first_republished["control"]["cleanupWarning"]["id"]
        );

        // A binding handoff inherits the owner-scoped warning without
        // republishing it as a new no-record failure.
        let handoff_generation = next_generation();
        {
            let session = session_state(owner);
            let mut state = session.lock().expect("cleanup handoff state");
            state.generation = handoff_generation;
        }
        let handoff = ConnectionShared::new(
            owner,
            handoff_generation,
            "cleanup-result-identity.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        );
        let after_handoff: serde_json::Value =
            serde_json::from_str(&workspace_snapshot_json(owner).expect("handed-off warning"))
                .expect("handed-off warning JSON");
        assert_eq!(
            after_handoff["control"]["cleanupWarning"]["id"],
            first["control"]["cleanupWarning"]["id"]
        );

        // Record-backed result B receives a different owner-scoped internal
        // identity, even though the visible warning is still single-valued.
        let runtime_identity = tmux::SessionEpoch {
            session_id: "$identity".to_owned(),
            server_pid: 44,
            server_start_time: 4_400_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_044 };
        assert!(handoff.record_zoom_cleanup_intent(runtime_identity.clone(), 23, 41, hooks));
        let record_result = handoff
            .session
            .lock()
            .expect("record-backed warning state")
            .zoom_cleanup_record
            .as_ref()
            .map(|record| CleanupWarningResult {
                result_id: record.result_id,
            })
            .expect("record-backed cleanup result");
        handoff.publish_layout_restore_warning_for(Some(record_result));
        let second: serde_json::Value =
            serde_json::from_str(&workspace_snapshot_json(owner).expect("record-backed warning"))
                .expect("record-backed warning JSON");
        assert_ne!(
            first["control"]["cleanupWarning"]["id"],
            second["control"]["cleanupWarning"]["id"]
        );

        // Discarding the durable record does not turn B into a new no-record
        // result on the same explicit-retirement actor.
        assert!(handoff.discard_zoom_cleanup_record());
        handoff.publish_layout_restore_warning();
        let second_republished: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("discarded record warning"),
        )
        .expect("discarded record warning JSON");
        assert_eq!(
            second["control"]["cleanupWarning"]["id"],
            second_republished["control"]["cleanupWarning"]["id"]
        );

        // A new binding with a new no-record result gets a new identity. A
        // second no-record actor gets another one, so None never aliases all
        // no-record outcomes.
        let next_binding_generation = next_generation();
        {
            let session = session_state(owner);
            let mut state = session.lock().expect("next cleanup binding state");
            state.generation = next_binding_generation;
        }
        let next = ConnectionShared::new(
            owner,
            next_binding_generation,
            "cleanup-result-identity.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        );
        next.publish_layout_restore_warning();
        let third: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("second no-record warning"),
        )
        .expect("second no-record warning JSON");
        assert_ne!(
            second_republished["control"]["cleanupWarning"]["id"],
            third["control"]["cleanupWarning"]["id"]
        );
        let another_binding_generation = next_generation();
        {
            let session = session_state(owner);
            let mut state = session.lock().expect("another cleanup binding state");
            state.generation = another_binding_generation;
        }
        let another = ConnectionShared::new(
            owner,
            another_binding_generation,
            "cleanup-result-identity.example.test".to_owned(),
            22,
            known_hosts_path.clone(),
        );
        another.publish_layout_restore_warning();
        let fourth: serde_json::Value =
            serde_json::from_str(&workspace_snapshot_json(owner).expect("third no-record warning"))
                .expect("third no-record warning JSON");
        assert_ne!(
            third["control"]["cleanupWarning"]["id"],
            fourth["control"]["cleanupWarning"]["id"]
        );

        registry::destroy_terminal(owner);
        let _ = fs::remove_file(known_hosts_path);
    }

    #[test]
    fn ready_commit_boundary_keeps_unobserved_cleanup_warning() {
        let (owner, shared) = recovery_fixture();
        shared.publish_layout_restore_warning();
        let before: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("ready-commit warning before"),
        )
        .expect("ready-commit warning before JSON");
        let expected_epoch = shared.operation_epoch();

        assert!(
            shared
                .commit_ready_at_epoch_result(expected_epoch, |_| Ok(()))
                .is_ok(),
            "production Ready commit boundary"
        );

        assert_eq!(
            shared
                .info
                .lock()
                .expect("ready-commit connection info")
                .state,
            ConnectionState::Ready
        );
        let state = session_state(owner);
        let state = state.lock().expect("ready-commit session state");
        assert!(state.runtime_operations_ready);
        drop(state);
        let after: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("ready-commit warning after"),
        )
        .expect("ready-commit warning after JSON");
        assert_eq!(
            after["control"]["cleanupWarning"]["id"],
            before["control"]["cleanupWarning"]["id"]
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn automatic_recovery_retirement_keeps_record_without_warning() {
        let owner = registry::create_terminal(80, 24).expect("automatic retirement terminal");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "automatic-retirement.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/automatic-retirement-known-hosts"),
        ));
        let runtime_identity = tmux::SessionEpoch {
            session_id: "$22".to_owned(),
            server_pid: 22,
            server_start_time: 2_200_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_024 };
        {
            let mut state = shared.session.lock().expect("automatic retirement state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "automatic-retirement.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/automatic-retirement-known-hosts"),
                backend: Backend::Tmux,
                runtime: Some("$22".to_owned()),
            });
        }
        assert!(shared.record_zoom_cleanup_intent(runtime_identity.clone(), 23, 41, hooks));
        shared.finish(Err(FlowFailure::Transport));
        let abort = runtime()
            .expect("automatic retirement runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        let shutdown = finish_or_force_explicit_shutdown(
            runtime().expect("automatic retirement runtime"),
            Arc::clone(&shared),
            abort,
        );
        assert!(!retire_explicit_cleanup_result(&shared, shutdown, true));
        assert!(
            shared
                .zoom_cleanup_record_for(&runtime_identity)
                .ok()
                .flatten()
                .is_some()
        );
        let snapshot = shared.snapshot().expect("automatic recovery snapshot");
        assert_ne!(
            &snapshot.error_code[..usize::from(snapshot.error_code_len)],
            b"layout_restore_unconfirmed"
        );
        registry::destroy_terminal(owner);
    }

    #[test]
    fn authoritative_record_clearance_does_not_publish_retirement_warning() {
        let owner = registry::create_terminal(80, 24).expect("cleared retirement terminal");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            owner,
            generation,
            "cleared-retirement.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/cleared-retirement-known-hosts"),
        ));
        let runtime_identity = tmux::SessionEpoch {
            session_id: "$23".to_owned(),
            server_pid: 23,
            server_start_time: 2_300_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_025 };
        {
            let mut state = shared.session.lock().expect("cleared retirement state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "cleared-retirement.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/cleared-retirement-known-hosts"),
                backend: Backend::Tmux,
                runtime: Some("$23".to_owned()),
            });
        }
        assert!(shared.record_zoom_cleanup_intent(runtime_identity.clone(), 23, 41, hooks));
        assert!(shared.clear_zoom_cleanup_record(&runtime_identity, 23, hooks));
        shared.record_zoom_cleanup(ZoomCleanupOutcome::RestoredConfirmed);
        shared.finish(Ok(()));
        let abort = runtime()
            .expect("cleared retirement runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        let shutdown = finish_or_force_explicit_shutdown(
            runtime().expect("cleared retirement runtime"),
            Arc::clone(&shared),
            abort,
        );
        assert!(!retire_explicit_cleanup_result(&shared, shutdown, false));
        assert_eq!(
            shared
                .snapshot()
                .expect("cleared retirement snapshot")
                .error_code_len,
            0
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
    fn zoom_cleanup_record_survives_active_ownership_clear_and_rejects_overwrite() {
        let owner = registry::create_terminal(80, 24).expect("durable cleanup terminal");
        let generation = next_generation();
        let shared = ConnectionShared::new(
            owner,
            generation,
            "durable-cleanup.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/durable-cleanup-known-hosts"),
        );
        {
            let mut state = shared.session.lock().expect("durable cleanup state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint {
                host: "durable-cleanup.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/durable-cleanup-known-hosts"),
                backend: Backend::Tmux,
                runtime: Some("$7".to_owned()),
            });
            state.meeterm_zoomed = true;
            state.meeterm_zoomed_window = Some(23);
            state.meeterm_zoomed_pane = Some(41);
        }
        let runtime = tmux::SessionEpoch {
            session_id: "$7".to_owned(),
            server_pid: 700,
            server_start_time: 1_700_000_000,
        };
        let first = tmux::ZoomRecoveryHookAllocation { index: 1_007 };
        assert!(shared.record_zoom_cleanup_intent(runtime.clone(), 23, 41, first));
        shared.clear_owned_zoom();
        let retained = shared
            .zoom_cleanup_record_for(&runtime)
            .ok()
            .flatten()
            .expect("retained cleanup record");
        assert!(retained.unconfirmed);
        assert_eq!(retained.window, 23);
        assert_eq!(retained.pane, 41);
        assert_eq!(retained.hooks, first);

        let replacement = tmux::ZoomRecoveryHookAllocation { index: 1_008 };
        assert!(!shared.record_zoom_cleanup_intent(runtime.clone(), 99, 41, replacement));
        let still_first = shared
            .zoom_cleanup_record_for(&runtime)
            .ok()
            .flatten()
            .expect("original record remains");
        assert_eq!(still_first.window, 23);
        assert_eq!(still_first.hooks, first);
        registry::destroy_terminal(owner);
    }

    #[test]
    fn zoom_cleanup_record_is_scoped_to_runtime_and_generation_for_late_completion() {
        let owner = registry::create_terminal(80, 24).expect("generation cleanup terminal");
        let old_generation = next_generation();
        let new_generation = old_generation + 1;
        let old_shared = ConnectionShared::new(
            owner,
            old_generation,
            "generation-cleanup.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/generation-cleanup-known-hosts"),
        );
        let endpoint = SessionEndpoint {
            host: "generation-cleanup.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/generation-cleanup-known-hosts"),
            backend: Backend::Tmux,
            runtime: Some("$8".to_owned()),
        };
        let runtime = tmux::SessionEpoch {
            session_id: "$8".to_owned(),
            server_pid: 800,
            server_start_time: 1_800_000_000,
        };
        let hooks = tmux::ZoomRecoveryHookAllocation { index: 1_009 };
        {
            let mut state = old_shared.session.lock().expect("old generation state");
            state.generation = old_generation;
            state.endpoint = Some(endpoint.clone());
        }
        assert!(old_shared.record_zoom_cleanup_intent(runtime.clone(), 31, 51, hooks));

        // Model the atomic generation rebase performed only for an exact
        // same-runtime automatic reconnect. The old actor still fails its
        // generation gate and cannot clear the new actor's record.
        {
            let mut state = old_shared.session.lock().expect("rebased generation state");
            state.generation = new_generation;
            state
                .zoom_cleanup_record
                .as_mut()
                .expect("record before rebase")
                .generation = new_generation;
        }
        let new_shared = ConnectionShared::new(
            owner,
            new_generation,
            "generation-cleanup.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/generation-cleanup-known-hosts"),
        );
        assert!(
            new_shared
                .zoom_cleanup_record_for(&runtime)
                .ok()
                .flatten()
                .is_some()
        );
        assert!(!old_shared.clear_zoom_cleanup_record(&runtime, 31, hooks));
        assert!(new_shared.clear_zoom_cleanup_record(&runtime, 31, hooks));
        assert!(
            new_shared
                .zoom_cleanup_record_for(&runtime)
                .ok()
                .flatten()
                .is_none()
        );

        // A different server epoch cannot inherit the saved authority.
        let mismatched_runtime = tmux::SessionEpoch {
            session_id: "$8".to_owned(),
            server_pid: 801,
            server_start_time: 1_800_000_000,
        };
        {
            let mut state = new_shared.session.lock().expect("mismatch record state");
            state.zoom_cleanup_record = Some(ZoomCleanupRecord {
                endpoint,
                runtime: runtime.clone(),
                window: 31,
                pane: 51,
                hooks,
                result_id: 1,
                unconfirmed: true,
                generation: new_generation,
            });
        }
        assert!(matches!(
            new_shared.zoom_cleanup_record_for(&mismatched_runtime),
            Err(FlowFailure::TmuxRuntimeMissing)
        ));
        assert!(
            new_shared
                .session
                .lock()
                .expect("mismatch record retained")
                .zoom_cleanup_record
                .is_some()
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

    fn register_recovery_test_connection(shared: &Arc<ConnectionShared>) {
        let abort = runtime()
            .expect("native runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        let previous = connections().lock().expect("connection registry").insert(
            shared.terminal_id,
            ConnectionEntry {
                shared: Arc::clone(shared),
                abort,
            },
        );
        assert!(
            previous.is_none(),
            "test owner should not already be registered"
        );
    }

    fn unregister_recovery_test_connection(owner: TerminalId) {
        connections()
            .lock()
            .expect("connection registry")
            .remove(&owner)
            .expect("registered test connection")
            .abort
            .abort();
        registry::destroy_terminal(owner);
    }

    async fn wait_for_reconnect_waiter(shared: &ConnectionShared) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !shared.reconnect_waiting.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor entered the reconnect wait");
    }

    #[test]
    fn network_change_wakes_foreground_backoff_without_starting_an_actor_or_resetting_budget() {
        let test_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        test_runtime.block_on(async {
            let (owner, shared) = recovery_fixture();
            let epoch = shared
                .begin_recovery("transport", 1)
                .expect("retained recovery");
            let generation = shared.generation;
            register_recovery_test_connection(&shared);
            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_secs(30)).await }
            });
            wait_for_reconnect_waiter(&shared).await;
            network_changed();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .expect("network change wakes current retry")
                    .expect("retry task joined")
            );
            assert_eq!(shared.generation, generation);
            let state = shared.session.lock().expect("retained recovery state");
            assert_eq!(state.operation_epoch, epoch);
            assert_eq!(state.recovery.attempt, 1);
            assert_eq!(state.recovery.phase, RecoveryPhase::Reconnecting);
            drop(state);
            assert!(Arc::ptr_eq(
                &shared,
                &connections()
                    .lock()
                    .expect("connection registry")
                    .get(&owner)
                    .expect("same actor remains installed")
                    .shared
            ));
            unregister_recovery_test_connection(owner);
        });
    }

    #[test]
    fn wake_between_waiter_registration_and_its_first_check_is_not_lost() {
        let test_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        test_runtime.block_on(async {
            let (owner, shared) = recovery_fixture();
            shared
                .begin_recovery("transport", 1)
                .expect("retained recovery");
            register_recovery_test_connection(&shared);

            // No wait is registered yet: the wake leaves no permit behind.
            assert!(!shared.wake_reconnect_wait());

            // Reproduce the interleaving where the wake lands after
            // registration but before the waiter has checked anything.
            shared.reconnect_waiting.store(true, Ordering::Release);
            network_changed();
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    wait_for_registered_reconnect(&shared, Duration::from_secs(30)),
                )
                .await
                .expect("the recorded wake ends the 30s backoff immediately")
            );
            // The consumed wake leaves nothing behind: outside a wait a wake
            // is refused, so it cannot skip the next attempt's backoff.
            assert!(!shared.reconnect_waiting.load(Ordering::Acquire));
            assert!(!shared.wake_reconnect_wait());
            unregister_recovery_test_connection(owner);
        });
    }

    #[test]
    fn retry_recovery_wakes_backoff_and_is_a_noop_during_an_attempt() {
        let test_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        test_runtime.block_on(async {
            let (owner, shared) = recovery_fixture();
            let epoch = shared
                .begin_recovery("transport", 1)
                .expect("retained recovery");
            register_recovery_test_connection(&shared);
            assert_eq!(retry_recovery(owner, epoch), Ok(()));
            assert!(
                !shared.reconnect_waiting.load(Ordering::Acquire),
                "an in-progress attempt is an accepted no-op"
            );
            assert!(
                !shared.wake_reconnect_wait(),
                "no wake permit is left for the next backoff"
            );

            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_secs(30)).await }
            });
            wait_for_reconnect_waiter(&shared).await;
            assert_eq!(
                retry_recovery(owner, epoch.saturating_add(1)),
                Err(ConnectionError::RecoveryStale)
            );
            assert_eq!(retry_recovery(owner, epoch), Ok(()));
            assert!(
                tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .expect("manual Retry wakes current backoff")
                    .expect("retry task joined")
            );
            let state = shared.session.lock().expect("retained recovery state");
            assert_eq!(state.recovery.attempt, 1);
            assert_eq!(state.operation_epoch, epoch);
            drop(state);
            unregister_recovery_test_connection(owner);
        });
    }

    #[test]
    fn network_change_is_ignored_in_background_when_disabled_or_after_cancel() {
        let test_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        test_runtime.block_on(async {
            let (owner, shared) = recovery_fixture();
            shared
                .begin_recovery("transport", 1)
                .expect("retained recovery");
            register_recovery_test_connection(&shared);
            set_foreground(owner, false).expect("background recovery");
            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_secs(30)).await }
            });
            wait_for_reconnect_waiter(&shared).await;
            network_changed();
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(
                !waiting.is_finished(),
                "background network changes do not retry"
            );
            assert!(
                shared.reconnect_waiting.load(Ordering::Acquire),
                "a background wake is not recorded"
            );
            set_foreground(owner, true).expect("foreground resumes recovery");
            assert!(
                tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .expect("foreground wakes retry")
                    .expect("retry task joined")
            );
            unregister_recovery_test_connection(owner);

            let (owner, shared) = recovery_fixture();
            shared
                .begin_recovery("transport", 1)
                .expect("disabled recovery");
            register_recovery_test_connection(&shared);
            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_secs(30)).await }
            });
            wait_for_reconnect_waiter(&shared).await;
            shared.set_automatic_reconnect(false);
            assert!(!waiting.await.expect("disabled retry task joined"));
            network_changed();
            assert!(!shared.wake_reconnect_wait());
            unregister_recovery_test_connection(owner);

            let (owner, shared) = recovery_fixture();
            let epoch = shared
                .begin_recovery("transport", 1)
                .expect("cancelled recovery");
            register_recovery_test_connection(&shared);
            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_secs(30)).await }
            });
            wait_for_reconnect_waiter(&shared).await;
            shared.cancel();
            network_changed();
            assert!(!waiting.await.expect("cancelled retry task joined"));
            assert!(!shared.wake_reconnect_wait());
            assert_eq!(
                retry_recovery(owner, epoch),
                Err(ConnectionError::RecoveryUnavailable)
            );
            unregister_recovery_test_connection(owner);
        });
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
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert!(!shared.current_terminal_input_is_ready(shared.operation_epoch()));
        shared.finish(Err(FlowFailure::HerdrSessionMissing));
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
    fn foreground_return_skips_remaining_reconnect_backoff_without_resetting_attempt() {
        let test_runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        test_runtime.block_on(async {
            let (owner, shared) = recovery_fixture();
            shared
                .begin_recovery("transport", 1)
                .expect("recovery epoch");
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

            set_foreground(owner, false).expect("background before retry wait");
            let waiting = tokio::spawn({
                let shared = Arc::clone(&shared);
                async move { wait_for_reconnect(&shared, Duration::from_secs(30)).await }
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                while !shared.reconnect_waiting.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("retry actor entered backoff wait");
            set_foreground(owner, true).expect("foreground wakes retry");
            assert!(
                tokio::time::timeout(Duration::from_secs(1), waiting)
                    .await
                    .expect("foreground returned retry immediately")
                    .expect("retry task joined")
            );
            let state = shared.session.lock().expect("retained recovery state");
            assert_eq!(state.recovery.attempt, 1);
            assert_eq!(state.recovery.phase, RecoveryPhase::Reconnecting);
            drop(state);

            connections()
                .lock()
                .expect("connection registry")
                .remove(&owner)
                .expect("foreground retry connection")
                .abort
                .abort();
            registry::destroy_terminal(owner);
        });
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
    fn change_runtime_reports_pre_boundary_rejections_and_injected_post_boundary_failure() {
        let (owner, shared) = recovery_fixture();
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

        let initial_epoch = shared.operation_epoch();
        let stale = change_runtime_with_start(owner, initial_epoch + 1, |_, _| {
            panic!("stale epoch must not start a replacement")
        });
        assert_eq!(
            stale,
            RuntimeBoundaryOutcome::RejectedBeforeBoundary(ConnectionError::RecoveryStale)
        );
        assert_eq!(stale.bridge_code(), ConnectionError::RecoveryStale.code());
        assert_eq!(shared.operation_epoch(), initial_epoch);
        assert!(!shared.explicit_cleanup_requested());

        let missing_profile = change_runtime_with_start(owner, initial_epoch, |_, _| {
            panic!("missing profile must not start a replacement")
        });
        assert_eq!(
            missing_profile,
            RuntimeBoundaryOutcome::RejectedBeforeBoundary(ConnectionError::RecoveryUnavailable)
        );
        assert_eq!(shared.operation_epoch(), initial_epoch);
        assert!(!shared.explicit_cleanup_requested());

        shared.set_profile(retry_profile());
        let failed = change_runtime_with_start(owner, initial_epoch, |_, start| {
            assert!(matches!(start, ConnectionStart::ManualReconnect(_)));
            // This deliberately shares a code with the pre-boundary profile
            // rejection above. The actual lifecycle path must retain the
            // post-boundary classification despite that duplicate error.
            Err(ConnectionError::RecoveryUnavailable)
        });
        assert_eq!(
            failed,
            RuntimeBoundaryOutcome::AcceptedAfterFailure(ConnectionError::RecoveryUnavailable)
        );
        assert_eq!(
            failed.bridge_code(),
            RuntimeBoundaryOutcome::ACCEPTED_AFTER_FAILURE_CODE
        );
        assert!(shared.explicit_cleanup_requested());
        assert!(shared.operation_epoch() > initial_epoch);
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Stopped);
        assert_eq!(
            shared.snapshot().expect("owner snapshot").state,
            ConnectionState::Closing as u32,
            "the retired owner is not restored to Ready after replacement start fails"
        );
        assert!(!shared.current_terminal_input_is_ready(shared.operation_epoch()));

        connections()
            .lock()
            .expect("connection registry")
            .remove(&owner)
            .expect("boundary test connection")
            .abort
            .abort();
        registry::destroy_terminal(owner);
    }

    #[test]
    fn stopped_retry_restores_retained_work_without_reenabling_automatic_retries() {
        let (owner, shared) = recovery_fixture();
        shared.set_profile(retry_profile());

        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let old_generation = shared.generation;
        shared.set_automatic_reconnect(false);
        let disposition = retry_disposition(&shared, FlowFailure::Transport);
        assert_eq!(disposition, RetryDisposition::Stop("transport".to_owned()));
        // This is the native boundary used when Ready work is lost while the
        // setting is disabled: retain the selected target, gate input, and
        // expose a stopped recovery that can still be retried explicitly.
        let RetryDisposition::Stop(reason) = disposition else {
            panic!("disabled automatic recovery must stop");
        };
        shared.stop_recovery(&reason);
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
        assert!(
            !replacement.automatic_reconnect_enabled(),
            "manual Retry must not re-enable automatic retries"
        );
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
    fn failed_retry_preparation_returns_to_public_stopped_error() {
        let (owner, shared) = recovery_fixture();
        shared.set_profile(retry_profile());
        shared
            .begin_recovery("transport", 1)
            .expect("transport recovery");
        shared.stop_recovery("retry_exhausted");
        shared.finish(Err(FlowFailure::Transport));
        register_recovery_test_connection(&shared);
        let stopped_epoch = shared.operation_epoch();

        let result = retry_recovery_with_start(owner, stopped_epoch, |target, start| {
            assert_eq!(target, owner);
            assert!(matches!(start, ConnectionStart::AutomaticReconnect(_)));
            Err(ConnectionError::RuntimeUnavailable)
        });
        assert_eq!(result, Err(ConnectionError::RuntimeUnavailable));
        let workspace: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("recovered failure snapshot"),
        )
        .expect("recovered failure JSON");
        assert_eq!(workspace["control"]["recovery"]["phase"], "stopped");
        assert_eq!(
            workspace["control"]["recovery"]["reason"],
            "runtime_unavailable"
        );
        assert_eq!(workspace["control"]["terminalInputReady"], false);
        let connection = connection_snapshot(owner).expect("replacement error snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            b"runtime_unavailable"
        );
        assert!(shared.info.lock().expect("finished owner info").finished);
        assert!(!shared.explicit_cleanup_requested());
        unregister_recovery_test_connection(owner);
    }

    #[test]
    fn finish_boundary_preserves_specific_stopped_recovery_reasons() {
        for reason in [
            "herdr_session_missing",
            "herdr_terminal_missing",
            "controller_conflict",
            "herdr_incompatible",
            "runtime_identity_uncertain",
            "retry_exhausted",
            "automatic_reconnect_disabled",
        ] {
            let (owner, shared) = recovery_fixture();
            shared
                .begin_recovery("transport", 1)
                .expect("recovery starts");
            shared.stop_recovery(reason);
            assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
            let disposition = retry_disposition(&shared, FlowFailure::Transport);
            assert_eq!(disposition, RetryDisposition::Stop(reason.to_owned()));
            stop_recovery_for_disposition(&shared, disposition);
            shared.finish(Err(FlowFailure::Transport));
            let value: serde_json::Value = serde_json::from_str(
                &workspace_snapshot_json(owner).expect("finished reason snapshot"),
            )
            .expect("finished reason JSON");
            assert_eq!(value["control"]["recovery"]["phase"], "stopped");
            assert_eq!(value["control"]["recovery"]["reason"], reason);
            registry::destroy_terminal(owner);
        }
    }

    #[test]
    fn local_recovery_classification_survives_outer_disposition_and_finish() {
        // A stopped selected Herdr runtime is classified inside recover()
        // before its FlowFailure reaches the enclosing SSH actor.
        let (owner, shared) = recovery_fixture();
        shared
            .begin_recovery("transport", 1)
            .expect("transport recovery starts");
        shared
            .session
            .lock()
            .expect("recovery session")
            .recovery
            .phase = RecoveryPhase::Resynchronizing;
        assert!(herdr_control::stage_local_recovery_failure(
            &shared,
            FlowFailure::HerdrSessionMissing
        ));
        let disposition = retry_disposition(&shared, FlowFailure::HerdrSessionMissing);
        assert_eq!(
            disposition,
            RetryDisposition::Stop("herdr_session_missing".to_owned())
        );
        stop_recovery_for_disposition(&shared, disposition);
        shared.finish(Err(FlowFailure::HerdrSessionMissing));
        assert_public_stopped_recovery(&shared, "herdr_session_missing");
        registry::destroy_terminal(owner);

        // If the retained stable terminal is already absent from a still-live
        // group, target classification stages the more specific terminal
        // reason before returning the same broad FlowFailure variant.
        let (owner, shared) = recovery_fixture();
        shared
            .begin_recovery("transport", 1)
            .expect("transport recovery starts");
        {
            let mut state = shared.session.lock().expect("recovery session");
            state.recovery_terminal_id = None;
            state.recovery_group_id = Some(7);
            state.herdr.snapshot.groups.push(workspace::TerminalGroup {
                id: "7".to_owned(),
                workspace_id: "3".to_owned(),
                name: "retained group".to_owned(),
                selected: true,
                agent_status: None,
            });
            state.herdr.panes.insert(
                701,
                herdr_control::RemotePane {
                    pane_id: "pane-other".to_owned(),
                    terminal_id: "terminal-other".to_owned(),
                    workspace: 3,
                    group: 7,
                },
            );
        }
        assert!(matches!(
            herdr_control::recovery_target_or_stop(&shared),
            Err(FlowFailure::HerdrSessionMissing)
        ));
        let disposition = retry_disposition(&shared, FlowFailure::HerdrSessionMissing);
        assert_eq!(
            disposition,
            RetryDisposition::Stop("herdr_terminal_missing".to_owned())
        );
        stop_recovery_for_disposition(&shared, disposition);
        shared.finish(Err(FlowFailure::HerdrSessionMissing));
        assert_public_stopped_recovery(&shared, "herdr_terminal_missing");
        registry::destroy_terminal(owner);

        // A host-key callback remains higher priority than a previously
        // staged local reason, including in the public recovery snapshot.
        let (owner, shared) = recovery_fixture();
        shared
            .begin_recovery("transport", 1)
            .expect("transport recovery starts");
        shared
            .session
            .lock()
            .expect("recovery session")
            .recovery
            .phase = RecoveryPhase::Resynchronizing;
        assert!(herdr_control::stage_local_recovery_failure(
            &shared,
            FlowFailure::HerdrSessionMissing
        ));
        shared.set_changed_key(
            "SHA256/presented".to_owned(),
            "ssh-ed25519".to_owned(),
            "SHA256/known".to_owned(),
        );
        let disposition = retry_disposition(&shared, FlowFailure::Network);
        assert_eq!(
            disposition,
            RetryDisposition::Stop("host_key_changed".to_owned())
        );
        stop_recovery_for_disposition(&shared, disposition);
        shared.finish(Err(FlowFailure::Network));
        assert_public_stopped_recovery(&shared, "host_key_changed");
        let connection = shared.snapshot().expect("changed-key snapshot");
        assert_eq!(
            &connection.fingerprint[..usize::from(connection.fingerprint_len)],
            b"SHA256/presented"
        );
        registry::destroy_terminal(owner);
    }

    fn assert_public_stopped_recovery(shared: &ConnectionShared, reason: &str) {
        let value: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(shared.terminal_id).expect("public recovery snapshot"),
        )
        .expect("public recovery JSON");
        assert_eq!(value["control"]["recovery"]["phase"], "stopped");
        assert_eq!(value["control"]["recovery"]["reason"], reason);
        assert_eq!(value["control"]["terminalInputReady"], false);
        assert_eq!(value["control"]["runtimeOperationsReady"], false);

        let connection = shared.snapshot().expect("public connection snapshot");
        assert_eq!(connection.state, ConnectionState::Failed as u32);
        assert_eq!(
            &connection.error_code[..usize::from(connection.error_code_len)],
            reason.as_bytes()
        );
    }

    #[test]
    fn stopped_retry_is_published_only_after_actor_finish_then_reuses_retained_term() {
        let (owner, shared) = recovery_fixture();
        shared.set_profile(retry_profile());
        registry::begin_remote(owner, shared.generation).expect("remote terminal binding");
        let (input_sender, _input_receiver) = mpsc::channel(1);
        let (resize_sender, _resize_receiver) = watch::channel((80, 24));
        registry::prepare_pane_transport(
            owner,
            shared.generation,
            (80, 24),
            input_sender,
            resize_sender,
        )
        .expect("ready remote transport");
        assert!(registry::mark_transport_ready(owner, shared.generation));
        assert!(registry::send_bytes(owner, b"before failure").is_ok());
        shared
            .begin_recovery("transport", 1)
            .expect("transport loss starts recovery");
        let old_generation = shared.generation;
        let retained_snapshot = session_snapshot(owner).expect("retained target");
        let retained_terminal = registry::shared_terminal(owner).expect("retained Term");
        let actor_shared = Arc::clone(&shared);
        let (actor_ready_sender, actor_ready_receiver) = std::sync::mpsc::channel();
        let (finish_sender, finish_receiver) = tokio::sync::oneshot::channel();
        let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
        let old_actor = runtime().expect("native runtime").spawn(async move {
            actor_shared.stop_recovery("retry_exhausted");
            actor_ready_sender
                .send(())
                .expect("test observes failure before actor exit");
            let _ = finish_receiver.await;
            actor_shared.finish(Err(FlowFailure::Transport));
            finished_sender
                .send(())
                .expect("test observes actor finish");
        });
        actor_ready_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("actor published the detected failure before exit");
        assert!(matches!(
            registry::send_bytes(owner, b"blocked during actor exit"),
            Err(crate::terminal::TerminalError::InputNotReady)
        ));
        let previous = connections().lock().expect("connection registry").insert(
            owner,
            ConnectionEntry {
                shared: Arc::clone(&shared),
                abort: old_actor.abort_handle(),
            },
        );
        assert!(previous.is_none(), "test owner should not be registered");
        assert!(!shared.info.lock().expect("old connection info").finished);
        let pending_epoch = shared.operation_epoch();
        let in_flight =
            serde_json::from_str::<serde_json::Value>(&workspace_snapshot_json(owner).unwrap())
                .expect("in-flight public workspace snapshot");
        assert_eq!(in_flight["control"]["recovery"]["phase"], "reconnecting");
        assert_eq!(
            in_flight["control"]["recovery"]["reason"],
            "retry_exhausted"
        );
        assert_eq!(in_flight["control"]["terminalInputReady"], false);
        assert_eq!(in_flight["control"]["runtimeOperationsReady"], false);
        assert_eq!(
            connection_snapshot(owner)
                .expect("public connection snapshot")
                .state,
            ConnectionState::Failed as u32
        );
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert!(!shared.is_cancelled());
        assert!(!shared.explicit_cleanup_requested());
        assert!(
            !automatic_retry_allowed(&shared, FlowFailure::Transport),
            "a staged local stop remains retry-ineligible until the actor finishes"
        );
        assert_eq!(
            retry_recovery(owner, pending_epoch),
            Ok(()),
            "a current-epoch Workspaces reconnect during actor teardown is a no-op"
        );
        assert_eq!(
            connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .expect("old owner remains referenceable")
                .shared
                .generation,
            old_generation
        );

        finish_sender
            .send(())
            .expect("release actor finish barrier");
        finished_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("actor commits its finish boundary");
        assert!(shared.info.lock().expect("old connection info").finished);
        let stopped = serde_json::from_str::<serde_json::Value>(
            &workspace_snapshot_json(owner).expect("finished public workspace snapshot"),
        )
        .expect("finished public snapshot JSON");
        assert_eq!(stopped["control"]["recovery"]["phase"], "stopped");
        assert_eq!(stopped["control"]["recovery"]["reason"], "retry_exhausted");
        let stopped_epoch = shared.operation_epoch();
        assert!(stopped_epoch > pending_epoch);

        // Retry begins one replacement after the old actor has finished. The
        // remote target and native Term remain the same across the handoff.
        retry_recovery(owner, stopped_epoch).expect("Retry starts replacement");
        let replacement = connections()
            .lock()
            .expect("connection registry")
            .get(&owner)
            .expect("replacement actor")
            .shared
            .clone();
        assert_ne!(replacement.generation, old_generation);
        assert_eq!(replacement.recovery_phase(), RecoveryPhase::Reconnecting);
        assert!(
            !shared.is_cancelled(),
            "same-intent Retry does not cancel old owner"
        );
        assert!(!shared.explicit_cleanup_requested());
        assert_eq!(
            session_state(owner)
                .lock()
                .expect("replacement session state")
                .generation,
            replacement.generation,
            "the new generation is installed after the old actor finish boundary"
        );
        assert_eq!(
            session_snapshot(owner).expect("replacement target"),
            retained_snapshot
        );
        assert!(Arc::ptr_eq(
            &retained_terminal,
            &registry::shared_terminal(owner).expect("same Term after replacement")
        ));

        old_actor.abort();
        unregister_recovery_test_connection(owner);
    }

    #[test]
    fn retry_handoff_keeps_public_owner_and_duplicate_current_epoch_is_noop() {
        let (owner, shared) = recovery_fixture();
        shared.set_profile(retry_profile());
        shared
            .begin_recovery("transport", 1)
            .expect("transport recovery");
        shared.stop_recovery("retry_exhausted");
        shared.finish(Err(FlowFailure::Transport));
        let stopped_epoch = shared.operation_epoch();
        let old_generation = shared.generation;
        register_recovery_test_connection(&shared);

        // Hold the per-owner start lock. Retry has committed Reconnecting but
        // cannot enter connection preparation yet, making the public handoff
        // interval deterministic for concurrent snapshot and Retry callers.
        let owner_state = owner_transition(owner).expect("owner transition");
        let serial = owner_state.serial.lock().expect("owner serial lock");
        let (retry_sender, retry_receiver) = std::sync::mpsc::channel();
        let retry_thread = std::thread::spawn(move || {
            retry_sender
                .send(retry_recovery(owner, stopped_epoch))
                .expect("return retry result");
        });

        let handoff_deadline = Instant::now() + Duration::from_secs(1);
        let handoff_epoch = loop {
            let value: serde_json::Value = serde_json::from_str(
                &workspace_snapshot_json(owner).expect("handoff workspace snapshot"),
            )
            .expect("handoff snapshot JSON");
            let epoch = value["control"]["operationEpoch"]
                .as_str()
                .expect("decimal operation epoch")
                .parse::<u64>()
                .expect("valid operation epoch");
            if value["control"]["recovery"]["phase"] == "reconnecting" && epoch != stopped_epoch {
                break epoch;
            }
            assert!(Instant::now() < handoff_deadline, "Retry entered handoff");
            std::thread::yield_now();
        };
        for _ in 0..3 {
            let snapshot = connection_snapshot(owner).expect("connection remains public");
            assert_eq!(snapshot.state, ConnectionState::Reconnecting as u32);
            let value: serde_json::Value = serde_json::from_str(
                &workspace_snapshot_json(owner).expect("workspace remains public"),
            )
            .expect("public workspace JSON");
            assert_eq!(value["control"]["recovery"]["phase"], "reconnecting");
            assert_ne!(value["control"]["recovery"]["reason"], "runtime_replaced");
        }
        assert_eq!(
            retry_recovery(owner, handoff_epoch),
            Ok(()),
            "a duplicate Retry at the current epoch is an accepted no-op"
        );
        assert!(Arc::ptr_eq(
            &connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .expect("old entry retained until commit")
                .shared,
            &shared
        ));
        assert!(!shared.is_cancelled());
        assert!(!shared.explicit_cleanup_requested());

        drop(serial);
        assert_eq!(
            retry_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("Retry completes after release"),
            Ok(())
        );
        retry_thread.join().expect("Retry worker joins");
        let replacement = connections()
            .lock()
            .expect("connection registry")
            .get(&owner)
            .expect("single committed replacement")
            .shared
            .clone();
        assert_ne!(replacement.generation, old_generation);
        assert!(!shared.is_cancelled());
        assert!(!shared.explicit_cleanup_requested());
        assert_eq!(
            session_state(owner)
                .lock()
                .expect("handoff session state")
                .generation,
            replacement.generation
        );
        unregister_recovery_test_connection(owner);
    }

    #[test]
    fn stopped_retry_stays_rejected_after_disconnect_or_change() {
        for boundary in ["explicit_disconnect", "runtime_changed"] {
            let (owner, shared) = recovery_fixture();
            shared.set_profile(retry_profile());
            shared
                .begin_recovery("transport", 1)
                .expect("recovery starts before actor finish");
            shared.stop_recovery("retry_exhausted");
            shared.finish(Err(FlowFailure::Transport));
            register_recovery_test_connection(&shared);

            let stopped_epoch = shared.operation_epoch();
            if boundary == "explicit_disconnect" {
                assert_eq!(
                    disconnect_with_boundary(owner),
                    RuntimeBoundaryOutcome::Accepted
                );
            } else {
                assert_eq!(
                    change_runtime_with_start(owner, stopped_epoch, |_, _| Ok(())),
                    RuntimeBoundaryOutcome::Accepted
                );
            }
            let current_epoch = shared.operation_epoch();
            assert!(shared.explicit_cleanup_requested());
            assert_eq!(shared.recovery_phase(), RecoveryPhase::Stopped);
            assert_eq!(
                shared
                    .snapshot()
                    .expect("finished owner after explicit boundary")
                    .state,
                ConnectionState::Disconnected as u32,
                "a finished owner is never republished as Closing"
            );
            assert_eq!(
                retry_recovery(owner, current_epoch),
                Err(ConnectionError::RecoveryUnavailable),
                "Retry must not reopen an explicit {boundary} boundary"
            );

            unregister_recovery_test_connection(owner);
        }
    }

    #[test]
    fn explicit_disconnect_or_change_wins_during_finished_retry_handoff() {
        for boundary in ["explicit_disconnect", "runtime_changed"] {
            let (owner, shared) = recovery_fixture();
            shared.set_profile(retry_profile());
            shared
                .begin_recovery("transport", 1)
                .expect("recovery starts");
            shared.stop_recovery("retry_exhausted");
            shared.finish(Err(FlowFailure::Transport));
            let stopped_epoch = shared.operation_epoch();
            let old_generation = shared.generation;
            register_recovery_test_connection(&shared);

            // Block Retry before it can publish an owner ticket, then accept
            // an explicit boundary after Retry has changed the public state
            // to Reconnecting. The explicit intent must prevent that delayed
            // same-intent request from installing a replacement.
            let owner_state = owner_transition(owner).expect("owner transition");
            let serial = owner_state.serial.lock().expect("owner serial lock");
            let (retry_sender, retry_receiver) = std::sync::mpsc::channel();
            let retry_thread = std::thread::spawn(move || {
                retry_sender
                    .send(retry_recovery(owner, stopped_epoch))
                    .expect("return retry result");
            });
            let handoff_deadline = Instant::now() + Duration::from_secs(1);
            let handoff_epoch = loop {
                let value: serde_json::Value = serde_json::from_str(
                    &workspace_snapshot_json(owner).expect("handoff snapshot"),
                )
                .expect("handoff snapshot JSON");
                let epoch = value["control"]["operationEpoch"]
                    .as_str()
                    .expect("operation epoch")
                    .parse::<u64>()
                    .expect("valid operation epoch");
                if value["control"]["recovery"]["phase"] == "reconnecting" && epoch != stopped_epoch
                {
                    break epoch;
                }
                assert!(Instant::now() < handoff_deadline, "Retry entered handoff");
                std::thread::yield_now();
            };

            if boundary == "explicit_disconnect" {
                assert_eq!(
                    disconnect_with_boundary(owner),
                    RuntimeBoundaryOutcome::Accepted
                );
            } else {
                assert_eq!(
                    change_runtime_with_start(owner, handoff_epoch, |_, _| Ok(())),
                    RuntimeBoundaryOutcome::Accepted
                );
            }
            drop(serial);
            assert_eq!(
                retry_receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("Retry exits after explicit boundary"),
                Err(ConnectionError::RecoveryUnavailable)
            );
            retry_thread.join().expect("Retry worker joins");

            assert!(shared.explicit_cleanup_requested());
            assert_eq!(shared.recovery_phase(), RecoveryPhase::Stopped);
            assert_eq!(
                shared.snapshot().expect("explicit owner snapshot").state,
                ConnectionState::Disconnected as u32
            );
            assert_eq!(
                connections()
                    .lock()
                    .expect("connection registry")
                    .get(&owner)
                    .expect("old owner remains until an explicit replacement is installed")
                    .shared
                    .generation,
                old_generation
            );
            assert_eq!(
                retry_recovery(owner, shared.operation_epoch()),
                Err(ConnectionError::RecoveryUnavailable),
                "Retry after explicit intent cannot revive the retained owner"
            );
            unregister_recovery_test_connection(owner);
        }
    }

    #[test]
    fn retry_during_an_active_attempt_is_an_accepted_noop() {
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
        let generation = shared.generation;
        assert_eq!(retry_recovery(owner, epoch), Ok(()));
        assert_eq!(shared.generation, generation);
        assert!(!shared.reconnect_waiting.load(Ordering::Acquire));
        assert!(Arc::ptr_eq(
            &shared,
            &connections()
                .lock()
                .expect("connection registry")
                .get(&owner)
                .expect("unchanged active actor")
                .shared
        ));
        unregister_recovery_test_connection(owner);
    }

    #[test]
    fn changed_host_key_stops_retained_recovery_without_retry_and_keeps_cleanup_warning() {
        let (owner, shared) = recovery_fixture();
        let retained_snapshot = session_snapshot(owner).expect("retained snapshot");
        let retained_terminal = registry::shared_terminal(owner).expect("retained terminal");
        let before_epoch = shared.operation_epoch();
        shared.publish_layout_restore_warning();
        let warning_before: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("changed-key warning before"),
        )
        .expect("changed-key warning before JSON");

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
        assert_eq!(
            disposition,
            RetryDisposition::Stop("host_key_changed".to_owned())
        );
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
            shared.stop_recovery(&reason);
        } else {
            panic!("changed host key must not be retried");
        }
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert!(!shared.current_terminal_input_is_ready(shared.operation_epoch()));
        shared.finish(Err(FlowFailure::Network));

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
        let warning_after: serde_json::Value = serde_json::from_str(
            &workspace_snapshot_json(owner).expect("changed-key warning after"),
        )
        .expect("changed-key warning after JSON");
        assert_eq!(
            warning_after["control"]["cleanupWarning"]["id"],
            warning_before["control"]["cleanupWarning"]["id"]
        );
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
        assert_eq!(
            disposition,
            RetryDisposition::Stop("authentication_failed".to_owned())
        );
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
            shared.stop_recovery(&reason);
        } else {
            panic!("authentication failure must not be retried");
        }
        assert_eq!(shared.recovery_phase(), RecoveryPhase::Reconnecting);
        assert!(!shared.current_terminal_input_is_ready(shared.operation_epoch()));
        shared.finish(Err(FlowFailure::Network));

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
        let state = shared.session.lock().expect("recovery session state");
        assert_eq!(state.recovery.phase, RecoveryPhase::Reconnecting);
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
