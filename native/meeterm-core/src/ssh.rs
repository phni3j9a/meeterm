//! Rust-owned SSH lifecycle and host-key trust policy.
//!
//! This module deliberately exposes a small control plane.  Terminal bytes
//! remain in the registry and are exchanged with russh through bounded native
//! queues; callers poll the fixed connection snapshot and terminal revision.

mod control;
mod herdr_control;

use std::collections::{HashMap, HashSet};
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
    BrowseUnavailable,
    BrowseStale,
    BrowseAlreadyActive,
    BrowsePermissionDenied,
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
            Self::BrowseUnavailable => -15,
            Self::BrowseStale => -16,
            Self::BrowseAlreadyActive => -17,
            Self::BrowsePermissionDenied => -18,
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
            Self::BrowseUnavailable => "runtime_browse_unavailable",
            Self::BrowseStale => "runtime_browse_stale",
            Self::BrowseAlreadyActive => "runtime_browse_already_active",
            Self::BrowsePermissionDenied => "runtime_browse_permission_denied",
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
            Self::BrowseUnavailable => "runtime browsing is not available for this connection",
            Self::BrowseStale => "the runtime browse request is stale",
            Self::BrowseAlreadyActive => "another runtime browse is already active",
            Self::BrowsePermissionDenied => {
                "the provisional connection cannot mutate or control a runtime"
            }
        })
    }
}

impl std::error::Error for ConnectionError {}

/// Lifecycle of the short-lived host-only connection used by the session
/// switcher.  The browse token and this phase are low-frequency control-plane
/// metadata; they never carry credentials, shell targets, or Herdr paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeBrowsePhase {
    Starting,
    Discovering,
    Ready,
    Committing,
    Committed,
    /// The requested candidate was positively confirmed as the current live
    /// tmux binding, so the browse can close without changing its owner.
    Unchanged,
    Failed,
    Cancelled,
}

impl RuntimeBrowsePhase {
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Discovering => "discovering",
            Self::Ready => "ready",
            Self::Committing => "committing",
            Self::Committed => "committed",
            Self::Unchanged => "unchanged",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeBrowseHostKeySnapshot {
    pub pending: bool,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub algorithm: String,
    pub known_fingerprint: String,
}

/// Sanitized state returned for one provisional browse.  The provisional
/// native terminal handle is intentionally absent; only the successful
/// promoted source handle may be returned in `active_terminal_id`.
#[derive(Clone, Debug)]
pub struct RuntimeBrowseSnapshot {
    pub token: String,
    pub browse_generation: u64,
    pub discovery_revision: u64,
    pub phase: RuntimeBrowsePhase,
    pub discovery: RuntimeDiscoverySnapshot,
    pub error_code: String,
    pub error_message: String,
    pub host_key: RuntimeBrowseHostKeySnapshot,
    pub cleanup_warning: Option<CleanupWarning>,
    pub active_terminal_id: Option<TerminalId>,
}

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
            pending_confirmation_token: None,
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

struct ConnectionShared {
    /// The public terminal handle may be promoted from a provisional root to
    /// the source owner after a successful ordered switch. All actor-owned
    /// registry access resolves through `terminal_id()`.
    active_terminal_id: AtomicU64,
    provisional: AtomicBool,
    provisional_bind_armed: AtomicBool,
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
    /// The cleanup result identity owned by this connection actor. A record
    /// backed result remains identifiable after its record is discarded, and
    /// a no-record retirement result is minted only once for this actor.
    cleanup_warning_result: Mutex<Option<CleanupWarningResult>>,
    /// Actor-private intent is published before the Control Mode writer await
    /// and lets a forced shutdown report an unconfirmed mutation in flight.
    zoom_cleanup_pending: AtomicBool,
}

impl ConnectionShared {
    #[cfg(test)]
    fn new(
        terminal_id: TerminalId,
        generation: u64,
        host: String,
        port: u16,
        known_hosts_path: PathBuf,
    ) -> Self {
        Self::new_with_provisional(terminal_id, generation, host, port, known_hosts_path, false)
    }

    fn new_with_provisional(
        terminal_id: TerminalId,
        generation: u64,
        host: String,
        port: u16,
        known_hosts_path: PathBuf,
        provisional: bool,
    ) -> Self {
        let session = session_state(terminal_id);
        let (foreground, automatic_reconnect) = session
            .lock()
            .map(|state| (state.foreground, state.automatic_reconnect))
            .unwrap_or((true, true));
        Self {
            active_terminal_id: AtomicU64::new(terminal_id),
            provisional: AtomicBool::new(provisional),
            provisional_bind_armed: AtomicBool::new(false),
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
            cleanup_warning_result: Mutex::new(None),
            zoom_cleanup_pending: AtomicBool::new(false),
        }
    }

    fn terminal_id(&self) -> TerminalId {
        self.active_terminal_id.load(Ordering::Acquire)
    }

    fn is_provisional(&self) -> bool {
        self.provisional.load(Ordering::Acquire)
    }

    fn promote_terminal(&self, terminal_id: TerminalId) {
        self.active_terminal_id
            .store(terminal_id, Ordering::Release);
        self.provisional.store(false, Ordering::Release);
        self.provisional_bind_armed.store(false, Ordering::Release);
    }

    fn arm_provisional_bind(&self) {
        self.provisional_bind_armed.store(true, Ordering::Release);
    }

    fn normal_control_allowed(&self) -> bool {
        !self.is_provisional()
    }

    fn provisional_bind_allowed(&self) -> bool {
        self.is_provisional() && self.provisional_bind_armed.load(Ordering::Acquire)
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

    /// Atomically fence a source owner for an explicit browse commit.  The
    /// source generation and operation epoch are checked while the same
    /// `info -> session` boundary that revokes input/resize/mutation is held,
    /// so a delayed callback cannot pass a check and then publish live state.
    fn fence_for_runtime_switch(
        &self,
        expected_epoch: u64,
        expected_recovery: &RecoverySnapshot,
    ) -> Result<(), ConnectionError> {
        let mut info = self.info.lock().map_err(|_| ConnectionError::Internal)?;
        if self.is_cancelled() {
            return Err(ConnectionError::BrowseStale);
        }
        let mut state = self.session.lock().map_err(|_| ConnectionError::Internal)?;
        if state.generation != self.generation
            || state.operation_epoch != expected_epoch
            || &state.recovery != expected_recovery
            || state.runtime_operations_ready != (expected_recovery.phase == RecoveryPhase::None)
            || (expected_recovery.phase != RecoveryPhase::None && !state.has_retained_work())
        {
            return Err(ConnectionError::BrowseStale);
        }
        // Keep normal input/resize/mutation fail-closed while the source map
        // entry is being replaced by the provisional actor.
        fence_terminal_data_plane(self.terminal_id())?;
        self.explicit_cleanup_requested
            .store(true, Ordering::Release);
        state.operation_epoch = next_operation_epoch(state.operation_epoch);
        state.recovery.phase = RecoveryPhase::Stopped;
        state.recovery.reason = "runtime_changed".to_owned();
        state.recovery.confirmation_token.clear();
        state.pending_confirmation_token = None;
        state.runtime_operations_ready = false;
        state.terminal_input_ready = false;
        if !info.finished {
            info.state = ConnectionState::Closing;
        }
        info.pending = None;
        self.explicit_cleanup_notify.notify_waiters();
        Ok(())
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
                terminal_ids.push(self.terminal_id());
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

enum ControlCommand {
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
    RetryRecovery,
    ConfirmRecovery {
        token: String,
    },
    /// Internal handoff command delivered to the provisional backend actor.
    /// It is never exposed through the normal control-plane APIs; keeping the
    /// promotion on the actor's serialized command loop lets tmux/Herdr remap
    /// their private native pane handles before the next remote event.
    PromoteRuntimeBrowse {
        token: u64,
        source_owner: TerminalId,
        provisional_owner: TerminalId,
    },
}

struct ConnectionEntry {
    shared: Arc<ConnectionShared>,
    abort: tokio::task::AbortHandle,
}

/// Browse-only capability retained after a runtime switch has released its
/// source actor. It is deliberately kept outside `connections`, so ordinary
/// controller, runtime, and data-plane APIs cannot revive the old binding.
#[derive(Clone)]
struct RetiredBrowseSource {
    shared: Arc<ConnectionShared>,
    profile: ConnectionProfile,
    generation: u64,
    operation_epoch: u64,
    recovery: RecoverySnapshot,
}

struct BrowseSource {
    shared: Arc<ConnectionShared>,
    profile: ConnectionProfile,
    operation_epoch: u64,
    recovery: RecoverySnapshot,
    runtime_operations_ready: bool,
    retired: bool,
}

#[derive(Clone)]
enum RuntimeBrowseTarget {
    Candidate(String),
    CreateTmux(String),
}

/// One active browse transaction.  It intentionally retains the exact source
/// Arc and target actor rather than comparing bare generation/epoch integers
/// from late callbacks.  Credentials remain owned by the target actor's
/// `ConnectionProfile`; this record never clones or serializes them.
#[derive(Clone)]
struct RuntimeBrowseEntry {
    token: u64,
    source_owner: TerminalId,
    source_generation: u64,
    source_epoch: u64,
    source_recovery: RecoverySnapshot,
    source_runtime_operations_ready: bool,
    source_retired: bool,
    source_shared: Arc<ConnectionShared>,
    provisional_owner: TerminalId,
    provisional_generation: u64,
    provisional_shared: Arc<ConnectionShared>,
    target_endpoint: SessionEndpoint,
    browse_generation: u64,
    /// Revision visible when a refresh was accepted. Until the actor publishes
    /// a complete later revision, candidates from this snapshot are stale.
    refresh_pending_from_revision: Option<u64>,
    phase: RuntimeBrowsePhase,
    target: Option<RuntimeBrowseTarget>,
    error_code: String,
    error_message: String,
    cleanup_warning: Option<CleanupWarning>,
    active_terminal_id: Option<TerminalId>,
}

impl RuntimeBrowseEntry {
    fn token_string(&self) -> String {
        self.token.to_string()
    }

    fn source_identity_matches(
        &self,
        owner: TerminalId,
        generation: u64,
        epoch: u64,
        shared: &Arc<ConnectionShared>,
    ) -> bool {
        self.source_owner == owner
            && self.source_generation == generation
            && self.source_epoch == epoch
            && Arc::ptr_eq(&self.source_shared, shared)
    }

    fn provisional_identity_matches(
        &self,
        owner: TerminalId,
        generation: u64,
        shared: &Arc<ConnectionShared>,
    ) -> bool {
        self.provisional_owner == owner
            && self.provisional_generation == generation
            && Arc::ptr_eq(&self.provisional_shared, shared)
    }
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
static RETIRED_BROWSE_SOURCES: OnceLock<Mutex<HashMap<TerminalId, RetiredBrowseSource>>> =
    OnceLock::new();
static FENCED_TERMINALS: OnceLock<Mutex<HashSet<TerminalId>>> = OnceLock::new();
static OWNER_TRANSITIONS: OnceLock<Mutex<HashMap<TerminalId, Arc<OwnerTransition>>>> =
    OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
static RUNTIME_BROWSE: OnceLock<Mutex<Option<RuntimeBrowseEntry>>> = OnceLock::new();
static NEXT_BROWSE_TOKEN: AtomicU64 = AtomicU64::new(1);
static RUNTIME_BROWSE_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

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

fn retired_browse_sources() -> &'static Mutex<HashMap<TerminalId, RetiredBrowseSource>> {
    RETIRED_BROWSE_SOURCES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn clear_retired_browse_source(owner: TerminalId) {
    if let Ok(mut sources) = retired_browse_sources().lock() {
        sources.remove(&owner);
    }
}

fn fenced_terminals() -> &'static Mutex<HashSet<TerminalId>> {
    FENCED_TERMINALS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn fence_terminal_data_plane(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    fenced_terminals()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .insert(terminal_id);
    Ok(())
}

fn terminal_data_plane_fenced(terminal_id: TerminalId) -> bool {
    fenced_terminals()
        .lock()
        .map(|fenced| fenced.contains(&terminal_id))
        .unwrap_or(false)
}

pub(crate) fn clear_terminal_data_plane_fence(terminal_id: TerminalId) {
    if let Ok(mut fenced) = fenced_terminals().lock() {
        fenced.remove(&terminal_id);
    }
}

fn runtime_browse() -> &'static Mutex<Option<RuntimeBrowseEntry>> {
    RUNTIME_BROWSE.get_or_init(|| Mutex::new(None))
}

fn runtime_browse_serial() -> &'static Mutex<()> {
    RUNTIME_BROWSE_SERIAL.get_or_init(|| Mutex::new(()))
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
    queue_runtime_selection(terminal_id, candidate_id, false)
}

fn queue_runtime_selection(
    terminal_id: TerminalId,
    candidate_id: &str,
    provisional_bind: bool,
) -> Result<(), ConnectionError> {
    let shared = current_connection(terminal_id)?;
    if provisional_bind {
        if !shared.provisional_bind_allowed() {
            return Err(ConnectionError::BrowsePermissionDenied);
        }
    } else if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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
    queue_runtime_creation(terminal_id, backend, name, false)
}

fn queue_runtime_creation(
    terminal_id: TerminalId,
    backend: Backend,
    name: &str,
    provisional_bind: bool,
) -> Result<(), ConnectionError> {
    let shared = current_connection(terminal_id)?;
    if provisional_bind {
        if !shared.provisional_bind_allowed() {
            return Err(ConnectionError::BrowsePermissionDenied);
        }
    } else if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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

/// Start a host-only browse using credentials associated with the source
/// owner. The source terminal remains the UI anchor while the short-lived
/// target actor performs bounded discovery; a released source is browse-only
/// and never regains its old controller.
pub fn runtime_browse_start_current(
    source_owner: TerminalId,
) -> Result<RuntimeBrowseSnapshot, ConnectionError> {
    cancel_active_runtime_browse();
    let source = browse_source_profile(source_owner)?;
    let mut profile = source.profile;
    profile.backend = Backend::Tmux;
    profile.runtime = None;
    profile.tmux_identity = None;
    profile.herdr_executable = None;
    runtime_browse_start(
        source_owner,
        ConnectionStart::ProvisionalProfile(profile),
        source.shared,
        source.operation_epoch,
        source.recovery,
        source.runtime_operations_ready,
        source.retired,
    )
}

/// Start a host-only browse with a transient credential resolved by the
/// platform secure-storage/credential sheet. The options are consumed by the
/// Rust actor and are never retained in the browse record or returned to the
/// caller.
pub fn runtime_browse_start_with_options(
    source_owner: TerminalId,
    mut options: ConnectOptions,
) -> Result<RuntimeBrowseSnapshot, ConnectionError> {
    cancel_active_runtime_browse();
    let source = browse_source_profile(source_owner)?;
    options.backend = Backend::Tmux;
    options.runtime = None;
    let options = options.validate()?;
    runtime_browse_start(
        source_owner,
        ConnectionStart::ProvisionalHost(options),
        source.shared,
        source.operation_epoch,
        source.recovery,
        source.runtime_operations_ready,
        source.retired,
    )
}

/// Read the low-frequency browse state for the exact opaque token. Candidate
/// IDs are scoped by the returned discovery revision and are never re-picked
/// by name on another connection.
pub fn runtime_browse_snapshot(token: &str) -> Result<RuntimeBrowseSnapshot, ConnectionError> {
    let _serial = runtime_browse_serial()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let token = parse_browse_token(token)?;
    let browse = runtime_browse()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let entry = browse
        .as_ref()
        .filter(|entry| entry.token == token)
        .ok_or(ConnectionError::BrowseStale)?;
    runtime_browse_view(entry)
}

/// Answer the explicit host-key prompt for the provisional actor identified
/// by the browse token. The provisional root handle is never exposed to the
/// platform, so the token is the only scope accepted by this operation.
pub fn runtime_browse_respond_to_host_key(
    token: &str,
    fingerprint: &str,
    accept: bool,
) -> Result<(), ConnectionError> {
    if fingerprint.is_empty() || fingerprint.len() > FINGERPRINT_CAPACITY {
        return Err(ConnectionError::InvalidArgument);
    }
    let _serial = runtime_browse_serial()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let token = parse_browse_token(token)?;
    let (owner, shared) = {
        let browse = runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let entry = browse
            .as_ref()
            .filter(|entry| entry.token == token)
            .ok_or(ConnectionError::BrowseStale)?;
        if matches!(
            entry.phase,
            RuntimeBrowsePhase::Committing
                | RuntimeBrowsePhase::Committed
                | RuntimeBrowsePhase::Failed
                | RuntimeBrowsePhase::Cancelled
        ) {
            return Err(ConnectionError::BrowseStale);
        }
        (
            entry.provisional_owner,
            Arc::clone(&entry.provisional_shared),
        )
    };
    let current = current_connection(owner)?;
    if !Arc::ptr_eq(&current, &shared) || !current.is_provisional() || current.is_cancelled() {
        return Err(ConnectionError::BrowseStale);
    }
    respond_to_host_key(owner, fingerprint, accept)
}

/// Refresh only the explored provisional connection. The current active
/// runtime actor is never sent back through its picker loop.
pub fn runtime_browse_refresh(token: &str) -> Result<(), ConnectionError> {
    let _serial = runtime_browse_serial()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let token = parse_browse_token(token)?;
    let mut browse = runtime_browse()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let entry = browse
        .as_mut()
        .filter(|entry| entry.token == token)
        .ok_or(ConnectionError::BrowseStale)?;
    if matches!(
        entry.phase,
        RuntimeBrowsePhase::Committing
            | RuntimeBrowsePhase::Committed
            | RuntimeBrowsePhase::Failed
            | RuntimeBrowsePhase::Cancelled
    ) {
        return Err(ConnectionError::BrowseStale);
    }
    let shared = current_connection(entry.provisional_owner)?;
    if !shared.is_provisional() || shared.is_cancelled() {
        return Err(ConnectionError::BrowseStale);
    }
    let discovery = runtime_discovery_snapshot(entry.provisional_owner)?;
    if let Some(previous_revision) = entry.refresh_pending_from_revision {
        if !runtime_discovery_refresh_published(
            &discovery,
            entry.provisional_generation,
            previous_revision,
        ) {
            return Err(ConnectionError::BrowseStale);
        }
        entry.refresh_pending_from_revision = None;
    }
    if !runtime_discovery_is_ready(&discovery)
        || discovery.connection_generation != entry.provisional_generation
    {
        return Err(ConnectionError::BrowseStale);
    }
    let previous_revision = discovery.discovery_revision;
    let epoch = shared.operation_epoch();
    let sender = shared
        .command_sender()
        .ok_or(ConnectionError::BrowseUnavailable)?;
    entry.refresh_pending_from_revision = Some(previous_revision);
    if sender
        .try_send(ControlRequest {
            epoch,
            command: ControlCommand::RefreshRuntimes,
        })
        .is_err()
    {
        entry.refresh_pending_from_revision = None;
        return Err(ConnectionError::BrowseUnavailable);
    }
    Ok(())
}

/// Cancel an uncommitted browse and release its SSH actor, native root, child
/// terminals, and process-local credentials. A committed token only clears
/// the low-frequency record; it never tears down the promoted active owner.
pub fn runtime_browse_cancel(token: &str) -> Result<(), ConnectionError> {
    let retired = {
        let _serial = runtime_browse_serial()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let token = parse_browse_token(token)?;
        let entry = {
            let mut browse = runtime_browse()
                .lock()
                .map_err(|_| ConnectionError::Internal)?;
            let matches = browse.as_ref().is_some_and(|entry| entry.token == token);
            if !matches {
                return Err(ConnectionError::BrowseStale);
            }
            browse.take().expect("browse entry matched above")
        };
        (!matches!(entry.phase, RuntimeBrowsePhase::Committed)).then(|| {
            (
                entry.provisional_shared.generation,
                retire_runtime_browse_target(&entry),
            )
        })
    };
    if let Some((generation, stale_terminals)) = retired {
        destroy_stale_terminals(generation, stale_terminals);
    }
    Ok(())
}

fn fail_runtime_browse_commit(
    token: u64,
    provisional_owner: TerminalId,
    provisional_shared: &Arc<ConnectionShared>,
    error: ConnectionError,
) -> ConnectionError {
    mark_runtime_browse_failed(token, error.error_code(), error.to_string());
    retire_and_destroy_runtime_browse_target_by_id(provisional_owner, provisional_shared);
    error
}

/// Commit one exact candidate or an explicit tmux create request. The old
/// source is fenced and explicitly shut down before the target actor receives
/// the bind command. Promotion and final Ready publication continue on the
/// same shared Tokio runtime; failures after release remain retained/read-only
/// and never auto-rollback or fall back to another backend.
pub fn runtime_browse_commit(
    token: &str,
    browse_generation: u64,
    discovery_revision: u64,
    candidate_id: Option<&str>,
    create_tmux_name: Option<&str>,
) -> Result<(), ConnectionError> {
    if candidate_id.is_some() == create_tmux_name.is_some() {
        return Err(ConnectionError::InvalidArgument);
    }
    if candidate_id.is_some_and(|candidate| {
        candidate.is_empty() || candidate.len() > 128 || invalid_runtime_name(candidate)
    }) {
        return Err(ConnectionError::InvalidArgument);
    }
    if let Some(name) = create_tmux_name {
        tmux::validate_create_name(name).map_err(|_| ConnectionError::InvalidArgument)?;
    }

    let _serial = runtime_browse_serial()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let token_value = parse_browse_token(token)?;

    let (
        source_owner,
        source_shared,
        source_epoch,
        source_recovery,
        source_retired,
        provisional_owner,
        provisional_shared,
        target,
        entry_snapshot,
    ) = {
        let browse = runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let entry = browse
            .as_ref()
            .filter(|entry| entry.token == token_value)
            .ok_or(ConnectionError::BrowseStale)?;
        if !matches!(
            entry.phase,
            RuntimeBrowsePhase::Discovering | RuntimeBrowsePhase::Ready
        ) || entry.browse_generation != browse_generation
        {
            return Err(ConnectionError::BrowseStale);
        }

        let discovery = runtime_discovery_snapshot(entry.provisional_owner)?;
        if discovery.discovery_revision != discovery_revision
            || discovery.connection_generation != entry.provisional_generation
            || entry
                .refresh_pending_from_revision
                .is_some_and(|previous_revision| {
                    !runtime_discovery_refresh_published(
                        &discovery,
                        entry.provisional_generation,
                        previous_revision,
                    )
                })
        {
            return Err(ConnectionError::BrowseStale);
        }
        let target = if let Some(candidate_id) = candidate_id {
            let candidate = discovery
                .tmux
                .candidates
                .iter()
                .chain(discovery.herdr.candidates.iter())
                .find(|candidate| candidate.id == candidate_id)
                .ok_or(ConnectionError::RuntimeSelectionUnavailable)?;
            if !candidate.selectable {
                return Err(ConnectionError::RuntimeSelectionUnavailable);
            }
            let state = session_state(entry.provisional_owner);
            let state = state.lock().map_err(|_| ConnectionError::Internal)?;
            if !state.runtime_candidates.contains_key(candidate_id) {
                return Err(ConnectionError::RuntimeSelectionUnavailable);
            }
            RuntimeBrowseTarget::Candidate(candidate_id.to_owned())
        } else {
            if !matches!(
                discovery.tmux.state,
                RuntimeSectionState::Success | RuntimeSectionState::Empty
            ) {
                return Err(ConnectionError::RuntimeSelectionUnavailable);
            }
            RuntimeBrowseTarget::CreateTmux(
                create_tmux_name
                    .expect("validated create target")
                    .to_owned(),
            )
        };

        let source = if entry.source_retired {
            let retired = retired_browse_sources()
                .lock()
                .map_err(|_| ConnectionError::Internal)?
                .get(&entry.source_owner)
                .filter(|retired| {
                    Arc::ptr_eq(&retired.shared, &entry.source_shared)
                        && retired.generation == entry.source_generation
                        && retired.operation_epoch == entry.source_epoch
                        && retired.recovery == entry.source_recovery
                })
                .map(|retired| Arc::clone(&retired.shared))
                .ok_or(ConnectionError::BrowseStale)?;
            if connections()
                .lock()
                .map_err(|_| ConnectionError::Internal)?
                .contains_key(&entry.source_owner)
            {
                return Err(ConnectionError::BrowseStale);
            }
            retired
        } else {
            connections()
                .lock()
                .map_err(|_| ConnectionError::Internal)?
                .get(&entry.source_owner)
                .map(|connection| Arc::clone(&connection.shared))
                .ok_or(ConnectionError::BrowseStale)?
        };
        if !entry.source_identity_matches(
            entry.source_owner,
            source.generation,
            source.operation_epoch(),
            &source,
        ) || source.is_provisional()
            || (!entry.source_retired && source.is_cancelled())
        {
            return Err(ConnectionError::BrowseStale);
        }
        let source_state = source
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if !browse_source_state_matches(entry, &source_state) {
            return Err(ConnectionError::BrowseStale);
        }
        drop(source_state);

        let provisional = connections()
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .get(&entry.provisional_owner)
            .map(|connection| Arc::clone(&connection.shared))
            .ok_or(ConnectionError::BrowseStale)?;
        if !entry.provisional_identity_matches(
            entry.provisional_owner,
            provisional.generation,
            &provisional,
        ) || !provisional.is_provisional()
            || provisional.is_cancelled()
            || !provisional.is_awaiting_runtime_selection()
        {
            return Err(ConnectionError::BrowseStale);
        }
        let provisional_state = provisional
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if provisional_state.endpoint.as_ref() != Some(&entry.target_endpoint) {
            return Err(ConnectionError::BrowseStale);
        }
        drop(provisional_state);
        (
            entry.source_owner,
            source,
            entry.source_epoch,
            entry.source_recovery.clone(),
            entry.source_retired,
            entry.provisional_owner,
            provisional,
            target,
            entry.clone(),
        )
    };

    if !source_retired
        && runtime_browse_noop_is_confirmed(
            &entry_snapshot,
            &source_shared,
            &provisional_shared,
            discovery_revision,
            &target,
        )?
    {
        let mut browse = runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let entry = browse
            .as_mut()
            .filter(|entry| entry.token == token_value)
            .ok_or(ConnectionError::BrowseStale)?;
        if entry.browse_generation != browse_generation
            || !matches!(
                entry.phase,
                RuntimeBrowsePhase::Discovering | RuntimeBrowsePhase::Ready
            )
            || !Arc::ptr_eq(&entry.source_shared, &source_shared)
            || !Arc::ptr_eq(&entry.provisional_shared, &provisional_shared)
        {
            return Err(ConnectionError::BrowseStale);
        }
        entry.phase = RuntimeBrowsePhase::Unchanged;
        entry.target = Some(target);
        entry.refresh_pending_from_revision = None;
        entry.active_terminal_id = Some(source_owner);
        return Ok(());
    }

    {
        let mut browse = runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let entry = browse
            .as_mut()
            .filter(|entry| entry.token == token_value)
            .ok_or(ConnectionError::BrowseStale)?;
        if entry.browse_generation != browse_generation
            || !matches!(
                entry.phase,
                RuntimeBrowsePhase::Discovering | RuntimeBrowsePhase::Ready
            )
            || !Arc::ptr_eq(&entry.source_shared, &source_shared)
            || !Arc::ptr_eq(&entry.provisional_shared, &provisional_shared)
        {
            return Err(ConnectionError::BrowseStale);
        }
        entry.phase = RuntimeBrowsePhase::Committing;
        entry.target = Some(target.clone());
        entry.refresh_pending_from_revision = None;
    }

    // Keep the source owner's serial/commit boundary across the fence, map
    // retirement, and bounded explicit shutdown. A concurrent reconnect or
    // disconnect therefore cannot install/cancel a late source generation.
    let owner = match owner_transition(source_owner) {
        Ok(owner) => owner,
        Err(error) => {
            drop(_serial);
            return Err(fail_runtime_browse_commit(
                token_value,
                provisional_owner,
                &provisional_shared,
                error,
            ));
        }
    };
    let _owner_serial = match owner.serial.lock() {
        Ok(guard) => guard,
        Err(_) => {
            drop(_serial);
            return Err(fail_runtime_browse_commit(
                token_value,
                provisional_owner,
                &provisional_shared,
                ConnectionError::Internal,
            ));
        }
    };
    let _owner_commit = match owner.commit.lock() {
        Ok(guard) => guard,
        Err(_) => {
            drop(_owner_serial);
            drop(_serial);
            return Err(fail_runtime_browse_commit(
                token_value,
                provisional_owner,
                &provisional_shared,
                ConnectionError::Internal,
            ));
        }
    };
    macro_rules! fail_commit {
        ($error:expr) => {{
            drop(_owner_commit);
            drop(_owner_serial);
            drop(_serial);
            return Err(fail_runtime_browse_commit(
                token_value,
                provisional_owner,
                &provisional_shared,
                $error,
            ));
        }};
    }
    if source_retired {
        if !terminal_data_plane_fenced(source_owner)
            || !retired_browse_source_matches(
                source_owner,
                &source_shared,
                source_epoch,
                &source_recovery,
            )
        {
            fail_commit!(ConnectionError::BrowseStale);
        }
    } else if let Err(error) =
        source_shared.fence_for_runtime_switch(source_epoch, &source_recovery)
    {
        fail_commit!(error);
    }
    owner.cancel_current_locked();
    let old_entry = if source_retired {
        None
    } else {
        detach_all(&source_shared);
        match retire_runtime_browse_source(source_owner, &source_shared) {
            Ok(entry) => Some(entry),
            Err(error) => fail_commit!(error),
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            fail_commit!(error);
        }
    };
    if let Some(old_entry) = old_entry {
        let shutdown = finish_or_force_explicit_shutdown(
            runtime,
            Arc::clone(&old_entry.shared),
            old_entry.abort,
        );
        let _ = retire_explicit_cleanup_result(&source_shared, shutdown, false);
    }
    let cleanup_warning = match source_shared.session.lock() {
        Ok(state) => state.cleanup_warning.clone(),
        Err(_) => {
            fail_commit!(ConnectionError::Internal);
        }
    };

    let queue_result = match &target {
        RuntimeBrowseTarget::Candidate(candidate) => {
            provisional_shared.arm_provisional_bind();
            queue_runtime_selection(provisional_owner, candidate, true)
        }
        RuntimeBrowseTarget::CreateTmux(name) => {
            provisional_shared.arm_provisional_bind();
            queue_runtime_creation(provisional_owner, Backend::Tmux, name, true)
        }
    };
    if let Err(error) = queue_result {
        mark_runtime_browse_failed(token_value, error.error_code(), error.to_string());
        drop(_owner_commit);
        drop(_owner_serial);
        drop(_serial);
        retire_and_destroy_runtime_browse_target_by_id(provisional_owner, &provisional_shared);
        return Err(error);
    }

    if let Ok(mut browse) = runtime_browse().lock()
        && let Some(entry) = browse.as_mut().filter(|entry| entry.token == token_value)
    {
        entry.cleanup_warning = cleanup_warning;
    }
    schedule_runtime_browse_promotion(
        token_value,
        source_owner,
        provisional_owner,
        provisional_shared,
    );
    Ok(())
}

fn parse_browse_token(token: &str) -> Result<u64, ConnectionError> {
    if token.is_empty() || token.len() > 20 || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ConnectionError::InvalidArgument);
    }
    token
        .parse::<u64>()
        .ok()
        .filter(|value| *value != 0)
        .ok_or(ConnectionError::InvalidArgument)
}

fn next_browse_token() -> u64 {
    loop {
        let token = NEXT_BROWSE_TOKEN.fetch_add(1, Ordering::Relaxed);
        if token != 0 {
            return token;
        }
    }
}

fn browse_source_profile(source_owner: TerminalId) -> Result<BrowseSource, ConnectionError> {
    registry::shared_terminal(source_owner).map_err(map_terminal_error)?;
    let active = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&source_owner)
        .map(|entry| Arc::clone(&entry.shared));
    if let Some(shared) = active {
        if shared.is_provisional() || shared.is_cancelled() {
            return Err(ConnectionError::BrowseUnavailable);
        }
        let state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let profile = state
            .profile
            .clone()
            .ok_or(ConnectionError::BrowseUnavailable)?;
        let source_is_ready =
            state.recovery.phase == RecoveryPhase::None && state.runtime_operations_ready;
        let source_is_retained_recovery = state.recovery.phase != RecoveryPhase::None
            && !state.runtime_operations_ready
            && state.has_retained_work();
        if state.generation != shared.generation
            || (!source_is_ready && !source_is_retained_recovery)
        {
            return Err(ConnectionError::BrowseUnavailable);
        }
        return Ok(BrowseSource {
            shared: Arc::clone(&shared),
            profile,
            operation_epoch: state.operation_epoch,
            recovery: state.recovery.clone(),
            runtime_operations_ready: state.runtime_operations_ready,
            retired: false,
        });
    }

    let retired = retired_browse_sources()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&source_owner)
        .cloned()
        .ok_or(ConnectionError::BrowseUnavailable)?;
    if retired.shared.is_provisional() {
        return Err(ConnectionError::BrowseUnavailable);
    }
    let state = retired
        .shared
        .session
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let state_profile = state
        .profile
        .as_ref()
        .ok_or(ConnectionError::BrowseUnavailable)?;
    let retained_browse_only_state = state.generation == retired.generation
        && state.generation == retired.shared.generation
        && state.operation_epoch == retired.operation_epoch
        && state.recovery == retired.recovery
        && state.recovery.phase != RecoveryPhase::None
        && !state.runtime_operations_ready
        && !state.terminal_input_ready
        && state.has_retained_work()
        && state.endpoint.as_ref() == Some(&SessionEndpoint::from_profile(&retired.profile))
        && same_ssh_endpoint(state_profile, &retired.profile)
        && stored_credentials_share_identity(
            &state_profile.credentials,
            &retired.profile.credentials,
        );
    if !retained_browse_only_state {
        return Err(ConnectionError::BrowseUnavailable);
    }
    Ok(BrowseSource {
        shared: Arc::clone(&retired.shared),
        profile: retired.profile,
        operation_epoch: retired.operation_epoch,
        recovery: retired.recovery,
        runtime_operations_ready: false,
        retired: true,
    })
}

fn browse_source_state_matches(entry: &RuntimeBrowseEntry, state: &SessionState) -> bool {
    let was_ready =
        entry.source_runtime_operations_ready && entry.source_recovery.phase == RecoveryPhase::None;
    let was_retained_recovery = !entry.source_runtime_operations_ready
        && entry.source_recovery.phase != RecoveryPhase::None;
    state.generation == entry.source_generation
        && state.operation_epoch == entry.source_epoch
        && state.recovery == entry.source_recovery
        && state.runtime_operations_ready == entry.source_runtime_operations_ready
        && (was_ready || (was_retained_recovery && state.has_retained_work()))
}

fn retired_browse_source_matches(
    owner: TerminalId,
    shared: &Arc<ConnectionShared>,
    operation_epoch: u64,
    recovery: &RecoverySnapshot,
) -> bool {
    retired_browse_sources()
        .lock()
        .map(|sources| {
            sources.get(&owner).is_some_and(|retired| {
                Arc::ptr_eq(&retired.shared, shared)
                    && retired.generation == shared.generation
                    && retired.operation_epoch == operation_epoch
                    && &retired.recovery == recovery
            })
        })
        .unwrap_or(false)
}

/// Remove a source actor from the normal connection map only after its state
/// is fenced, then retain just the parsed credentials and exact source
/// identity needed to anchor another explicit browse. The source remains
/// absent from `connections` for all normal operations.
fn retire_runtime_browse_source(
    owner: TerminalId,
    shared: &Arc<ConnectionShared>,
) -> Result<ConnectionEntry, ConnectionError> {
    let (profile, operation_epoch, recovery) = {
        let state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        if state.generation != shared.generation
            || state.runtime_operations_ready
            || state.terminal_input_ready
            || state.recovery.phase == RecoveryPhase::None
            || state.recovery.reason != "runtime_changed"
            || !terminal_data_plane_fenced(owner)
        {
            return Err(ConnectionError::BrowseStale);
        }
        (
            state
                .profile
                .clone()
                .ok_or(ConnectionError::BrowseUnavailable)?,
            state.operation_epoch,
            state.recovery.clone(),
        )
    };

    let mut entries = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    if !entries
        .get(&owner)
        .is_some_and(|entry| Arc::ptr_eq(&entry.shared, shared))
    {
        return Err(ConnectionError::BrowseStale);
    }
    let mut retired = retired_browse_sources()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let entry = entries.remove(&owner).ok_or(ConnectionError::BrowseStale)?;
    retired.insert(
        owner,
        RetiredBrowseSource {
            shared: Arc::clone(shared),
            profile,
            generation: shared.generation,
            operation_epoch,
            recovery,
        },
    );
    Ok(entry)
}

fn stored_credentials_share_identity(left: &StoredCredentials, right: &StoredCredentials) -> bool {
    match (left, right) {
        (
            StoredCredentials::PublicKey { key: left },
            StoredCredentials::PublicKey { key: right },
        ) => Arc::ptr_eq(left, right),
        (
            StoredCredentials::Password { password: left },
            StoredCredentials::Password { password: right },
        ) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

fn same_ssh_endpoint(left: &ConnectionProfile, right: &ConnectionProfile) -> bool {
    left.host == right.host
        && left.port == right.port
        && left.username == right.username
        && left.known_hosts_path == right.known_hosts_path
}

/// Confirm only the identity that the current read-only discovery can prove:
/// an exact tmux session ID within the exact server PID/start-time epoch. The
/// provisional connection must use the same native credential object as the
/// source. Herdr's browse rows contain a running session name but not its
/// stable terminal ID, so they intentionally cannot satisfy this predicate.
fn runtime_browse_noop_is_confirmed(
    entry: &RuntimeBrowseEntry,
    source_shared: &Arc<ConnectionShared>,
    provisional_shared: &Arc<ConnectionShared>,
    discovery_revision: u64,
    target: &RuntimeBrowseTarget,
) -> Result<bool, ConnectionError> {
    let RuntimeBrowseTarget::Candidate(candidate_id) = target else {
        return Ok(false);
    };
    if entry.source_owner != source_shared.terminal_id()
        || entry.source_generation != source_shared.generation
        || entry.provisional_owner != provisional_shared.terminal_id()
        || entry.provisional_generation != provisional_shared.generation
        || !Arc::ptr_eq(&entry.source_shared, source_shared)
        || !Arc::ptr_eq(&entry.provisional_shared, provisional_shared)
        || entry.source_shared.is_cancelled()
        || provisional_shared.is_cancelled()
    {
        return Ok(false);
    }

    // Explicit owner replacement/disconnect is serialized against this
    // identity check. Recovery itself is checked under the source session
    // lock below; if it advances before that check, the normal commit path
    // will reject the stale browse.
    let owner = owner_transition(entry.source_owner)?;
    let _owner_serial = owner.serial.lock().map_err(|_| ConnectionError::Internal)?;
    let _owner_commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
    let current_source = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&entry.source_owner)
        .is_some_and(|connection| Arc::ptr_eq(&connection.shared, source_shared));
    let current_target = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&entry.provisional_owner)
        .is_some_and(|connection| Arc::ptr_eq(&connection.shared, provisional_shared));
    if !current_source || !current_target || !provisional_shared.is_provisional() {
        return Ok(false);
    }

    let source_info = source_shared
        .info
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    if source_info.finished || source_info.state != ConnectionState::Ready {
        return Ok(false);
    }
    let source_state = source_shared
        .session
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    if !browse_source_state_matches(entry, &source_state)
        || !entry.source_runtime_operations_ready
        || entry.source_recovery.phase != RecoveryPhase::None
    {
        return Ok(false);
    }
    let Some(source_profile) = source_state.profile.as_ref() else {
        return Ok(false);
    };
    if source_profile.backend != Backend::Tmux
        || source_state.endpoint.as_ref() != Some(&SessionEndpoint::from_profile(source_profile))
        || source_shared.host != source_profile.host
        || source_shared.port != source_profile.port
        || source_shared.known_hosts_path != source_profile.known_hosts_path
    {
        return Ok(false);
    }
    let Some(source_identity) = source_profile.tmux_identity.as_ref() else {
        return Ok(false);
    };

    let provisional_state = provisional_shared
        .session
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    if provisional_state.generation != entry.provisional_generation
        || provisional_state.runtime_discovery.connection_generation != entry.provisional_generation
        || provisional_state.runtime_discovery.discovery_revision != discovery_revision
        || provisional_state.endpoint.as_ref() != Some(&entry.target_endpoint)
    {
        return Ok(false);
    }
    let selectable = provisional_state
        .runtime_discovery
        .tmux
        .candidates
        .iter()
        .find(|candidate| candidate.id == *candidate_id)
        .is_some_and(|candidate| candidate.selectable);
    let Some(RuntimeBinding::Tmux(target_identity)) =
        provisional_state.runtime_candidates.get(candidate_id)
    else {
        return Ok(false);
    };
    let Some(target_profile) = provisional_state.profile.as_ref() else {
        return Ok(false);
    };
    selectable
        .then_some(
            target_profile.backend == Backend::Tmux
                && target_profile.runtime.is_none()
                && target_identity == source_identity
                && same_ssh_endpoint(source_profile, target_profile)
                && stored_credentials_share_identity(
                    &source_profile.credentials,
                    &target_profile.credentials,
                ),
        )
        .ok_or(ConnectionError::RuntimeSelectionUnavailable)
}

fn cancel_active_runtime_browse() {
    let retired = {
        let Ok(_serial) = runtime_browse_serial().lock() else {
            return;
        };
        runtime_browse()
            .lock()
            .ok()
            .and_then(|mut browse| browse.take())
            .filter(|entry| !matches!(entry.phase, RuntimeBrowsePhase::Committed))
            .map(|entry| {
                (
                    entry.provisional_shared.generation,
                    retire_runtime_browse_target(&entry),
                )
            })
    };
    if let Some((generation, stale_terminals)) = retired {
        destroy_stale_terminals(generation, stale_terminals);
    }
}

fn cancel_runtime_browse_for_source(source_owner: TerminalId) {
    let retired = {
        let Ok(_serial) = runtime_browse_serial().lock() else {
            return;
        };
        runtime_browse().lock().ok().and_then(|mut browse| {
            browse
                .as_ref()
                .is_some_and(|entry| entry.source_owner == source_owner)
                .then(|| browse.take().expect("browse entry matched above"))
                .filter(|entry| !matches!(entry.phase, RuntimeBrowsePhase::Committed))
                .map(|entry| {
                    (
                        entry.provisional_shared.generation,
                        retire_runtime_browse_target(&entry),
                    )
                })
        })
    };
    if let Some((generation, stale_terminals)) = retired {
        destroy_stale_terminals(generation, stale_terminals);
    }
}

fn runtime_browse_start(
    source_owner: TerminalId,
    start: ConnectionStart,
    source_shared: Arc<ConnectionShared>,
    source_epoch: u64,
    source_recovery: RecoverySnapshot,
    source_runtime_operations_ready: bool,
    source_retired: bool,
) -> Result<RuntimeBrowseSnapshot, ConnectionError> {
    let _serial = runtime_browse_serial()
        .lock()
        .map_err(|_| ConnectionError::Internal)?;
    let mut retired = None;
    let result = (|| {
        // Opening a different server or reopening the same row cancels the
        // prior uncommitted browse before allocating another root. Keep the
        // single-slot transaction serialized, but defer child destruction
        // until after its locks are released.
        if let Some(previous) = runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .take()
            && !matches!(previous.phase, RuntimeBrowsePhase::Committed)
        {
            retired = Some((
                previous.provisional_shared.generation,
                retire_runtime_browse_target(&previous),
            ));
        }

        let source_generation = source_shared.generation;
        let dimensions = registry::terminal_dimensions(source_owner).unwrap_or((80, 24));
        let provisional_owner =
            registry::create_terminal(dimensions.0, dimensions.1).map_err(map_terminal_error)?;
        let target_endpoint = match &start {
            ConnectionStart::ProvisionalHost(options) => SessionEndpoint::from_options(options),
            ConnectionStart::ProvisionalProfile(profile) => SessionEndpoint::from_profile(profile),
            _ => {
                registry::discard_terminal(provisional_owner);
                return Err(ConnectionError::InvalidArgument);
            }
        };
        if let Err(error) = start_connection(provisional_owner, start) {
            registry::discard_terminal(provisional_owner);
            return Err(error);
        }
        let provisional_shared = match current_connection(provisional_owner) {
            Ok(shared) => shared,
            Err(error) => {
                registry::discard_terminal(provisional_owner);
                return Err(error);
            }
        };
        let entry = RuntimeBrowseEntry {
            token: next_browse_token(),
            source_owner,
            source_generation,
            source_epoch,
            source_recovery,
            source_runtime_operations_ready,
            source_retired,
            source_shared,
            provisional_owner,
            provisional_generation: provisional_shared.generation,
            provisional_shared,
            target_endpoint,
            browse_generation: next_browse_token(),
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Discovering,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        };
        let snapshot = runtime_browse_view(&entry)?;
        runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .replace(entry);
        Ok(snapshot)
    })();
    drop(_serial);
    if let Some((generation, stale_terminals)) = retired {
        destroy_stale_terminals(generation, stale_terminals);
    }
    result
}

fn runtime_browse_view(
    entry: &RuntimeBrowseEntry,
) -> Result<RuntimeBrowseSnapshot, ConnectionError> {
    let published_discovery = if entry.provisional_owner == 0 {
        RuntimeDiscoverySnapshot::default()
    } else {
        runtime_discovery_snapshot(entry.provisional_owner).unwrap_or_default()
    };
    let refresh_pending = entry
        .refresh_pending_from_revision
        .is_some_and(|previous_revision| {
            !runtime_discovery_refresh_published(
                &published_discovery,
                entry.provisional_generation,
                previous_revision,
            )
        });
    // A refresh is accepted synchronously, before the actor can consume its
    // bounded command queue. Hide the old candidate IDs during that interval
    // so a snapshot read cannot make a queued stale selection look current.
    let discovery = if refresh_pending {
        RuntimeDiscoverySnapshot::loading(
            entry.provisional_generation,
            entry
                .refresh_pending_from_revision
                .unwrap_or_default()
                .saturating_add(1)
                .max(1),
        )
    } else {
        published_discovery
    };
    let connection = if entry.provisional_owner == 0 {
        ConnectionSnapshot::disconnected()
    } else {
        connection_snapshot(entry.provisional_owner)
            .unwrap_or_else(|_| ConnectionSnapshot::disconnected())
    };
    let mut phase = if refresh_pending {
        RuntimeBrowsePhase::Discovering
    } else {
        entry.phase
    };
    let mut error_code = entry.error_code.clone();
    let mut error_message = entry.error_message.clone();
    if matches!(
        phase,
        RuntimeBrowsePhase::Discovering | RuntimeBrowsePhase::Ready
    ) {
        if connection.state == ConnectionState::Failed as u32 {
            phase = RuntimeBrowsePhase::Failed;
            if error_code.is_empty() {
                error_code = bounded_error_text(&connection, true);
            }
            if error_message.is_empty() {
                error_message = bounded_error_text(&connection, false);
            }
        } else if runtime_discovery_is_ready(&discovery) {
            phase = RuntimeBrowsePhase::Ready;
        }
    }
    let cleanup_warning = entry.cleanup_warning.clone().or_else(|| {
        entry
            .source_shared
            .session
            .lock()
            .ok()
            .and_then(|state| state.cleanup_warning.clone())
    });
    let host_key = RuntimeBrowseHostKeySnapshot {
        pending: connection.state == ConnectionState::HostKeyPending as u32,
        host: bounded_snapshot_text(&connection.host, connection.host_len),
        port: connection.port,
        fingerprint: bounded_snapshot_text(&connection.fingerprint, connection.fingerprint_len),
        algorithm: bounded_snapshot_text(&connection.algorithm, connection.algorithm_len),
        known_fingerprint: bounded_snapshot_text(
            &connection.known_fingerprint,
            connection.known_fingerprint_len,
        ),
    };
    Ok(RuntimeBrowseSnapshot {
        token: entry.token_string(),
        browse_generation: entry.browse_generation,
        discovery_revision: discovery.discovery_revision,
        phase,
        discovery,
        error_code,
        error_message,
        host_key,
        cleanup_warning,
        active_terminal_id: entry.active_terminal_id,
    })
}

fn bounded_snapshot_text(bytes: &[u8], length: u16) -> String {
    String::from_utf8_lossy(&bytes[..usize::from(length).min(bytes.len())]).into_owned()
}

fn bounded_error_text(snapshot: &ConnectionSnapshot, code: bool) -> String {
    let (bytes, length): (&[u8], u16) = if code {
        (&snapshot.error_code[..], snapshot.error_code_len)
    } else {
        (&snapshot.error_message[..], snapshot.error_message_len)
    };
    String::from_utf8_lossy(&bytes[..usize::from(length).min(bytes.len())]).into_owned()
}

fn runtime_discovery_is_ready(discovery: &RuntimeDiscoverySnapshot) -> bool {
    !matches!(discovery.tmux.state, RuntimeSectionState::Loading)
        && !matches!(discovery.herdr.state, RuntimeSectionState::Loading)
        && discovery.discovery_revision != 0
}

fn runtime_discovery_refresh_published(
    discovery: &RuntimeDiscoverySnapshot,
    generation: u64,
    previous_revision: u64,
) -> bool {
    discovery.connection_generation == generation
        && discovery.discovery_revision > previous_revision
        && runtime_discovery_is_ready(discovery)
}

fn mark_runtime_browse_failed(token: u64, code: &str, message: String) {
    if let Ok(mut browse) = runtime_browse().lock()
        && let Some(entry) = browse.as_mut().filter(|entry| entry.token == token)
    {
        entry.phase = RuntimeBrowsePhase::Failed;
        entry.error_code = code.to_owned();
        entry.error_message = message;
        entry.target = None;
    }
}

fn retire_runtime_browse_target(entry: &RuntimeBrowseEntry) -> Vec<TerminalId> {
    retire_runtime_browse_target_by_id(entry.provisional_owner, &entry.provisional_shared)
}

fn retire_runtime_browse_target_by_id(
    provisional_owner: TerminalId,
    provisional_shared: &Arc<ConnectionShared>,
) -> Vec<TerminalId> {
    if provisional_owner == 0 {
        return Vec::new();
    }
    // A promotion changes both the public owner and the shared actor's native
    // root before the browse record is allowed to become `Committed`. A late
    // monitor/cancel callback may still carry the old provisional ID; it must
    // not cancel the actor that now owns the source handle.
    if !provisional_shared.is_provisional() && provisional_shared.terminal_id() != provisional_owner
    {
        return Vec::new();
    }
    let owner = match owner_transition(provisional_owner) {
        Ok(owner) => owner,
        Err(_) => {
            provisional_shared.cancel();
            let stale_terminals = take_runtime_browse_children(provisional_owner);
            let _ = registry::discard_terminal(provisional_owner);
            return stale_terminals;
        }
    };
    let entry = {
        let Ok(_commit) = owner.commit.lock() else {
            provisional_shared.cancel();
            let stale_terminals = take_runtime_browse_children(provisional_owner);
            let _ = registry::discard_terminal(provisional_owner);
            return stale_terminals;
        };
        owner.cancel_current_locked();
        connections().lock().ok().and_then(|mut entries| {
            let matches = entries
                .get(&provisional_owner)
                .is_some_and(|entry| Arc::ptr_eq(&entry.shared, provisional_shared));
            matches
                .then(|| entries.remove(&provisional_owner))
                .flatten()
        })
    };
    provisional_shared.invalidate_explicitly("explicit_disconnect");
    detach_all(provisional_shared);
    if let Some(entry) = entry {
        if let Ok(runtime) = runtime() {
            let shutdown = finish_or_force_explicit_shutdown(
                runtime,
                Arc::clone(provisional_shared),
                entry.abort,
            );
            let _ = retire_explicit_cleanup_result(provisional_shared, shutdown, false);
        } else {
            provisional_shared.cancel();
            provisional_shared.finish(Err(FlowFailure::Stale));
            entry.abort.abort();
        }
    } else {
        provisional_shared.cancel();
    }
    let stale_terminals = take_runtime_browse_children(provisional_owner);
    let _ = registry::discard_terminal(provisional_owner);
    stale_terminals
}

fn take_runtime_browse_children(owner: TerminalId) -> Vec<TerminalId> {
    session_states()
        .lock()
        .ok()
        .and_then(|mut states| states.remove(&owner))
        .and_then(|state| {
            state.lock().ok().map(|state| {
                state
                    .pane_terminals
                    .values()
                    .copied()
                    .filter(|id| *id != owner)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default()
}

fn retire_and_destroy_runtime_browse_target_by_id(
    provisional_owner: TerminalId,
    provisional_shared: &Arc<ConnectionShared>,
) {
    let stale_terminals = retire_runtime_browse_target_by_id(provisional_owner, provisional_shared);
    destroy_stale_terminals(provisional_shared.generation, stale_terminals);
}

fn schedule_runtime_browse_promotion(
    token: u64,
    source_owner: TerminalId,
    provisional_owner: TerminalId,
    provisional_shared: Arc<ConnectionShared>,
) {
    let Ok(runtime) = runtime() else {
        mark_runtime_browse_failed(
            token,
            "runtime_unavailable",
            "Native runtime is unavailable.".to_owned(),
        );
        retire_and_destroy_runtime_browse_target_by_id(provisional_owner, &provisional_shared);
        return;
    };
    runtime.spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if provisional_shared.has_been_ready() {
                let Some(sender) = provisional_shared.command_sender() else {
                    mark_runtime_browse_failed(
                        token,
                        "runtime_browse_promotion_failed",
                        "The selected runtime could not be promoted safely.".to_owned(),
                    );
                    retire_and_destroy_runtime_browse_target_by_id(
                        provisional_owner,
                        &provisional_shared,
                    );
                    return;
                };
                let epoch = provisional_shared.operation_epoch();
                if sender
                    .try_send(ControlRequest {
                        epoch,
                        command: ControlCommand::PromoteRuntimeBrowse {
                            token,
                            source_owner,
                            provisional_owner,
                        },
                    })
                    .is_err()
                {
                    mark_runtime_browse_failed(
                        token,
                        "runtime_browse_promotion_failed",
                        "The selected runtime could not be promoted safely.".to_owned(),
                    );
                    retire_and_destroy_runtime_browse_target_by_id(
                        provisional_owner,
                        &provisional_shared,
                    );
                } else {
                    monitor_runtime_browse_promotion(
                        token,
                        provisional_owner,
                        Arc::clone(&provisional_shared),
                    );
                }
                return;
            }
            if provisional_shared.is_cancelled()
                || Instant::now() >= deadline
                || runtime_browse_target_failed(token)
            {
                mark_runtime_browse_failed(
                    token,
                    "runtime_browse_commit_failed",
                    "The selected runtime could not be committed safely.".to_owned(),
                );
                retire_and_destroy_runtime_browse_target_by_id(
                    provisional_owner,
                    &provisional_shared,
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
}

fn monitor_runtime_browse_promotion(
    token: u64,
    provisional_owner: TerminalId,
    provisional_shared: Arc<ConnectionShared>,
) {
    let Ok(runtime) = runtime() else {
        return;
    };
    runtime.spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if !runtime_browse_active(token) {
                return;
            }
            if runtime_browse_committed(token) {
                return;
            }
            if provisional_shared.is_cancelled()
                || Instant::now() >= deadline
                || runtime_browse_target_failed(token)
            {
                mark_runtime_browse_failed(
                    token,
                    "runtime_browse_promotion_failed",
                    "The selected runtime could not be promoted safely.".to_owned(),
                );
                retire_and_destroy_runtime_browse_target_by_id(
                    provisional_owner,
                    &provisional_shared,
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
}

fn runtime_browse_committed(token: u64) -> bool {
    runtime_browse()
        .lock()
        .map(|browse| {
            browse.as_ref().is_some_and(|entry| {
                entry.token == token && entry.phase == RuntimeBrowsePhase::Committed
            })
        })
        .unwrap_or(false)
}

fn runtime_browse_active(token: u64) -> bool {
    runtime_browse()
        .lock()
        .map(|browse| browse.as_ref().is_some_and(|entry| entry.token == token))
        .unwrap_or(false)
}

fn runtime_browse_target_failed(token: u64) -> bool {
    let Ok(browse) = runtime_browse().lock() else {
        return true;
    };
    let Some(entry) = browse.as_ref().filter(|entry| entry.token == token) else {
        return false;
    };
    if matches!(
        entry.phase,
        RuntimeBrowsePhase::Failed | RuntimeBrowsePhase::Cancelled
    ) {
        return true;
    }
    let Some(target) = entry.target.as_ref() else {
        return false;
    };
    let state = match entry.provisional_shared.session.lock() {
        Ok(state) => state,
        Err(_) => return true,
    };
    match target {
        RuntimeBrowseTarget::Candidate(candidate) => state
            .runtime_discovery
            .tmux
            .candidates
            .iter()
            .chain(state.runtime_discovery.herdr.candidates.iter())
            .find(|item| item.id == *candidate)
            .is_some_and(|item| !item.selectable),
        RuntimeBrowseTarget::CreateTmux(_) => state.runtime_discovery.tmux.error_code.is_some(),
    }
}

fn carry_cleanup_warning(old: &SessionState, new: &mut SessionState) {
    if new.cleanup_warning.is_none() {
        new.cleanup_warning = old.cleanup_warning.clone();
    }
    if new.cleanup_warning_result.is_none() {
        new.cleanup_warning_result = old.cleanup_warning_result;
    }
}

fn promote_runtime_browse(
    token: u64,
    source_owner: TerminalId,
    provisional_owner: TerminalId,
    provisional_shared: Arc<ConnectionShared>,
) -> Result<(), ConnectionError> {
    let (old_generation, old_children) = {
        let serial = runtime_browse_serial()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let mut browse = runtime_browse()
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        let entry_matches = browse.as_ref().is_some_and(|entry| {
            entry.token == token
                && entry.source_owner == source_owner
                && entry.provisional_owner == provisional_owner
                && Arc::ptr_eq(&entry.provisional_shared, &provisional_shared)
                && entry.phase == RuntimeBrowsePhase::Committing
        });
        if !entry_matches {
            return Err(ConnectionError::BrowseStale);
        }

        // Keep owner disposal/reconnect from interleaving with the final root
        // move. `terminal_destroyed` waits on the browse serial before touching
        // the same owner transition, so a missing source root is observed
        // before any SessionState or connection-map mutation.
        let owner = owner_transition(source_owner)?;
        let owner_serial = owner.serial.lock().map_err(|_| ConnectionError::Internal)?;
        let owner_commit = owner.commit.lock().map_err(|_| ConnectionError::Internal)?;
        {
            let entries = connections()
                .lock()
                .map_err(|_| ConnectionError::Internal)?;
            if entries.contains_key(&source_owner)
                || !entries
                    .get(&provisional_owner)
                    .is_some_and(|connection| Arc::ptr_eq(&connection.shared, &provisional_shared))
            {
                return Err(ConnectionError::BrowseStale);
            }
        }
        {
            let states = session_states()
                .lock()
                .map_err(|_| ConnectionError::Internal)?;
            let old_state = states
                .get(&source_owner)
                .cloned()
                .ok_or(ConnectionError::BrowseStale)?;
            let new_state = states
                .get(&provisional_owner)
                .cloned()
                .ok_or(ConnectionError::BrowseStale)?;
            let _old = old_state.lock().map_err(|_| ConnectionError::Internal)?;
            let _new = new_state.lock().map_err(|_| ConnectionError::Internal)?;
            registry::shared_terminal(source_owner).map_err(|_| ConnectionError::BrowseStale)?;
            registry::shared_terminal(provisional_owner)
                .map_err(|_| ConnectionError::BrowseStale)?;
        }

        registry::promote_terminal(provisional_owner, source_owner)
            .map_err(|_| ConnectionError::BrowseStale)?;
        {
            let mut entries = connections()
                .lock()
                .map_err(|_| ConnectionError::Internal)?;
            let target = entries
                .remove(&provisional_owner)
                .ok_or(ConnectionError::BrowseStale)?;
            entries.insert(source_owner, target);
        }
        let (old_state, new_state) = {
            let mut states = session_states()
                .lock()
                .map_err(|_| ConnectionError::Internal)?;
            let old_state = states
                .get(&source_owner)
                .cloned()
                .ok_or(ConnectionError::BrowseStale)?;
            let new_state = states
                .remove(&provisional_owner)
                .ok_or(ConnectionError::BrowseStale)?;
            states.insert(source_owner, Arc::clone(&new_state));
            (old_state, new_state)
        };
        let (old_generation, old_children) = {
            let old = old_state.lock().map_err(|_| ConnectionError::Internal)?;
            (
                old.generation,
                old.pane_terminals
                    .values()
                    .copied()
                    .filter(|id| *id != source_owner)
                    .collect::<Vec<_>>(),
            )
        };
        {
            let old = old_state.lock().map_err(|_| ConnectionError::Internal)?;
            let mut new = new_state.lock().map_err(|_| ConnectionError::Internal)?;
            carry_cleanup_warning(&old, &mut new);
            for native in new.pane_terminals.values_mut() {
                if *native == provisional_owner {
                    *native = source_owner;
                }
            }
            for pane in &mut new.snapshot.panes {
                if pane.terminal_id == provisional_owner {
                    pane.terminal_id = source_owner;
                }
            }
            new.generation = provisional_shared.generation;
        }

        // The map entry now owns the candidate actor under the source owner.
        // Its command loop is paused in this promotion command, so the backend
        // adapter remaps its private pane mapping immediately after return.
        provisional_shared.promote_terminal(source_owner);
        clear_terminal_data_plane_fence(source_owner);
        let cleanup_warning = new_state
            .lock()
            .map_err(|_| ConnectionError::Internal)?
            .cleanup_warning
            .clone();
        let entry = browse
            .as_mut()
            .filter(|entry| entry.token == token)
            .ok_or(ConnectionError::BrowseStale)?;
        entry.source_shared = Arc::clone(&provisional_shared);
        entry.provisional_owner = 0;
        entry.provisional_generation = provisional_shared.generation;
        entry.refresh_pending_from_revision = None;
        entry.phase = RuntimeBrowsePhase::Committed;
        entry.active_terminal_id = Some(source_owner);
        entry.cleanup_warning = cleanup_warning;
        clear_retired_browse_source(source_owner);

        // Child destruction calls back into terminal_destroyed, which needs
        // the browse serial and owner transition. Root/map promotion stays
        // atomic above; only cleanup runs after every lifecycle lock is free.
        drop(old_state);
        drop(owner_commit);
        drop(owner_serial);
        drop(browse);
        drop(serial);
        (old_generation, old_children)
    };
    destroy_stale_terminals(old_generation, old_children);
    Ok(())
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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
        registry::detach_transport(shared.terminal_id(), generation);
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
        if !shared.normal_control_allowed() {
            return Err(ConnectionError::BrowsePermissionDenied);
        }
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
            registry::detach_transport(shared.terminal_id(), generation);
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
    if !foreground {
        cancel_runtime_browse_for_source(terminal_id);
    }
    let shared = connections()
        .lock()
        .map_err(|_| ConnectionError::Internal)?
        .get(&terminal_id)
        .map(|entry| Arc::clone(&entry.shared));
    if let Some(shared) = shared {
        if !shared.normal_control_allowed() {
            return Err(ConnectionError::BrowsePermissionDenied);
        }
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
        if !shared.normal_control_allowed() {
            return Err(ConnectionError::BrowsePermissionDenied);
        }
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
    if !shared.normal_control_allowed() {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
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

fn prepare_provisional_profile(
    terminal_id: TerminalId,
    profile: &ConnectionProfile,
) -> Result<Vec<TerminalId>, ConnectionError> {
    let state = session_state(terminal_id);
    let mut state = state.lock().map_err(|_| ConnectionError::Internal)?;
    // A provisional root is always host-only. The selected backend/runtime in
    // the source profile is deliberately discarded so discovery cannot attach
    // by inheriting the legacy last-used hint.
    state.endpoint = Some(SessionEndpoint {
        host: profile.host.clone(),
        port: profile.port,
        username: profile.username.clone(),
        known_hosts_path: profile.known_hosts_path.clone(),
        backend: Backend::Tmux,
        runtime: None,
    });
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
    /// A short-lived host-only connection owned by the session switcher. It
    /// uses the same SSH/discovery actor as a normal host connect, but its
    /// root is guarded until the ordered commit promotes it.
    ProvisionalHost(ConnectOptions),
    /// The current connection's parsed, process-local credentials reused for
    /// a same-endpoint provisional browse. No secret crosses the bridge.
    ProvisionalProfile(ConnectionProfile),
    /// A transport loss after Ready may retry the selected binding. This mode
    /// is deliberately distinct from the public manual reconnect operation.
    AutomaticReconnect(ConnectionProfile),
    /// The UI-requested reconnect reauthenticates but must always rediscover
    /// and wait for an explicit runtime choice, even if one candidate exists.
    ManualReconnect(ConnectionProfile),
}

impl ConnectionStart {
    fn enters_picker(&self) -> bool {
        matches!(
            self,
            Self::Host(_)
                | Self::ProvisionalHost(_)
                | Self::ProvisionalProfile(_)
                | Self::ManualReconnect(_)
        )
    }

    fn is_provisional(&self) -> bool {
        matches!(self, Self::ProvisionalHost(_) | Self::ProvisionalProfile(_))
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
            Self::ProvisionalHost(options) => (
                &options.host,
                options.port,
                &options.username,
                &options.known_hosts_path,
            ),
            Self::ProvisionalProfile(profile) => (
                &profile.host,
                profile.port,
                &profile.username,
                &profile.known_hosts_path,
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
    registry::detach_transport(shared.terminal_id(), shared.generation);
    destroy_stale_terminals(shared.generation, stale_terminals);
}

fn start_connection(
    terminal_id: TerminalId,
    start: ConnectionStart,
) -> Result<(), ConnectionError> {
    let provisional = start.is_provisional();
    if !provisional {
        // A manual replacement/disconnect is an explicit source-owner
        // disposal boundary. Any uncommitted browse must be fenced before a
        // new actor can be installed for that owner.
        cancel_runtime_browse_for_source(terminal_id);
        clear_retired_browse_source(terminal_id);
    }
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
    if let Some(old) = old {
        let old_shared = Arc::clone(&old.shared);
        old.shared.invalidate_explicitly("runtime_replaced");
        // A controller/transport that never acknowledges the explicit
        // cleanup cannot safely retain the old generation. The shared helper
        // forces both cancellation and task abort before the new generation
        // is allowed to touch SessionState.
        let shutdown = finish_or_force_explicit_shutdown(runtime, old.shared, old.abort);
        if !reconnecting {
            // An explicit binding retirement is the last point at which the
            // old generation may decide what to do with its detailed cleanup
            // target. Automatic same-runtime recovery deliberately keeps a
            // surviving record for post-identity-verification reconciliation.
            let _ = retire_explicit_cleanup_result(&old_shared, shutdown, reconnecting);
        }
    }

    if !owner.install_allowed(ticket) {
        return Err(ConnectionError::RecoveryUnavailable);
    }

    let stale_terminals = match &start {
        #[cfg(test)]
        ConnectionStart::Options(options) => prepare_session_endpoint(terminal_id, options)?,
        ConnectionStart::Host(options) | ConnectionStart::ProvisionalHost(options) => {
            prepare_host_endpoint(terminal_id, options)?
        }
        ConnectionStart::ProvisionalProfile(profile) => {
            prepare_provisional_profile(terminal_id, profile)?
        }
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
    let shared = Arc::new(ConnectionShared::new_with_provisional(
        terminal_id,
        generation,
        host.to_owned(),
        port,
        known_hosts_path.to_owned(),
        provisional,
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
    {
        let mut state = shared
            .session
            .lock()
            .map_err(|_| ConnectionError::Internal)?;
        // Abort completion is asynchronous. Install the new generation and
        // discard the old actor's local cleanup authority in one operation.
        state.generation = generation;
        if reconnecting
            && let ConnectionStart::AutomaticReconnect(profile) = &start
            && let Some(identity) = profile.tmux_identity.as_ref()
            && let Some(record) = state.zoom_cleanup_record.as_mut()
            && record.endpoint == SessionEndpoint::from_profile(profile)
            && record.runtime == identity.epoch()
        {
            // The connection-scoped record is intentionally retained across
            // this same-runtime actor replacement, but its generation gate
            // moves atomically with the replacement. An old actor still
            // fails its state-generation checks and cannot clear it later.
            record.generation = generation;
        }
        if !reconnecting {
            state.meeterm_zoomed = false;
            state.meeterm_zoomed_window = None;
            state.meeterm_zoomed_pane = None;
            // A manual/fresh binding is never allowed to inherit a cleanup
            // target from another host, backend, or runtime. The old shared
            // actor already had its bounded cleanup opportunity above.
            state.zoom_cleanup_record = None;
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
        if !provisional {
            clear_terminal_data_plane_fence(terminal_id);
        }
    }
    destroy_stale_terminals(generation, stale_terminals);
    Ok(())
}

/// Abort the session and leave the terminal in remote mode with local echo
/// disabled.  The registry entry remains available for state polling.
pub fn disconnect_terminal(terminal_id: TerminalId) -> Result<(), ConnectionError> {
    registry::shared_terminal(terminal_id).map_err(map_terminal_error)?;
    cancel_runtime_browse_for_source(terminal_id);
    clear_retired_browse_source(terminal_id);
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
    clear_terminal_data_plane_fence(terminal_id);
    cancel_runtime_browse_for_source(terminal_id);
    clear_retired_browse_source(terminal_id);
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
    if !terminal_data_plane_allowed(terminal_id) {
        return Err(ConnectionError::BrowsePermissionDenied);
    }
    registry::send_bytes(terminal_id, bytes).map_err(map_terminal_error)
}

/// Native registry input/resize APIs use this guard before touching a
/// terminal. A provisional browse is intentionally invisible to those normal
/// data-plane operations; backend actors use their private registry helpers
/// only after the ordered commit has fenced the old owner.
pub(crate) fn terminal_data_plane_allowed(terminal_id: TerminalId) -> bool {
    let not_fenced = fenced_terminals()
        .lock()
        .map(|fenced| !fenced.contains(&terminal_id))
        .unwrap_or(false);
    not_fenced
        && connections()
            .lock()
            .map(|entries| {
                entries
                    .get(&terminal_id)
                    .is_none_or(|entry| !entry.shared.is_provisional())
            })
            .unwrap_or(false)
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
        // `start` may own a parsed provisional profile. Drop it before the
        // completion notification so explicit cancellation also establishes
        // the credential-release boundary for callers waiting on `finished`.
        drop(start);
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
        })
        | ConnectionStart::ProvisionalHost(ConnectOptions {
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
        ConnectionStart::ProvisionalProfile(profile) => profile,
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
        // A host-only browse needs to prove that a later re-tap is the same
        // live tmux binding without returning credentials to the platform.
        // Keep its already parsed credential identity in the native owner;
        // cancellation retires this provisional owner and releases it.
        if shared.is_provisional() {
            shared.set_profile(profile.clone());
        }
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
            .filter_map(|(_, id)| (id != shared.terminal_id()).then_some(id))
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
    registry::detach_transport(shared.terminal_id(), shared.generation);
    registry::reset_remote_binding(shared.terminal_id(), shared.generation)
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
    registry::detach_transport(shared.terminal_id(), shared.generation);
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

    fn run_lock_regression_subprocess(test_name: &str, marker: &str) -> bool {
        if std::env::var_os(marker).is_some() {
            return false;
        }
        let executable = std::env::current_exe().expect("current Rust test executable");
        let mut child = std::process::Command::new(executable)
            .args(["--exact", test_name, "--nocapture"])
            .env(marker, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("spawn isolated lock regression test");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match child.try_wait().expect("poll isolated regression test") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "isolated regression test failed: {status}"
                    );
                    return true;
                }
                None if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{test_name} did not complete within its 8-second bound");
                }
            }
        }
    }

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
    fn provisional_root_preserves_source_and_rejects_normal_mutations() {
        let source = registry::create_terminal(80, 24).expect("browse source terminal");
        let provisional = registry::create_terminal(80, 24).expect("browse provisional terminal");
        let source_generation = next_generation();
        let provisional_generation = next_generation();
        let source_shared = Arc::new(ConnectionShared::new(
            source,
            source_generation,
            "source.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-source-known-hosts"),
        ));
        let provisional_shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional,
            provisional_generation,
            "target.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-target-known-hosts"),
            true,
        ));
        {
            let mut state = source_shared.session.lock().expect("source state");
            state.generation = source_generation;
            state.operation_epoch = 41;
            state.runtime_operations_ready = true;
            state.selected_pane = Some(source);
        }
        source_shared.set_state(ConnectionState::Ready);
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("provisional state");
            state.generation = provisional_generation;
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: provisional_generation,
                discovery_revision: 7,
                ..RuntimeDiscoverySnapshot::default()
            };
        }
        provisional_shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        let before_source = source_shared
            .session
            .lock()
            .map(|state| {
                (
                    state.generation,
                    state.operation_epoch,
                    state.runtime_operations_ready,
                    state.selected_pane,
                )
            })
            .expect("source snapshot");
        let (sender, _receiver) = mpsc::channel(4);
        provisional_shared.set_commands(sender);
        let abort = runtime()
            .expect("browse test runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections()
            .lock()
            .expect("browse test connection registry")
            .insert(
                provisional,
                ConnectionEntry {
                    shared: Arc::clone(&provisional_shared),
                    abort,
                },
            );

        // The private bind arm is not a public bypass: even after the
        // ordered commit has armed it, normal APIs remain denied.
        provisional_shared.arm_provisional_bind();
        assert!(!provisional_shared.normal_control_allowed());
        assert!(provisional_shared.provisional_bind_allowed());
        assert_eq!(
            list_runtimes(provisional),
            Err(ConnectionError::BrowsePermissionDenied)
        );
        assert_eq!(
            select_runtime(provisional, "candidate"),
            Err(ConnectionError::BrowsePermissionDenied)
        );
        assert_eq!(
            create_runtime(provisional, Backend::Tmux, "new-session"),
            Err(ConnectionError::BrowsePermissionDenied)
        );
        assert_eq!(
            refresh_terminal(provisional),
            Err(ConnectionError::BrowsePermissionDenied)
        );
        assert_eq!(
            set_terminal_visible(provisional, false),
            Err(ConnectionError::BrowsePermissionDenied)
        );
        assert_eq!(
            set_automatic_reconnect(provisional, false),
            Err(ConnectionError::BrowsePermissionDenied)
        );
        assert!(matches!(
            registry::send_bytes(provisional, b"blocked"),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));
        assert!(matches!(
            registry::resize_terminal(provisional, 100, 30),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));
        assert!(matches!(
            registry::commit_utf8(provisional, b"blocked"),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));
        assert!(matches!(
            registry::scroll_lines(provisional, 1),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));
        assert!(matches!(
            registry::select_start(provisional, 0, 0),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));

        let after_source = source_shared
            .session
            .lock()
            .map(|state| {
                (
                    state.generation,
                    state.operation_epoch,
                    state.runtime_operations_ready,
                    state.selected_pane,
                )
            })
            .expect("source state after denied browse operations");
        assert_eq!(after_source, before_source);

        let entry = connections()
            .lock()
            .expect("browse test connection cleanup")
            .remove(&provisional)
            .expect("provisional connection entry");
        entry.shared.cancel();
        entry.abort.abort();
        registry::destroy_terminal(provisional);
        registry::destroy_terminal(source);
    }

    #[test]
    fn browse_identity_uses_owner_attempt_arc_and_exact_epochs() {
        let source = Arc::new(ConnectionShared::new(
            70_001,
            11,
            "source.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-identity-source"),
        ));
        let provisional = Arc::new(ConnectionShared::new_with_provisional(
            70_002,
            12,
            "target.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-identity-target"),
            true,
        ));
        let target_endpoint = SessionEndpoint {
            host: "target.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/browse-identity-target"),
            backend: Backend::Tmux,
            runtime: None,
        };
        let entry = RuntimeBrowseEntry {
            token: 91,
            source_owner: 70_001,
            source_generation: 11,
            source_epoch: 17,
            source_recovery: RecoverySnapshot::default(),
            source_runtime_operations_ready: true,
            source_retired: false,
            source_shared: Arc::clone(&source),
            provisional_owner: 70_002,
            provisional_generation: 12,
            provisional_shared: Arc::clone(&provisional),
            target_endpoint: target_endpoint.clone(),
            browse_generation: 92,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Ready,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        };
        assert!(entry.source_identity_matches(70_001, 11, 17, &source));
        assert!(!entry.source_identity_matches(70_001, 11, 18, &source));
        assert!(!entry.source_identity_matches(
            70_001,
            11,
            17,
            &Arc::new(ConnectionShared::new(
                70_001,
                11,
                "source.example.test".to_owned(),
                22,
                PathBuf::from("/tmp/browse-identity-source"),
            )),
        ));
        assert!(entry.provisional_identity_matches(70_002, 12, &provisional));
        assert!(!entry.provisional_identity_matches(70_002, 13, &provisional));
        assert_eq!(entry.target_endpoint, target_endpoint);
    }

    #[test]
    fn browse_source_allows_retained_recovery_without_changing_work_state() {
        let source = registry::create_terminal(80, 24).expect("recovery browse source");
        let generation = next_generation();
        let password = Arc::new(Zeroizing::new("recovery-browse-secret".to_owned()));
        let profile = ConnectionProfile {
            host: "recovery.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from(format!("/tmp/recovery-browse-{source}")),
            credentials: StoredCredentials::Password {
                password: Arc::clone(&password),
            },
            backend: Backend::Tmux,
            runtime: Some("retained".to_owned()),
            tmux_identity: Some(tmux::SessionIdentity {
                session_id: "$31".to_owned(),
                name: "retained".to_owned(),
                server_pid: 31,
                server_start_time: 1_700_000_031,
            }),
            herdr_executable: None,
        };
        let shared = Arc::new(ConnectionShared::new(
            source,
            generation,
            profile.host.clone(),
            profile.port,
            profile.known_hosts_path.clone(),
        ));
        let retained_topology = SessionSnapshot {
            windows: vec![WindowSnapshot {
                window_id: 4,
                name: "editor".to_owned(),
                panes: Vec::new(),
                selected: true,
                zoomed: false,
            }],
            panes: vec![PaneSnapshot {
                window_id: 4,
                pane_id: 12,
                terminal_id: source,
                window_name: "editor".to_owned(),
                active: true,
                selected: true,
                index: 0,
                columns: 80,
                rows: 24,
                pane_name: "shell".to_owned(),
                title: "shell".to_owned(),
            }],
            selected_pane: Some(12),
        };
        let retained_recovery = RecoverySnapshot {
            phase: RecoveryPhase::Stopped,
            reason: "runtime_identity_uncertain".to_owned(),
            attempt: 6,
            max_attempts: 6,
            confirmation_token: String::new(),
        };
        {
            let mut state = shared.session.lock().expect("recovery browse state");
            state.generation = generation;
            state.operation_epoch = 27;
            state.endpoint = Some(SessionEndpoint::from_profile(&profile));
            state.profile = Some(profile.clone());
            state.snapshot = retained_topology.clone();
            state.selected_pane = Some(12);
            state.pane_terminals.insert(12, source);
            state.recovery = retained_recovery.clone();
            state.runtime_operations_ready = false;
            state.terminal_input_ready = false;
        }
        registry::begin_remote(source, generation).expect("begin retained native Term");
        assert!(registry::feed_remote(
            source,
            generation,
            b"retained-work-screen"
        ));
        let retained_term = registry::snapshot(source).expect("retained Term snapshot");
        shared.set_state(ConnectionState::Failed);
        shared.finish(Err(FlowFailure::Network));
        let abort = runtime()
            .expect("recovery browse runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections()
            .lock()
            .expect("recovery browse connection map")
            .insert(
                source,
                ConnectionEntry {
                    shared: Arc::clone(&shared),
                    abort,
                },
            );

        let browsed =
            browse_source_profile(source).expect("explicit Change can browse retained work");
        assert!(Arc::ptr_eq(&browsed.shared, &shared));
        assert!(!browsed.retired);
        assert!(stored_credentials_share_identity(
            &browsed.profile.credentials,
            &profile.credentials
        ));
        assert_eq!(browsed.operation_epoch, 27);
        assert_eq!(browsed.recovery, retained_recovery);
        assert!(!browsed.runtime_operations_ready);
        let state = shared
            .session
            .lock()
            .expect("source after browse inspection");
        assert_eq!(state.snapshot, retained_topology);
        assert_eq!(state.selected_pane, Some(12));
        assert_eq!(state.recovery, browsed.recovery);
        assert_eq!(state.operation_epoch, 27);
        assert!(!state.runtime_operations_ready);
        drop(state);
        assert_eq!(
            registry::snapshot(source).expect("Term after browse inspection"),
            retained_term
        );

        let entry = connections()
            .lock()
            .expect("recovery browse cleanup map")
            .remove(&source)
            .expect("recovery browse source entry");
        entry.shared.cancel();
        entry.abort.abort();
        registry::destroy_terminal(source);
    }

    #[test]
    fn released_browse_source_can_retry_and_cancel_without_reviving_old_owner() {
        if run_lock_regression_subprocess(
            "ssh::tests::released_browse_source_can_retry_and_cancel_without_reviving_old_owner",
            "MEETERM_TEST_RELEASED_BROWSE_SOURCE_CHILD",
        ) {
            return;
        }

        fn publish_discovery(token: &str) -> RuntimeBrowseSnapshot {
            let provisional_owner = runtime_browse()
                .lock()
                .expect("retry browse entry")
                .as_ref()
                .filter(|entry| entry.token_string() == token)
                .expect("retry browse token")
                .provisional_owner;
            let shared = current_connection(provisional_owner).expect("retry browse connection");
            let candidate = RuntimeCandidate {
                id: format!("candidate-{token}"),
                backend: Backend::Tmux,
                name: "retry-target".to_owned(),
                state: RuntimeState::Running,
                selectable: true,
                suggested: false,
                error_code: None,
                error_message: None,
            };
            {
                let mut state = shared.session.lock().expect("retry browse discovery state");
                state.runtime_discovery = RuntimeDiscoverySnapshot {
                    connection_generation: shared.generation,
                    discovery_revision: 1,
                    tmux: RuntimeSection {
                        state: RuntimeSectionState::Success,
                        candidates: vec![candidate.clone()],
                        ..RuntimeSection::default()
                    },
                    herdr: RuntimeSection {
                        state: RuntimeSectionState::Empty,
                        ..RuntimeSection::default()
                    },
                };
                state.runtime_candidates.insert(
                    candidate.id.clone(),
                    RuntimeBinding::Tmux(tmux::SessionIdentity {
                        session_id: "$91".to_owned(),
                        name: candidate.name.clone(),
                        server_pid: 91,
                        server_start_time: 1_700_000_091,
                    }),
                );
            }
            let snapshot = runtime_browse_snapshot(token).expect("published retry discovery");
            assert_eq!(snapshot.phase, RuntimeBrowsePhase::Ready);
            assert_eq!(snapshot.discovery.tmux.candidates, vec![candidate]);
            snapshot
        }

        // Leave a local TCP listener open without replying to SSH handshakes,
        // so provisional actors cannot finish discovery before this test
        // publishes its bounded native discovery snapshot.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("pending SSH listener");
        let port = listener.local_addr().expect("listener endpoint").port();
        let source = registry::create_terminal(80, 24).expect("released source terminal");
        let source_generation = next_generation();
        let password = Arc::new(Zeroizing::new("retained-browse-secret".to_owned()));
        let profile = ConnectionProfile {
            host: "127.0.0.1".to_owned(),
            port,
            username: "source-user".to_owned(),
            known_hosts_path: PathBuf::from(format!("/tmp/released-browse-{source}")),
            credentials: StoredCredentials::Password {
                password: Arc::clone(&password),
            },
            backend: Backend::Tmux,
            runtime: Some("retained".to_owned()),
            tmux_identity: Some(tmux::SessionIdentity {
                session_id: "$90".to_owned(),
                name: "retained".to_owned(),
                server_pid: 90,
                server_start_time: 1_700_000_090,
            }),
            herdr_executable: None,
        };
        let source_shared = Arc::new(ConnectionShared::new(
            source,
            source_generation,
            profile.host.clone(),
            profile.port,
            profile.known_hosts_path.clone(),
        ));
        let retained_topology = SessionSnapshot {
            windows: vec![WindowSnapshot {
                window_id: 9,
                name: "editor".to_owned(),
                panes: Vec::new(),
                selected: true,
                zoomed: false,
            }],
            panes: vec![PaneSnapshot {
                window_id: 9,
                pane_id: 90,
                terminal_id: source,
                window_name: "editor".to_owned(),
                active: true,
                selected: true,
                index: 0,
                columns: 80,
                rows: 24,
                pane_name: "shell".to_owned(),
                title: "shell".to_owned(),
            }],
            selected_pane: Some(90),
        };
        {
            let mut state = source_shared.session.lock().expect("released source state");
            state.generation = source_generation;
            state.operation_epoch = 19;
            state.endpoint = Some(SessionEndpoint::from_profile(&profile));
            state.profile = Some(profile.clone());
            state.snapshot = retained_topology.clone();
            state.selected_pane = Some(90);
            state.pane_terminals.insert(90, source);
            state.runtime_operations_ready = true;
            state.terminal_input_ready = true;
        }
        registry::begin_remote(source, source_generation).expect("begin source Term");
        assert!(registry::feed_remote(
            source,
            source_generation,
            b"retained source screen"
        ));
        let retained_term = registry::snapshot(source).expect("source Term before release");
        source_shared.set_state(ConnectionState::Ready);
        let source_actor_shared = Arc::clone(&source_shared);
        let source_actor = runtime().expect("source actor runtime").spawn(async move {
            source_actor_shared.explicit_cleanup().await;
            source_actor_shared.finish(Ok(()));
        });
        let (source_commands, source_receiver) = mpsc::channel(4);
        drop(source_receiver);
        source_shared.set_commands(source_commands);

        let provisional_owner = registry::create_terminal(80, 24).expect("first retry target");
        let provisional_generation = next_generation();
        let target_endpoint = SessionEndpoint {
            host: profile.host.clone(),
            port,
            username: profile.username.clone(),
            known_hosts_path: profile.known_hosts_path.clone(),
            backend: Backend::Tmux,
            runtime: None,
        };
        let provisional_shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional_owner,
            provisional_generation,
            target_endpoint.host.clone(),
            target_endpoint.port,
            target_endpoint.known_hosts_path.clone(),
            true,
        ));
        let candidate = RuntimeCandidate {
            id: "first-target".to_owned(),
            backend: Backend::Tmux,
            name: "next".to_owned(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: false,
            error_code: None,
            error_message: None,
        };
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("first target state");
            state.generation = provisional_generation;
            state.endpoint = Some(target_endpoint.clone());
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: provisional_generation,
                discovery_revision: 1,
                tmux: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![candidate.clone()],
                    ..RuntimeSection::default()
                },
                herdr: RuntimeSection {
                    state: RuntimeSectionState::Empty,
                    ..RuntimeSection::default()
                },
            };
            state.runtime_candidates.insert(
                candidate.id.clone(),
                RuntimeBinding::Tmux(tmux::SessionIdentity {
                    session_id: "$92".to_owned(),
                    name: candidate.name.clone(),
                    server_pid: 90,
                    server_start_time: 1_700_000_090,
                }),
            );
        }
        provisional_shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        let (target_sender, target_receiver) = mpsc::channel(4);
        drop(target_receiver);
        provisional_shared.set_commands(target_sender);
        let target_abort = runtime()
            .expect("target actor runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections()
            .lock()
            .expect("released browse connection map")
            .extend([
                (
                    source,
                    ConnectionEntry {
                        shared: Arc::clone(&source_shared),
                        abort: source_actor.abort_handle(),
                    },
                ),
                (
                    provisional_owner,
                    ConnectionEntry {
                        shared: Arc::clone(&provisional_shared),
                        abort: target_abort,
                    },
                ),
            ]);
        let token = next_browse_token();
        let browse_generation = next_browse_token();
        *runtime_browse().lock().expect("first runtime browse slot") = Some(RuntimeBrowseEntry {
            token,
            source_owner: source,
            source_generation,
            source_epoch: 19,
            source_recovery: RecoverySnapshot::default(),
            source_runtime_operations_ready: true,
            source_retired: false,
            source_shared: Arc::clone(&source_shared),
            provisional_owner,
            provisional_generation,
            provisional_shared: Arc::clone(&provisional_shared),
            target_endpoint,
            browse_generation,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Ready,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        });

        assert_eq!(
            runtime_browse_commit(
                &token.to_string(),
                browse_generation,
                1,
                Some(&candidate.id),
                None
            ),
            Err(ConnectionError::RuntimeSelectionUnavailable),
            "the target's closed command channel fails after source release"
        );
        let (retired_epoch, retired_recovery) = {
            let state = source_shared.session.lock().expect("released source state");
            (state.operation_epoch, state.recovery.clone())
        };
        assert_eq!(retired_recovery.phase, RecoveryPhase::Stopped);
        assert_eq!(retired_recovery.reason, "runtime_changed");
        assert!(
            retired_browse_source_matches(source, &source_shared, retired_epoch, &retired_recovery,),
            "source capability is retained only in the browse anchor"
        );
        {
            let anchors = retired_browse_sources()
                .lock()
                .expect("released browse anchor map");
            let anchor = anchors.get(&source).expect("released browse anchor");
            assert_eq!(anchor.profile.host, profile.host);
            assert_eq!(anchor.profile.port, profile.port);
            assert!(stored_credentials_share_identity(
                &anchor.profile.credentials,
                &profile.credentials
            ));
        }
        assert!(
            connections()
                .lock()
                .expect("released source map")
                .get(&source)
                .is_none()
        );
        assert!(current_connection(source).is_err());
        assert!(terminal_data_plane_fenced(source));
        assert!(!terminal_data_plane_allowed(source));
        assert!(matches!(
            registry::send_bytes(source, b"must remain blocked"),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));
        let dimensions = registry::terminal_dimensions(source).expect("source dimensions");
        assert!(matches!(
            registry::resize_terminal(source, 100, 30),
            Err(crate::terminal::TerminalError::TransportClosed)
        ));
        assert_eq!(registry::terminal_dimensions(source), Ok(dimensions));
        assert!(refresh_terminal(source).is_err());
        assert_eq!(
            connection_snapshot(source)
                .expect("released source snapshot")
                .state,
            ConnectionState::Disconnected as u32
        );
        source_shared.cancel();
        {
            let state = source_shared.session.lock().expect("stale source state");
            assert_eq!(state.snapshot, retained_topology);
            assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
            assert_eq!(state.recovery.reason, "runtime_changed");
            assert!(!state.runtime_operations_ready);
            assert!(!state.terminal_input_ready);
        }
        assert_eq!(
            registry::snapshot(source).expect("retained source Term after release"),
            retained_term
        );

        // The current-server action reuses the tombstone's endpoint and
        // credential identity, publishes a fresh discovery snapshot, then
        // cancellation leaves the old work screen and anchor available.
        let same_server = runtime_browse_start_current(source)
            .expect("Retry can browse the released source's server");
        let same_discovery = publish_discovery(&same_server.token);
        assert_eq!(same_discovery.phase, RuntimeBrowsePhase::Ready);
        let same_owner = runtime_browse()
            .lock()
            .expect("same-server browse")
            .as_ref()
            .expect("same-server entry")
            .provisional_owner;
        let same_state = session_state(same_owner);
        let same_state = same_state.lock().expect("same-server profile state");
        assert_eq!(
            same_state
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.host.as_str()),
            Some(profile.host.as_str())
        );
        drop(same_state);
        runtime_browse_cancel(&same_server.token).expect("cancel same-server retry");
        assert_eq!(
            registry::snapshot(source).expect("source screen after retry cancel"),
            retained_term
        );
        assert!(retired_browse_source_matches(
            source,
            &source_shared,
            retired_epoch,
            &retired_recovery,
        ));

        let saved_profile = runtime_browse_start_with_options(
            source,
            ConnectOptions {
                host: "127.0.0.1".to_owned(),
                port,
                username: "saved-profile-user".to_owned(),
                credentials: AuthOptions::password("saved-profile-secret".to_owned()),
                known_hosts_path: PathBuf::from(format!("/tmp/released-profile-{source}")),
                backend: Backend::Tmux,
                runtime: None,
            },
        )
        .expect("saved-profile browse can use a retired terminal as source");
        assert_eq!(
            publish_discovery(&saved_profile.token).phase,
            RuntimeBrowsePhase::Ready
        );
        runtime_browse_cancel(&saved_profile.token).expect("cancel saved-profile retry");

        let rebrowsed = runtime_browse_start_current(source)
            .expect("cancelled browse leaves the retained source re-browsable");
        assert_eq!(
            publish_discovery(&rebrowsed.token).phase,
            RuntimeBrowsePhase::Ready
        );
        runtime_browse_cancel(&rebrowsed.token).expect("cancel final retry browse");
        assert_eq!(
            registry::snapshot(source).expect("retained screen after repeated retry"),
            retained_term
        );
        assert_ne!(
            source_shared
                .snapshot()
                .expect("released source actor snapshot")
                .state,
            ConnectionState::Ready as u32,
            "a browse retry cannot make the released source Ready again"
        );

        registry::destroy_terminal(source);
        assert!(
            !retired_browse_sources()
                .lock()
                .expect("browse anchor cleanup map")
                .contains_key(&source)
        );
        drop(listener);
    }

    #[test]
    fn browse_source_state_match_rejects_epoch_or_recovery_change() {
        let source = Arc::new(ConnectionShared::new(
            70_101,
            21,
            "recovery.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-stale-source"),
        ));
        let provisional = Arc::new(ConnectionShared::new_with_provisional(
            70_102,
            22,
            "target.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-stale-target"),
            true,
        ));
        let recovery = RecoverySnapshot {
            phase: RecoveryPhase::Reconnecting,
            reason: "reconnecting".to_owned(),
            attempt: 2,
            max_attempts: 6,
            confirmation_token: String::new(),
        };
        let entry = RuntimeBrowseEntry {
            token: 501,
            source_owner: 70_101,
            source_generation: 21,
            source_epoch: 17,
            source_recovery: recovery.clone(),
            source_runtime_operations_ready: false,
            source_retired: false,
            source_shared: source,
            provisional_owner: 70_102,
            provisional_generation: 22,
            provisional_shared: provisional,
            target_endpoint: SessionEndpoint {
                host: "target.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/browse-stale-target"),
                backend: Backend::Tmux,
                runtime: None,
            },
            browse_generation: 502,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Ready,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        };
        let retained = SessionState {
            generation: 21,
            operation_epoch: 17,
            snapshot: SessionSnapshot {
                windows: vec![WindowSnapshot {
                    window_id: 1,
                    name: "retained".to_owned(),
                    panes: Vec::new(),
                    selected: true,
                    zoomed: false,
                }],
                ..SessionSnapshot::default()
            },
            recovery: recovery.clone(),
            runtime_operations_ready: false,
            ..SessionState::default()
        };
        assert!(browse_source_state_matches(&entry, &retained));

        let mut advanced_epoch = SessionState {
            generation: 21,
            operation_epoch: 18,
            snapshot: retained.snapshot.clone(),
            recovery: recovery.clone(),
            runtime_operations_ready: false,
            ..SessionState::default()
        };
        assert!(!browse_source_state_matches(&entry, &advanced_epoch));
        advanced_epoch.operation_epoch = 17;
        advanced_epoch.recovery.phase = RecoveryPhase::Stopped;
        advanced_epoch.recovery.reason = "runtime_identity_uncertain".to_owned();
        assert!(!browse_source_state_matches(&entry, &advanced_epoch));
    }

    #[test]
    fn browse_noop_requires_exact_live_tmux_identity_and_preserves_source() {
        let source = registry::create_terminal(80, 24).expect("no-op source terminal");
        let provisional = registry::create_terminal(80, 24).expect("no-op provisional terminal");
        let source_generation = next_generation();
        let provisional_generation = next_generation();
        let secret = Arc::new(Zeroizing::new("same-native-credential".to_owned()));
        let identity = tmux::SessionIdentity {
            session_id: "$41".to_owned(),
            name: "live".to_owned(),
            server_pid: 41,
            server_start_time: 1_700_000_041,
        };
        let source_profile = ConnectionProfile {
            host: "same.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/noop-known-hosts"),
            credentials: StoredCredentials::Password {
                password: Arc::clone(&secret),
            },
            backend: Backend::Tmux,
            runtime: Some("live".to_owned()),
            tmux_identity: Some(identity.clone()),
            herdr_executable: None,
        };
        let target_profile = ConnectionProfile {
            runtime: None,
            tmux_identity: None,
            ..source_profile.clone()
        };
        let source_shared = Arc::new(ConnectionShared::new(
            source,
            source_generation,
            source_profile.host.clone(),
            source_profile.port,
            source_profile.known_hosts_path.clone(),
        ));
        let provisional_shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional,
            provisional_generation,
            target_profile.host.clone(),
            target_profile.port,
            target_profile.known_hosts_path.clone(),
            true,
        ));
        {
            let mut state = source_shared.session.lock().expect("no-op source state");
            state.generation = source_generation;
            state.operation_epoch = 4;
            state.endpoint = Some(SessionEndpoint::from_profile(&source_profile));
            state.profile = Some(source_profile.clone());
            state.runtime_operations_ready = true;
        }
        source_shared.set_state(ConnectionState::Ready);
        let candidate = RuntimeCandidate {
            id: "candidate-exact".to_owned(),
            backend: Backend::Tmux,
            name: "live".to_owned(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: false,
            error_code: None,
            error_message: None,
        };
        let target_endpoint = SessionEndpoint::from_profile(&target_profile);
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("no-op provisional state");
            state.generation = provisional_generation;
            state.endpoint = Some(target_endpoint.clone());
            state.profile = Some(target_profile.clone());
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: provisional_generation,
                discovery_revision: 8,
                tmux: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![candidate],
                    ..RuntimeSection::default()
                },
                ..RuntimeDiscoverySnapshot::default()
            };
            state.runtime_candidates.insert(
                "candidate-exact".to_owned(),
                RuntimeBinding::Tmux(identity.clone()),
            );
        }
        provisional_shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        let source_abort = runtime()
            .expect("no-op runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        let target_abort = runtime()
            .expect("no-op runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections().lock().expect("no-op connection map").extend([
            (
                source,
                ConnectionEntry {
                    shared: Arc::clone(&source_shared),
                    abort: source_abort,
                },
            ),
            (
                provisional,
                ConnectionEntry {
                    shared: Arc::clone(&provisional_shared),
                    abort: target_abort,
                },
            ),
        ]);
        let entry = RuntimeBrowseEntry {
            token: 601,
            source_owner: source,
            source_generation,
            source_epoch: 4,
            source_recovery: RecoverySnapshot::default(),
            source_runtime_operations_ready: true,
            source_retired: false,
            source_shared: Arc::clone(&source_shared),
            provisional_owner: provisional,
            provisional_generation,
            provisional_shared: Arc::clone(&provisional_shared),
            target_endpoint,
            browse_generation: 602,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Ready,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        };
        let target = RuntimeBrowseTarget::Candidate("candidate-exact".to_owned());
        let before_term = registry::snapshot(source).expect("source Term before no-op");

        assert!(
            runtime_browse_noop_is_confirmed(
                &entry,
                &source_shared,
                &provisional_shared,
                8,
                &target,
            )
            .expect("exact candidate check")
        );
        {
            let info = source_shared.info.lock().expect("source info after no-op");
            assert_eq!(info.state, ConnectionState::Ready);
            assert!(!info.finished);
        }
        {
            let state = source_shared
                .session
                .lock()
                .expect("source session after no-op");
            assert_eq!(state.operation_epoch, 4);
            assert!(state.runtime_operations_ready);
            assert_eq!(state.recovery.phase, RecoveryPhase::None);
        }
        assert!(!source_shared.is_cancelled());
        assert!(!source_shared.explicit_cleanup_requested());
        assert!(
            !fenced_terminals()
                .lock()
                .expect("no-op terminal fence set")
                .contains(&source)
        );
        assert_eq!(
            registry::snapshot(source).expect("source Term after no-op"),
            before_term
        );

        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("candidate mismatch state");
            state.runtime_candidates.insert(
                "candidate-exact".to_owned(),
                RuntimeBinding::Tmux(tmux::SessionIdentity {
                    session_id: "$42".to_owned(),
                    name: "live".to_owned(),
                    server_pid: 41,
                    server_start_time: 1_700_000_041,
                }),
            );
        }
        assert!(
            !runtime_browse_noop_is_confirmed(
                &entry,
                &source_shared,
                &provisional_shared,
                8,
                &target,
            )
            .expect("nonmatching candidate check")
        );
        assert_eq!(
            registry::snapshot(source).expect("source Term after mismatch"),
            before_term
        );

        let entries = {
            let mut connections = connections().lock().expect("no-op cleanup map");
            vec![
                connections.remove(&source),
                connections.remove(&provisional),
            ]
        };
        for entry in entries.into_iter().flatten() {
            entry.shared.cancel();
            entry.abort.abort();
        }
        registry::destroy_terminal(source);
        registry::destroy_terminal(provisional);
        drop(secret);
    }

    #[test]
    fn browse_warning_carry_over_does_not_replace_new_target_warning() {
        let old_warning = CleanupWarning {
            id: "3".to_owned(),
            code: workspace::CLEANUP_WARNING_CODE.to_owned(),
            message: workspace::CLEANUP_WARNING_MESSAGE.to_owned(),
        };
        let old = SessionState {
            cleanup_warning: Some(old_warning.clone()),
            cleanup_warning_result: Some(CleanupWarningResult { result_id: 8 }),
            ..SessionState::default()
        };
        let mut new = SessionState::default();
        carry_cleanup_warning(&old, &mut new);
        assert_eq!(new.cleanup_warning, Some(old_warning.clone()));
        assert_eq!(
            new.cleanup_warning_result,
            Some(CleanupWarningResult { result_id: 8 })
        );

        let replacement = CleanupWarning {
            id: "4".to_owned(),
            code: workspace::CLEANUP_WARNING_CODE.to_owned(),
            message: workspace::CLEANUP_WARNING_MESSAGE.to_owned(),
        };
        new.cleanup_warning = Some(replacement.clone());
        new.cleanup_warning_result = Some(CleanupWarningResult { result_id: 9 });
        carry_cleanup_warning(&old, &mut new);
        assert_eq!(new.cleanup_warning, Some(replacement));
        assert_eq!(
            new.cleanup_warning_result,
            Some(CleanupWarningResult { result_id: 9 })
        );
    }

    #[test]
    fn browse_source_fence_rejects_stale_epoch_before_revoking_gates() {
        let source = registry::create_terminal(80, 24).expect("fence source terminal");
        let generation = next_generation();
        let shared = ConnectionShared::new(
            source,
            generation,
            "source.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-fence-known-hosts"),
        );
        {
            let mut state = shared.session.lock().expect("fence source state");
            state.generation = generation;
            state.operation_epoch = 9;
            state.runtime_operations_ready = true;
            state.terminal_input_ready = true;
            state.recovery.phase = RecoveryPhase::None;
        }

        let expected_recovery = RecoverySnapshot::default();
        assert_eq!(
            shared.fence_for_runtime_switch(8, &expected_recovery),
            Err(ConnectionError::BrowseStale)
        );
        {
            let state = shared.session.lock().expect("state after stale fence");
            assert_eq!(state.operation_epoch, 9);
            assert!(state.runtime_operations_ready);
            assert!(state.terminal_input_ready);
            assert_eq!(state.recovery.phase, RecoveryPhase::None);
        }
        assert!(!shared.explicit_cleanup_requested());

        shared
            .fence_for_runtime_switch(9, &expected_recovery)
            .expect("exact source fence");
        {
            let state = shared.session.lock().expect("state after source fence");
            assert_eq!(state.operation_epoch, 10);
            assert!(!state.runtime_operations_ready);
            assert!(!state.terminal_input_ready);
            assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
            assert_eq!(state.recovery.reason, "runtime_changed");
        }
        assert!(shared.explicit_cleanup_requested());
        assert!(
            fenced_terminals()
                .lock()
                .expect("fence set")
                .contains(&source)
        );
        assert_eq!(
            shared.snapshot().expect("fenced snapshot").state,
            ConnectionState::Closing as u32
        );
        registry::destroy_terminal(source);
        assert!(
            !fenced_terminals()
                .lock()
                .expect("cleared fence set")
                .contains(&source)
        );
    }

    #[test]
    fn recovery_switch_fences_retained_state_before_bounded_shutdown_even_if_finished() {
        let source = registry::create_terminal(80, 24).expect("recovery commit source");
        let generation = next_generation();
        let shared = Arc::new(ConnectionShared::new(
            source,
            generation,
            "recovery-commit.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/recovery-commit-known-hosts"),
        ));
        let topology = SessionSnapshot {
            windows: vec![WindowSnapshot {
                window_id: 8,
                name: "retained".to_owned(),
                panes: Vec::new(),
                selected: true,
                zoomed: false,
            }],
            selected_pane: Some(19),
            ..SessionSnapshot::default()
        };
        let recovery = RecoverySnapshot {
            phase: RecoveryPhase::Stopped,
            reason: "runtime_identity_uncertain".to_owned(),
            attempt: 6,
            max_attempts: 6,
            confirmation_token: String::new(),
        };
        {
            let mut state = shared.session.lock().expect("recovery commit state");
            state.generation = generation;
            state.operation_epoch = 9;
            state.snapshot = topology.clone();
            state.selected_pane = Some(19);
            state.recovery = recovery.clone();
            state.runtime_operations_ready = false;
            state.terminal_input_ready = false;
        }
        registry::begin_remote(source, generation).expect("begin recovery source Term");
        assert!(registry::feed_remote(
            source,
            generation,
            b"stale retained screen"
        ));
        let retained_term = registry::snapshot(source).expect("retained screen before switch");
        shared.set_state(ConnectionState::Failed);
        shared.finish(Err(FlowFailure::Network));
        let abort = runtime()
            .expect("recovery commit runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();

        // The ordered commit's first boundary is the local fence. A stopped,
        // already-finished recovery actor still has a retained owner that can
        // be explicitly retired without waiting for a new shutdown event.
        shared
            .fence_for_runtime_switch(9, &recovery)
            .expect("fence a finished retained source");
        {
            let state = shared.session.lock().expect("state after recovery fence");
            assert_eq!(state.operation_epoch, 10);
            assert_eq!(state.snapshot, topology);
            assert_eq!(state.selected_pane, Some(19));
            assert_eq!(state.recovery.phase, RecoveryPhase::Stopped);
            assert_eq!(state.recovery.reason, "runtime_changed");
            assert!(!state.runtime_operations_ready);
            assert!(!state.terminal_input_ready);
        }
        assert_eq!(
            registry::snapshot(source).expect("Term after recovery fence"),
            retained_term
        );
        let shutdown = finish_or_force_explicit_shutdown(
            runtime().expect("recovery commit runtime"),
            Arc::clone(&shared),
            abort,
        );
        assert!(
            shutdown.finished,
            "finished actor shutdown is immediately bounded"
        );
        assert!(!shared.is_cancelled());
        registry::destroy_terminal(source);
    }

    #[test]
    fn provisional_handle_promotion_replaces_only_the_public_root() {
        let source = registry::create_terminal(80, 24).expect("promotion source terminal");
        let provisional =
            registry::create_terminal(80, 24).expect("promotion provisional terminal");
        let source_root = registry::shared_terminal(source).expect("source root");
        let provisional_root = registry::shared_terminal(provisional).expect("provisional root");
        registry::promote_terminal(provisional, source).expect("promote native root");
        assert!(Arc::ptr_eq(
            &registry::shared_terminal(source).expect("promoted root"),
            &provisional_root
        ));
        assert!(registry::shared_terminal(provisional).is_err());
        assert!(!Arc::ptr_eq(
            &registry::shared_terminal(source).expect("promoted root remains live"),
            &source_root
        ));
        registry::destroy_terminal(source);
    }

    #[test]
    fn late_browse_cleanup_cannot_cancel_a_promoted_actor() {
        let source = registry::create_terminal(80, 24).expect("late-cleanup source terminal");
        let provisional =
            registry::create_terminal(80, 24).expect("late-cleanup provisional terminal");
        let shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional,
            next_generation(),
            "late-cleanup.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/late-cleanup-known-hosts"),
            true,
        ));

        registry::promote_terminal(provisional, source).expect("promote late-cleanup root");
        shared.promote_terminal(source);
        assert!(retire_runtime_browse_target_by_id(provisional, &shared).is_empty());

        assert!(!shared.is_cancelled());
        assert_eq!(shared.terminal_id(), source);
        registry::destroy_terminal(source);
    }

    #[test]
    fn browse_promotion_destroys_old_children_after_releasing_locks() {
        if run_lock_regression_subprocess(
            "ssh::tests::browse_promotion_destroys_old_children_after_releasing_locks",
            "MEETERM_TEST_BROWSE_PROMOTION_CHILD",
        ) {
            return;
        }

        let source_owner = registry::create_terminal(80, 24).expect("promotion source root");
        let provisional_owner =
            registry::create_terminal(80, 24).expect("promotion provisional root");
        let old_child_one = registry::create_terminal(80, 24).expect("first old pane terminal");
        let old_child_two = registry::create_terminal(80, 24).expect("second old pane terminal");
        let selected_child =
            registry::create_terminal(80, 24).expect("promoted selected pane terminal");
        let source_generation = next_generation();
        let provisional_generation = next_generation();
        let provisional_shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional_owner,
            provisional_generation,
            "target.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-promotion-target-known-hosts"),
            true,
        ));
        let source_state = session_state(source_owner);
        *source_state.lock().expect("old source session state") = SessionState {
            generation: source_generation,
            snapshot: SessionSnapshot {
                windows: vec![WindowSnapshot {
                    window_id: 1,
                    name: "old workspace".to_owned(),
                    panes: Vec::new(),
                    selected: true,
                    zoomed: false,
                }],
                panes: vec![
                    PaneSnapshot {
                        window_id: 1,
                        pane_id: 11,
                        terminal_id: old_child_one,
                        window_name: "old workspace".to_owned(),
                        active: true,
                        selected: false,
                        index: 0,
                        columns: 80,
                        rows: 24,
                        pane_name: "one".to_owned(),
                        title: "one".to_owned(),
                    },
                    PaneSnapshot {
                        window_id: 1,
                        pane_id: 12,
                        terminal_id: old_child_two,
                        window_name: "old workspace".to_owned(),
                        active: true,
                        selected: true,
                        index: 1,
                        columns: 80,
                        rows: 24,
                        pane_name: "two".to_owned(),
                        title: "two".to_owned(),
                    },
                ],
                selected_pane: Some(12),
            },
            pane_terminals: HashMap::from([(11, old_child_one), (12, old_child_two)]),
            selected_pane: Some(12),
            ..SessionState::default()
        };
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("provisional session state");
            *state = SessionState {
                generation: provisional_generation,
                snapshot: SessionSnapshot {
                    windows: vec![WindowSnapshot {
                        window_id: 2,
                        name: "new workspace".to_owned(),
                        panes: Vec::new(),
                        selected: true,
                        zoomed: false,
                    }],
                    panes: vec![PaneSnapshot {
                        window_id: 2,
                        pane_id: 22,
                        terminal_id: selected_child,
                        window_name: "new workspace".to_owned(),
                        active: true,
                        selected: true,
                        index: 0,
                        columns: 80,
                        rows: 24,
                        pane_name: "selected".to_owned(),
                        title: "selected".to_owned(),
                    }],
                    selected_pane: Some(22),
                },
                pane_terminals: HashMap::from([
                    (provisional_owner, provisional_owner),
                    (22, selected_child),
                ]),
                selected_pane: Some(22),
                runtime_operations_ready: true,
                terminal_input_ready: true,
                ..SessionState::default()
            };
        }
        registry::begin_remote(provisional_owner, provisional_generation)
            .expect("begin provisional root Term");
        assert!(registry::feed_remote(
            provisional_owner,
            provisional_generation,
            b"promoted root screen"
        ));
        let promoted_root = registry::snapshot(provisional_owner).expect("target root Term");
        provisional_shared.set_state(ConnectionState::Ready);
        provisional_shared.ready_once.store(true, Ordering::Release);
        let target_endpoint = SessionEndpoint {
            host: "target.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/browse-promotion-target-known-hosts"),
            backend: Backend::Tmux,
            runtime: None,
        };
        let actor = runtime()
            .expect("promotion runtime")
            .spawn(std::future::pending::<()>());
        connections()
            .lock()
            .expect("promotion connection map")
            .insert(
                provisional_owner,
                ConnectionEntry {
                    shared: Arc::clone(&provisional_shared),
                    abort: actor.abort_handle(),
                },
            );
        let source_shared = Arc::new(ConnectionShared::new(
            source_owner,
            source_generation,
            "source.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-promotion-source-known-hosts"),
        ));
        *runtime_browse().lock().expect("promotion browse slot") = Some(RuntimeBrowseEntry {
            token: 71,
            source_owner,
            source_generation,
            source_epoch: 1,
            source_recovery: RecoverySnapshot::default(),
            source_runtime_operations_ready: true,
            source_retired: false,
            source_shared,
            provisional_owner,
            provisional_generation,
            provisional_shared: Arc::clone(&provisional_shared),
            target_endpoint,
            browse_generation: 72,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Committing,
            target: Some(RuntimeBrowseTarget::Candidate("candidate".to_owned())),
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        });

        promote_runtime_browse(
            71,
            source_owner,
            provisional_owner,
            Arc::clone(&provisional_shared),
        )
        .expect("multi-pane browse promotion completes");

        assert_eq!(
            registry::snapshot(source_owner).expect("promoted source root Term"),
            promoted_root
        );
        assert!(registry::shared_terminal(provisional_owner).is_err());
        assert!(registry::shared_terminal(old_child_one).is_err());
        assert!(registry::shared_terminal(old_child_two).is_err());
        assert!(registry::shared_terminal(selected_child).is_ok());
        let promoted_state = session_state(source_owner);
        let promoted_state = promoted_state.lock().expect("promoted session state");
        assert_eq!(promoted_state.selected_pane, Some(22));
        assert_eq!(promoted_state.snapshot.selected_pane, Some(22));
        assert_eq!(promoted_state.snapshot.panes[0].terminal_id, selected_child);
        assert!(
            promoted_state
                .pane_terminals
                .values()
                .any(|id| *id == source_owner)
        );
        drop(promoted_state);
        let browse = runtime_browse().lock().expect("committed browse record");
        assert_eq!(
            browse.as_ref().map(|entry| entry.phase),
            Some(RuntimeBrowsePhase::Committed)
        );
        drop(browse);
        assert!(runtime_browse_serial().try_lock().is_ok());
        let owner = owner_transition(source_owner).expect("promoted owner transition");
        assert!(owner.serial.try_lock().is_ok());
        assert!(owner.commit.try_lock().is_ok());

        runtime_browse()
            .lock()
            .expect("promotion cleanup browse")
            .take();
        if let Some(entry) = connections()
            .lock()
            .expect("promotion cleanup connection map")
            .remove(&source_owner)
        {
            entry.shared.cancel();
            entry.abort.abort();
        }
        session_states()
            .lock()
            .expect("promotion cleanup session map")
            .remove(&source_owner);
        registry::destroy_terminal(source_owner);
        registry::destroy_terminal(selected_child);
    }

    #[test]
    fn browse_retirement_destroys_acquired_children_after_releasing_locks() {
        if run_lock_regression_subprocess(
            "ssh::tests::browse_retirement_destroys_acquired_children_after_releasing_locks",
            "MEETERM_TEST_BROWSE_RETIREMENT_CHILD",
        ) {
            return;
        }

        let source_owner = registry::create_terminal(80, 24).expect("retirement source root");
        let provisional_owner =
            registry::create_terminal(80, 24).expect("retirement provisional root");
        let acquired_child =
            registry::create_terminal(80, 24).expect("acquired provisional pane terminal");
        let source_generation = next_generation();
        let provisional_generation = next_generation();
        let source_shared = Arc::new(ConnectionShared::new(
            source_owner,
            source_generation,
            "source.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-retirement-source-known-hosts"),
        ));
        let provisional_shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional_owner,
            provisional_generation,
            "target.example.test".to_owned(),
            22,
            PathBuf::from("/tmp/browse-retirement-target-known-hosts"),
            true,
        ));
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("partial target session state");
            *state = SessionState {
                generation: provisional_generation,
                endpoint: Some(SessionEndpoint {
                    host: "target.example.test".to_owned(),
                    port: 22,
                    username: "fixture".to_owned(),
                    known_hosts_path: PathBuf::from("/tmp/browse-retirement-target-known-hosts"),
                    backend: Backend::Tmux,
                    runtime: Some("meeterm".to_owned()),
                }),
                snapshot: SessionSnapshot {
                    panes: vec![PaneSnapshot {
                        window_id: 3,
                        pane_id: 31,
                        terminal_id: acquired_child,
                        window_name: "partially bound".to_owned(),
                        active: true,
                        selected: true,
                        index: 0,
                        columns: 80,
                        rows: 24,
                        pane_name: "child".to_owned(),
                        title: "child".to_owned(),
                    }],
                    selected_pane: Some(31),
                    ..SessionSnapshot::default()
                },
                pane_terminals: HashMap::from([
                    (provisional_owner, provisional_owner),
                    (31, acquired_child),
                ]),
                selected_pane: Some(31),
                ..SessionState::default()
            };
        }
        provisional_shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        let actor = runtime()
            .expect("retirement runtime")
            .spawn(std::future::pending::<()>());
        let actor_abort = actor.abort_handle();
        provisional_shared.finish(Ok(()));
        connections()
            .lock()
            .expect("retirement connection map")
            .insert(
                provisional_owner,
                ConnectionEntry {
                    shared: Arc::clone(&provisional_shared),
                    abort: actor_abort.clone(),
                },
            );
        *runtime_browse().lock().expect("retirement browse slot") = Some(RuntimeBrowseEntry {
            token: 81,
            source_owner,
            source_generation,
            source_epoch: 1,
            source_recovery: RecoverySnapshot::default(),
            source_runtime_operations_ready: true,
            source_retired: false,
            source_shared,
            provisional_owner,
            provisional_generation,
            provisional_shared: Arc::clone(&provisional_shared),
            target_endpoint: SessionEndpoint {
                host: "target.example.test".to_owned(),
                port: 22,
                username: "fixture".to_owned(),
                known_hosts_path: PathBuf::from("/tmp/browse-retirement-target-known-hosts"),
                backend: Backend::Tmux,
                runtime: None,
            },
            browse_generation: 82,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Ready,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        });

        runtime_browse_cancel("81").expect("retire partially bound target");

        assert!(registry::shared_terminal(provisional_owner).is_err());
        assert!(registry::shared_terminal(acquired_child).is_err());
        assert!(
            !connections()
                .lock()
                .expect("retirement map after cleanup")
                .contains_key(&provisional_owner)
        );
        assert!(
            !session_states()
                .lock()
                .expect("retirement state after cleanup")
                .contains_key(&provisional_owner)
        );
        assert!(
            runtime_browse()
                .lock()
                .expect("retirement slot after cancel")
                .is_none()
        );
        assert!(runtime_browse_serial().try_lock().is_ok());
        let owner = owner_transition(provisional_owner).expect("retired owner transition");
        assert!(owner.serial.try_lock().is_ok());
        assert!(owner.commit.try_lock().is_ok());

        actor_abort.abort();
        registry::destroy_terminal(source_owner);
    }

    #[test]
    fn queued_browse_refresh_refuses_old_revision_before_source_fence() {
        if run_lock_regression_subprocess(
            "ssh::tests::queued_browse_refresh_refuses_old_revision_before_source_fence",
            "MEETERM_TEST_BROWSE_REFRESH_CHILD",
        ) {
            return;
        }

        let source_owner = registry::create_terminal(80, 24).expect("refresh source root");
        let provisional_owner =
            registry::create_terminal(80, 24).expect("refresh provisional root");
        let source_generation = next_generation();
        let provisional_generation = next_generation();
        let source_secret = Arc::new(Zeroizing::new("source-fixture".to_owned()));
        let source_profile = ConnectionProfile {
            host: "source.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/browse-refresh-source-known-hosts"),
            credentials: StoredCredentials::Password {
                password: Arc::clone(&source_secret),
            },
            backend: Backend::Tmux,
            runtime: Some("source".to_owned()),
            tmux_identity: Some(tmux::SessionIdentity {
                session_id: "$10".to_owned(),
                name: "source".to_owned(),
                server_pid: 10,
                server_start_time: 1_700_000_010,
            }),
            herdr_executable: None,
        };
        let target_endpoint = SessionEndpoint {
            host: "target.example.test".to_owned(),
            port: 22,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from("/tmp/browse-refresh-target-known-hosts"),
            backend: Backend::Tmux,
            runtime: None,
        };
        let source_shared = Arc::new(ConnectionShared::new(
            source_owner,
            source_generation,
            source_profile.host.clone(),
            source_profile.port,
            source_profile.known_hosts_path.clone(),
        ));
        let provisional_shared = Arc::new(ConnectionShared::new_with_provisional(
            provisional_owner,
            provisional_generation,
            target_endpoint.host.clone(),
            target_endpoint.port,
            target_endpoint.known_hosts_path.clone(),
            true,
        ));
        let source_term = b"source still interactive after queued refresh";
        registry::begin_remote(source_owner, source_generation).expect("begin source Term");
        assert!(registry::feed_remote(
            source_owner,
            source_generation,
            source_term
        ));
        let source_term_before = registry::snapshot(source_owner).expect("source Term snapshot");
        {
            let mut state = source_shared.session.lock().expect("refresh source state");
            state.generation = source_generation;
            state.operation_epoch = 13;
            state.endpoint = Some(SessionEndpoint::from_profile(&source_profile));
            state.profile = Some(source_profile.clone());
            state.selected_pane = Some(source_owner);
            state.runtime_operations_ready = true;
            state.terminal_input_ready = true;
        }
        source_shared.set_state(ConnectionState::Ready);
        source_shared.ready_once.store(true, Ordering::Release);
        source_shared
            .session
            .lock()
            .expect("ready source input gate")
            .terminal_input_ready = true;
        let (source_sender, _source_receiver) = mpsc::channel(4);
        source_shared.set_commands(source_sender);
        let source_actor_shared = Arc::clone(&source_shared);
        let source_actor = runtime().expect("refresh test runtime").spawn(async move {
            source_actor_shared.explicit_cleanup().await;
            source_actor_shared.finish(Ok(()));
        });

        let old_candidate = RuntimeCandidate {
            id: "candidate-old-revision".to_owned(),
            backend: Backend::Tmux,
            name: "old-list-session".to_owned(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: false,
            error_code: None,
            error_message: None,
        };
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("refresh provisional state");
            state.generation = provisional_generation;
            state.endpoint = Some(target_endpoint.clone());
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: provisional_generation,
                discovery_revision: 7,
                tmux: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![old_candidate.clone()],
                    ..RuntimeSection::default()
                },
                herdr: RuntimeSection {
                    state: RuntimeSectionState::Empty,
                    ..RuntimeSection::default()
                },
            };
            state.runtime_candidates.insert(
                old_candidate.id.clone(),
                RuntimeBinding::Tmux(tmux::SessionIdentity {
                    session_id: "$20".to_owned(),
                    name: old_candidate.name.clone(),
                    server_pid: 20,
                    server_start_time: 1_700_000_020,
                }),
            );
        }
        provisional_shared.set_state(ConnectionState::AwaitingRuntimeSelection);
        let (target_sender, mut target_receiver) = mpsc::channel(4);
        provisional_shared.set_commands(target_sender);
        let provisional_actor = runtime()
            .expect("refresh target actor runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        let browse_generation = 92;
        *runtime_browse().lock().expect("refresh browse slot") = Some(RuntimeBrowseEntry {
            token: 91,
            source_owner,
            source_generation,
            source_epoch: 13,
            source_recovery: RecoverySnapshot::default(),
            source_runtime_operations_ready: true,
            source_retired: false,
            source_shared: Arc::clone(&source_shared),
            provisional_owner,
            provisional_generation,
            provisional_shared: Arc::clone(&provisional_shared),
            target_endpoint: target_endpoint.clone(),
            browse_generation,
            refresh_pending_from_revision: None,
            phase: RuntimeBrowsePhase::Ready,
            target: None,
            error_code: String::new(),
            error_message: String::new(),
            cleanup_warning: None,
            active_terminal_id: None,
        });
        connections()
            .lock()
            .expect("refresh connection map")
            .extend([
                (
                    source_owner,
                    ConnectionEntry {
                        shared: Arc::clone(&source_shared),
                        abort: source_actor.abort_handle(),
                    },
                ),
                (
                    provisional_owner,
                    ConnectionEntry {
                        shared: Arc::clone(&provisional_shared),
                        abort: provisional_actor.clone(),
                    },
                ),
            ]);

        let target_discovery = runtime_discovery_snapshot(provisional_owner)
            .expect("paused target discovery snapshot");
        assert_eq!(
            target_discovery.connection_generation,
            provisional_generation
        );
        assert_eq!(target_discovery.discovery_revision, 7);
        assert!(runtime_discovery_is_ready(&target_discovery));
        assert!(provisional_shared.is_provisional());
        assert!(!provisional_shared.is_cancelled());
        assert!(provisional_shared.command_sender().is_some());
        runtime_browse_refresh("91").expect("queue refresh on paused provisional actor");
        let refreshing = runtime_browse_snapshot("91").expect("refresh-pending snapshot");
        assert_eq!(refreshing.phase, RuntimeBrowsePhase::Discovering);
        assert_eq!(refreshing.discovery_revision, 8);
        assert!(refreshing.discovery.tmux.candidates.is_empty());
        assert!(matches!(
            target_receiver
                .try_recv()
                .expect("queued refresh request")
                .command,
            ControlCommand::RefreshRuntimes
        ));

        assert_eq!(
            runtime_browse_commit("91", browse_generation, 7, Some(&old_candidate.id), None,),
            Err(ConnectionError::BrowseStale)
        );
        assert!(
            connections()
                .lock()
                .expect("source map before fresh publication")
                .get(&source_owner)
                .is_some_and(|entry| Arc::ptr_eq(&entry.shared, &source_shared))
        );
        assert!(source_shared.command_sender().is_some());
        assert!(!source_shared.is_cancelled());
        assert!(!source_shared.explicit_cleanup_requested());
        {
            let info = source_shared
                .info
                .lock()
                .expect("source info after stale commit");
            assert_eq!(info.state, ConnectionState::Ready);
            assert!(!info.finished);
        }
        {
            let state = source_shared
                .session
                .lock()
                .expect("source state after stale commit");
            assert_eq!(state.operation_epoch, 13);
            assert!(state.runtime_operations_ready);
            assert!(state.terminal_input_ready);
        }
        assert_eq!(
            registry::snapshot(source_owner).expect("source Term after stale commit"),
            source_term_before
        );

        let fresh_candidate = RuntimeCandidate {
            id: "candidate-fresh-revision".to_owned(),
            backend: Backend::Tmux,
            name: "fresh-list-session".to_owned(),
            state: RuntimeState::Running,
            selectable: true,
            suggested: false,
            error_code: None,
            error_message: None,
        };
        {
            let mut state = provisional_shared
                .session
                .lock()
                .expect("publish fresh discovery revision");
            state.runtime_candidates.clear();
            state.runtime_candidates.insert(
                fresh_candidate.id.clone(),
                RuntimeBinding::Tmux(tmux::SessionIdentity {
                    session_id: "$21".to_owned(),
                    name: fresh_candidate.name.clone(),
                    server_pid: 20,
                    server_start_time: 1_700_000_020,
                }),
            );
            state.runtime_discovery = RuntimeDiscoverySnapshot {
                connection_generation: provisional_generation,
                discovery_revision: 8,
                tmux: RuntimeSection {
                    state: RuntimeSectionState::Success,
                    candidates: vec![fresh_candidate.clone()],
                    ..RuntimeSection::default()
                },
                herdr: RuntimeSection {
                    state: RuntimeSectionState::Empty,
                    ..RuntimeSection::default()
                },
            };
        }
        let fresh_snapshot = runtime_browse_snapshot("91").expect("published fresh snapshot");
        assert_eq!(fresh_snapshot.phase, RuntimeBrowsePhase::Ready);
        assert_eq!(fresh_snapshot.discovery_revision, 8);
        assert_eq!(
            fresh_snapshot.discovery.tmux.candidates,
            vec![fresh_candidate.clone()]
        );
        runtime_browse_commit("91", browse_generation, 8, Some(&fresh_candidate.id), None)
            .expect("fresh-revision candidate can be selected");
        assert!(source_shared.explicit_cleanup_requested());
        assert!(
            connections()
                .lock()
                .expect("source removed after fresh commit")
                .get(&source_owner)
                .is_none()
        );
        assert!(matches!(
            target_receiver
                .try_recv()
                .expect("fresh candidate bind request")
                .command,
            ControlCommand::SelectRuntime { candidate_id } if candidate_id == fresh_candidate.id
        ));

        provisional_shared.finish(Ok(()));
        runtime_browse_cancel("91").expect("clean up paused target after fresh commit");
        provisional_actor.abort();
        registry::destroy_terminal(source_owner);
    }

    #[test]
    fn browse_cancel_replaces_at_most_one_target_and_releases_target_credentials() {
        let source = registry::create_terminal(80, 24).expect("cancel source terminal");
        let generation = next_generation();
        let password = Arc::new(Zeroizing::new("browse-only-secret".to_owned()));
        let profile = ConnectionProfile {
            host: "127.0.0.1".to_owned(),
            port: 1,
            username: "fixture".to_owned(),
            known_hosts_path: PathBuf::from(format!("/tmp/browse-cancel-known-hosts-{source}")),
            credentials: StoredCredentials::Password {
                password: Arc::clone(&password),
            },
            backend: Backend::Tmux,
            runtime: Some("old-runtime".to_owned()),
            tmux_identity: None,
            herdr_executable: None,
        };
        let weak_password = Arc::downgrade(&password);
        let source_shared = Arc::new(ConnectionShared::new(
            source,
            generation,
            profile.host.clone(),
            profile.port,
            profile.known_hosts_path.clone(),
        ));
        {
            let mut state = source_shared.session.lock().expect("cancel source state");
            state.generation = generation;
            state.endpoint = Some(SessionEndpoint::from_profile(&profile));
            state.profile = Some(profile.clone());
            state.operation_epoch = 3;
            state.runtime_operations_ready = true;
            state.selected_pane = Some(source);
        }
        source_shared.set_state(ConnectionState::Ready);
        let source_abort = runtime()
            .expect("cancel test runtime")
            .spawn(std::future::pending::<()>())
            .abort_handle();
        connections()
            .lock()
            .expect("cancel source registry")
            .insert(
                source,
                ConnectionEntry {
                    shared: Arc::clone(&source_shared),
                    abort: source_abort,
                },
            );

        let first = runtime_browse_start_current(source).expect("first browse start");
        let first_owner = runtime_browse()
            .lock()
            .expect("first browse state")
            .as_ref()
            .expect("first browse entry")
            .provisional_owner;
        assert!(registry::shared_terminal(first_owner).is_ok());

        // Starting another target is the explicit open-another-server edge:
        // the old provisional actor/root is retired before the new one is
        // installed, so there is never more than one browse root.
        let second = runtime_browse_start_current(source).expect("second browse start");
        let second_owner = runtime_browse()
            .lock()
            .expect("second browse state")
            .as_ref()
            .expect("second browse entry")
            .provisional_owner;
        assert_ne!(first.token, second.token);
        assert_ne!(first_owner, second_owner);
        assert!(registry::shared_terminal(first_owner).is_err());
        assert!(registry::shared_terminal(second_owner).is_ok());

        // A confirmed no-op is still an unpromoted browse. Closing it retires
        // the provisional SSH/root while leaving the Ready source untouched.
        {
            let mut browse = runtime_browse().lock().expect("unchanged browse entry");
            let entry = browse.as_mut().expect("active browse entry");
            entry.phase = RuntimeBrowsePhase::Unchanged;
            entry.active_terminal_id = Some(source);
        }

        runtime_browse_cancel(&second.token).expect("cancel second browse");
        assert!(registry::shared_terminal(second_owner).is_err());
        assert!(
            connections()
                .lock()
                .expect("cancelled browse connection registry")
                .get(&second_owner)
                .is_none()
        );
        {
            let source_state = source_shared.session.lock().expect("source after cancel");
            assert_eq!(source_state.generation, generation);
            assert_eq!(source_state.operation_epoch, 3);
            assert!(source_state.runtime_operations_ready);
            assert_eq!(
                source_state.profile.as_ref().map(|p| p.host.as_str()),
                Some("127.0.0.1")
            );
        }
        assert!(!source_shared.is_cancelled());
        assert!(!source_shared.explicit_cleanup_requested());
        assert_eq!(
            source_shared
                .snapshot()
                .expect("source remains Ready")
                .state,
            ConnectionState::Ready as u32
        );

        let source_entry = connections()
            .lock()
            .expect("source cleanup registry")
            .remove(&source)
            .expect("source entry");
        source_entry.shared.cancel();
        source_entry.abort.abort();
        drop(source_entry);
        registry::destroy_terminal(source);
        drop(source_shared);
        drop(profile);
        drop(password);
        // Cancellation must not add a target-owned reference to the source's
        // parsed credential. The synthetic source is destroyed above, so the
        // zeroizing Arc can now disappear completely.
        assert!(weak_password.upgrade().is_none());
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
