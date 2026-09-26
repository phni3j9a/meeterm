//! Rust-owned attachment operations (Issue #28).
//!
//! One attachment operation uploads a single locally selected image file
//! over a second SFTP channel multiplexed on the *existing* authenticated
//! SSH connection, stores it under a private remote directory, and — only
//! on an explicit user request — inserts one quoted remote path line into
//! the destination pane through the epoch-guarded native paste path.
//!
//! The operation binds a stable destination fence (connection generation,
//! session operation epoch, remote pane identity, the native terminal
//! mapped to that pane, and for Herdr the stable remote `terminal_id`).
//! Display names, pane indices, and focus are never used as identity.
//!
//! Uploaded, inserted, and CLI/model-observed are deliberately different
//! milestones. This module never sends Enter, never builds a shell command,
//! never creates a CLI session, and never claims a Codex/Claude session
//! consumed the image. A detached transfer completing after cancellation
//! is discarded without changing the visible state.

use std::collections::HashMap;
use std::fmt;
use std::io::Read as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags, StatusCode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::registry;
use crate::workspace::Backend;

/// Public snapshot capacities shared with the fixed C ABI record.
pub const ATTACHMENT_PATH_CAPACITY: usize = 512;
pub const ATTACHMENT_NAME_CAPACITY: usize = 128;
pub const ATTACHMENT_CODE_CAPACITY: usize = 64;
pub const ATTACHMENT_MSG_CAPACITY: usize = 256;

/// One image stays well inside every supported CLI's practical input and a
/// phone-class uplink. The limit is an argument bound, not a remote quota.
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
pub const ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED: u32 = 0x1;
/// The remote file this operation created was explicitly deleted through
/// `attachment_delete_remote`. Composes with the phase: `inserted` is not
/// revoked, while `uploaded`+removed means the path is gone.
pub const ATTACHMENT_FLAG_REMOTE_REMOVED: u32 = 0x2;

/// Default remote base resolved against the SFTP start directory
/// (`realpath(".")`), never a client-side `~` assumption.
const REMOTE_DIR_COMPONENTS: [&str; 4] = [".local", "share", "meeterm", "attachments"];
/// Number of trailing default-dir components that are app-private
/// (`meeterm/attachments`): they are created *and* restricted to `0700`.
/// `.local` and `share` are only created when missing and never chmod'ed.
const APP_DIR_COMPONENTS: usize = 2;
const REMOTE_DIR_MODE: u32 = 0o700;
const REMOTE_FILE_MODE: u32 = 0o600;
const WRITE_CHUNK_BYTES: usize = 32 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const TRANSFER_TIMEOUT_SECS: u64 = 300;
const MAX_REMOTE_NAME_BYTES: usize = 128;
/// The product contract is one attachment operation at a time; there is no
/// multi-attachment queue. Terminal (`failed`/`cancelled`) records do not
/// count — the adapter may dispose them at leisure.
const MAX_LIVE_OPS_PER_OWNER: usize = 1;
const MAX_LOCAL_PATH_BYTES: usize = 4096;
const MAX_DISPLAY_NAME_BYTES: usize = 512;
const MAX_REMOTE_DIR_BYTES: usize = 256;
const MAX_EXT_BYTES: usize = 8;
/// Slack over the payload so the availability check also covers directory
/// metadata and the partial file that briefly coexists with a final copy.
const REMOTE_SPACE_SLACK: u64 = 64 * 1024;

/// User-visible operation milestone. Serialized verbatim in the fixed ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AttachmentPhase {
    /// Blocked before/without an upload attempt; `reason` explains and an
    /// explicit retry is required. Nothing proceeds automatically.
    Pending = 0,
    /// The SFTP transfer task owns the operation.
    Uploading = 1,
    /// The remote file was confirmed by lstat. Insertion is a separate
    /// explicit step; `reason` may still carry a pending insert-blocked code.
    Uploaded = 2,
    /// The native input path accepted one path line. This is not a delivery
    /// acknowledgment: tmux/Herdr/CLI never confirms that the remote program
    /// read the path or that any model saw the image.
    Inserted = 3,
    /// Terminal failure; retry requires a new operation or `retry_upload`.
    Failed = 4,
    /// User cancelled. A delayed transfer completion cannot resurrect it.
    Cancelled = 5,
}

/// Synchronous rejection of a public attachment call. Ongoing-state reasons
/// live on the operation snapshot instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentError {
    InvalidArgument,
    UnknownTerminal,
    UnknownAttachment,
    InvalidState,
    SourceUnreadable,
    SourceTooLarge,
    DestinationNotReady,
    Busy,
    Internal,
    /// The intent id passed to `attachment_begin` was never created by
    /// `attachment_intent` or was already disposed.
    UnknownIntent,
    /// The recorded destination identity no longer matches the current
    /// connection/runtime/pane — the operation is never retargeted.
    DestinationChanged,
    /// The recorded destination pane/terminal no longer exists.
    DestinationMissing,
}

impl AttachmentError {
    pub const fn code(self) -> i32 {
        match self {
            Self::InvalidArgument => -1,
            Self::UnknownTerminal => -2,
            Self::UnknownAttachment => -3,
            Self::InvalidState => -4,
            Self::SourceUnreadable => -5,
            Self::SourceTooLarge => -6,
            Self::DestinationNotReady => -7,
            Self::Busy => -8,
            Self::Internal => -9,
            Self::UnknownIntent => -10,
            Self::DestinationChanged => -11,
            Self::DestinationMissing => -12,
        }
    }

    pub const fn error_code(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::UnknownTerminal => "unknown_terminal",
            Self::UnknownAttachment => "unknown_attachment",
            Self::InvalidState => "invalid_state",
            Self::SourceUnreadable => "source_unreadable",
            Self::SourceTooLarge => "source_too_large",
            Self::DestinationNotReady => "destination_not_ready",
            Self::Busy => "busy",
            Self::Internal => "internal_error",
            Self::UnknownIntent => "unknown_intent",
            Self::DestinationChanged => "destination_changed",
            Self::DestinationMissing => "destination_missing",
        }
    }
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArgument => "attachment arguments are invalid",
            Self::UnknownTerminal => "terminal ID is not registered",
            Self::UnknownAttachment => "attachment ID is not registered",
            Self::InvalidState => "the attachment is not in a state for that action",
            Self::SourceUnreadable => "the selected image file cannot be read",
            Self::SourceTooLarge => "the selected image exceeds the attachment limit",
            Self::DestinationNotReady => "the fenced destination is not currently usable",
            Self::Busy => "the connection command queue cannot accept the request",
            Self::Internal => "native attachment state is unavailable",
            Self::UnknownIntent => "the attachment intent id is not registered",
            Self::DestinationChanged => "the recorded destination no longer matches",
            Self::DestinationMissing => "the recorded destination pane no longer exists",
        })
    }
}

impl std::error::Error for AttachmentError {}

/// A pending/blocked reason. These codes appear in the snapshot's
/// `error_code` field while the phase stays `Pending` (pre-upload) or
/// `Uploaded` (insert blocked); they are distinct from failure codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachmentBlock {
    NotReady,
    Busy,
    Timeout,
    StaleConnection,
    StaleOperation,
    DestinationChanged,
    DestinationMissing,
    InputNotReady,
    InputQueueFull,
    TransportClosed,
    StaleTerminal,
    /// The uploaded remote file failed its re-verification — it is gone or
    /// was replaced. The op drops to `Pending` so an explicit retry can
    /// upload again on the still-matching intent.
    RemoteMissing,
    Internal,
}

impl AttachmentBlock {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
            Self::StaleConnection => "stale_connection",
            Self::StaleOperation => "stale_operation",
            Self::DestinationChanged => "destination_changed",
            Self::DestinationMissing => "destination_missing",
            Self::InputNotReady => "input_not_ready",
            Self::InputQueueFull => "input_queue_full",
            Self::TransportClosed => "transport_closed",
            Self::StaleTerminal => "stale_terminal",
            Self::RemoteMissing => "remote_missing",
            Self::Internal => "internal_error",
        }
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::NotReady => "the connection is not ready for this operation",
            Self::Busy => "the connection command queue is full",
            Self::Timeout => "the remote did not answer in time",
            Self::StaleConnection => "the connection was replaced; retry on the current one",
            Self::StaleOperation => "the operation was issued for an older operation epoch",
            Self::DestinationChanged => "the selected terminal changed; reselect it and retry",
            Self::DestinationMissing => "the destination pane no longer exists",
            Self::InputNotReady => "terminal input is not ready right now",
            Self::InputQueueFull => "the native input queue is full",
            Self::TransportClosed => "the terminal input transport is closed",
            Self::StaleTerminal => "the terminal input binding was replaced",
            Self::RemoteMissing => "the remote file is gone; retry uploads it again",
            Self::Internal => "native attachment state is unavailable",
        }
    }
}

/// An `AttachmentBlock` raised while synchronously handling a public call
/// maps onto the public error enum. `destination_changed` and
/// `destination_missing` keep their precise codes so the adapter can tell
/// "reselect the pane" from "the pane is gone".
fn block_as_error(block: AttachmentBlock) -> AttachmentError {
    match block {
        AttachmentBlock::DestinationChanged => AttachmentError::DestinationChanged,
        AttachmentBlock::DestinationMissing => AttachmentError::DestinationMissing,
        AttachmentBlock::Busy => AttachmentError::Busy,
        _ => AttachmentError::DestinationNotReady,
    }
}

/// Credential-free identity of the SSH endpoint an attachment was uploaded
/// to. A remote path may be reused for the same endpoint only; another
/// endpoint (or another runtime on it) can never inherit a remote path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttachmentEndpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) backend: Backend,
    pub(crate) runtime: Option<String>,
}

/// The stable destination identity captured at operation creation. Every
/// field is re-validated under the session lock before a transfer retry or
/// a single-line insert; nothing here is a display label or array index.
#[derive(Clone, Debug)]
pub(crate) struct DestinationFence {
    /// `ConnectionShared::generation` at capture/recheck time.
    pub(crate) generation: u64,
    /// `SessionState::operation_epoch` at capture/recheck time.
    pub(crate) operation_epoch: u64,
    /// Remote pane identity (`%N` numeric handle / Herdr pane handle).
    pub(crate) pane_id: u64,
    /// Native terminal mapped to `pane_id` at fence time.
    pub(crate) native_terminal: u64,
    /// Herdr's stable remote `terminal_id`; `None` for the tmux backend,
    /// whose `%N` pane identity is already stable for the connection life.
    pub(crate) herdr_terminal_id: Option<String>,
    /// Endpoint the upload targeted; same-target reuse requires equality.
    pub(crate) endpoint: AttachmentEndpoint,
}

/// The durable destination intent captured when the attachment sheet was
/// opened: which pane the user picked, resolved to the SSH connection
/// owning it. Every operation and later call re-validates this stable
/// identity against the *current* connection state — connection
/// generation, operation epoch, and native terminal epoch are never part
/// of the identity; they are re-bound into a fresh `DestinationFence` per
/// call. Nothing ever retargets to the currently-selected pane.
#[derive(Clone)]
pub(crate) struct AttachmentIntent {
    /// Adapter-created intent id (registry key; not part of the identity).
    pub(crate) id: u64,
    /// Native terminal id the adapter passed for the picked pane.
    pub(crate) target_terminal: u64,
    /// Owning SSH connection's terminal id, resolved by core at creation.
    pub(crate) owner_terminal: u64,
    /// Credential-free SSH endpoint identity.
    pub(crate) endpoint: AttachmentEndpoint,
    /// Remote pane identity at capture (tmux `%N` handle / Herdr alias).
    pub(crate) pane_id: u64,
    /// Herdr's stable remote `terminal_id`; `None` for tmux whose pane id
    /// is already the stable identity. Herdr aliases change on workspace
    /// moves, so resolution keys on this value when present.
    pub(crate) herdr_terminal_id: Option<String>,
}

#[derive(Clone)]
struct TransferSpec {
    local_path: String,
    /// `None` resolves to the app-private default directory under the SFTP
    /// start dir; `Some` is the user's explicitly chosen absolute directory.
    remote_dir: Option<String>,
    /// Final remote file name (generated, unique per operation — the picked
    /// file's own name is never used remotely).
    remote_name: String,
    /// Private staging name inside the remote attachment directory.
    partial_name: String,
    size_bytes: u64,
}

pub(crate) struct AttachmentOperation {
    id: u64,
    /// The destination intent this op was created for. `owner_terminal`
    /// identifies the SSH connection; `target_terminal` is the pane
    /// terminal all caller-side calls must repeat for verification.
    intent: AttachmentIntent,
    fence: DestinationFence,
    /// Monotonically increasing per-op attempt identity. Every actor-side
    /// job (upload/remove/verify) carries the attempt it was launched for;
    /// outcomes and progress from an earlier attempt are discarded so a
    /// stale detached task can never mutate a retried operation.
    attempt: u64,
    spec: TransferSpec,
    display_name: String,
    phase: AttachmentPhase,
    /// Pending/blocked reason code (also reused while `Uploaded` to show a
    /// blocked insert retry path). Failure carries `error_code` instead.
    reason_code: Option<&'static str>,
    reason_message: Option<&'static str>,
    error_code: Option<&'static str>,
    error_message: Option<String>,
    remote_path: Option<String>,
    /// Canonical resolved remote directory captured when the upload
    /// established it. Deletion verifies the *current* resolution equals
    /// this base — a replaced/redirected parent is never followed.
    remote_base: Option<String>,
    bytes_uploaded: u64,
    cancel_requested: bool,
    insert_enqueued: bool,
    /// The uploaded remote file was explicitly deleted.
    removed: bool,
    /// Attempt id of the `SftpRemove` request owning this op right now;
    /// `Some` guards against duplicate remove enqueues, and the recorded
    /// attempt identifies which job's completion may land.
    remove_in_flight: Option<u64>,
    /// Attempt id of the `SftpVerify` re-verification request owning this
    /// op (`Uploaded` retry re-verifies the remote file instead of
    /// re-uploading). `Some` while the job is queued/running.
    verify_in_flight: Option<u64>,
}

impl AttachmentOperation {
    fn note_block(&mut self, block: AttachmentBlock) {
        self.reason_code = Some(block.code());
        self.reason_message = Some(block.message());
    }

    fn clear_status(&mut self) {
        self.reason_code = None;
        self.reason_message = None;
        self.error_code = None;
        self.error_message = None;
    }

    /// Mark `Pending` only while an upload is in flight or already
    /// pending; never resurrects a cancelled/failed/finished operation.
    fn pend(&mut self, block: AttachmentBlock) {
        if self.cancel_requested {
            self.phase = AttachmentPhase::Cancelled;
            return;
        }
        if matches!(
            self.phase,
            AttachmentPhase::Uploading | AttachmentPhase::Pending
        ) {
            self.phase = AttachmentPhase::Pending;
            self.note_block(block);
        }
    }

    fn fail(&mut self, code: &'static str, message: impl Into<String>) {
        if self.cancel_requested {
            self.phase = AttachmentPhase::Cancelled;
            return;
        }
        if matches!(
            self.phase,
            AttachmentPhase::Uploading | AttachmentPhase::Pending
        ) {
            self.phase = AttachmentPhase::Failed;
            self.error_code = Some(code);
            self.error_message = Some(message.into());
        }
    }

    fn cancel(&mut self) {
        self.cancel_requested = true;
        if matches!(
            self.phase,
            AttachmentPhase::Pending | AttachmentPhase::Uploading | AttachmentPhase::Uploaded
        ) {
            self.phase = AttachmentPhase::Cancelled;
        }
    }
}

static ATTACHMENTS: OnceLock<Mutex<HashMap<u64, Arc<Mutex<AttachmentOperation>>>>> =
    OnceLock::new();
static INTENTS: OnceLock<Mutex<HashMap<u64, AttachmentIntent>>> = OnceLock::new();
static NEXT_ATTACHMENT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_INTENT_ID: AtomicU64 = AtomicU64::new(1);

/// Bound on registered-but-never-used intents so a crashed adapter cannot
/// grow the map without bound. Ops keep their own copy of the intent's
/// stable identity, so disposing the intent never affects live ops.
const MAX_LIVE_INTENTS: usize = 32;

fn operations() -> &'static Mutex<HashMap<u64, Arc<Mutex<AttachmentOperation>>>> {
    ATTACHMENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn intents() -> &'static Mutex<HashMap<u64, AttachmentIntent>> {
    INTENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn operation(id: u64) -> Option<Arc<Mutex<AttachmentOperation>>> {
    operations()
        .lock()
        .ok()
        .and_then(|operations| operations.get(&id).cloned())
}

fn lock_operation(
    op: &Arc<Mutex<AttachmentOperation>>,
) -> Result<std::sync::MutexGuard<'_, AttachmentOperation>, AttachmentError> {
    op.lock().map_err(|_| AttachmentError::Internal)
}

/// Test-only: stand in for the detached transfer's `Uploaded` outcome so
/// fence/intent tests can exercise the post-upload paths without SFTP.
#[cfg(test)]
pub(crate) fn test_mark_uploaded(id: u64, remote_path: &str) {
    if let Some(op) = operation(id)
        && let Ok(mut op) = op.lock()
    {
        op.phase = AttachmentPhase::Uploaded;
        op.remote_path = Some(remote_path.to_owned());
    }
}

/// Capture the stable destination intent for the pane terminal the user
/// picked. Core resolves the owning SSH connection itself — the adapter
/// never needs the owner id, and non-owner panes (every Herdr pane, tmux
/// second-and-later panes) resolve to their owning connection the same
/// way. The recorded identity is the SSH endpoint, backend/runtime, and
/// the remote pane identity — *not* the current generation/epochs, which
/// are re-bound into a fresh fence at each later call.
pub fn attachment_intent(target_terminal_id: u64) -> Result<u64, AttachmentError> {
    registry::shared_terminal(target_terminal_id).map_err(|_| AttachmentError::UnknownTerminal)?;
    let (owner, shared) = crate::ssh::connection_for_terminal(target_terminal_id)
        .ok_or(AttachmentError::DestinationNotReady)?;
    let mut intent = shared
        .attachment_intent_identity(owner, target_terminal_id)
        .map_err(block_as_error)?;
    let id = NEXT_INTENT_ID.fetch_add(1, Ordering::AcqRel).max(1);
    intent.id = id;
    let mut intents = intents().lock().map_err(|_| AttachmentError::Internal)?;
    if intents.len() >= MAX_LIVE_INTENTS {
        return Err(AttachmentError::Busy);
    }
    intents.insert(id, intent);
    Ok(id)
}

/// Drop a recorded intent. Idempotent — the sheet may be closed before or
/// after operations were created from it; live ops keep their own copy of
/// the captured identity.
pub fn attachment_intent_dispose(intent_id: u64) -> Result<(), AttachmentError> {
    intents()
        .lock()
        .map_err(|_| AttachmentError::Internal)?
        .remove(&intent_id);
    Ok(())
}

/// Reject a `target_terminal_id` that is not the pane terminal the
/// operation's intent recorded. A live-but-different native terminal means
/// the caller addressed a different destination, so the operation is held
/// with `destination_changed` — never silently retargeted. An id that is
/// not a terminal at all is a plain argument error.
fn reject_foreign_target(op: &mut AttachmentOperation, target_terminal_id: u64) -> AttachmentError {
    if registry::shared_terminal(target_terminal_id).is_ok() {
        op.note_block(AttachmentBlock::DestinationChanged);
        AttachmentError::DestinationChanged
    } else {
        AttachmentError::InvalidArgument
    }
}

/// Look up an intent id for `attachment_begin`.
fn intent(intent_id: u64) -> Result<AttachmentIntent, AttachmentError> {
    intents()
        .lock()
        .map_err(|_| AttachmentError::Internal)?
        .get(&intent_id)
        .cloned()
        .ok_or(AttachmentError::UnknownIntent)
}

/// Mark one in-flight op pending after its actor-side request was dropped
/// (stale epoch, not-ready gate, a dead command channel, or a queued
/// launch the actor never reached). The attempt must still be current —
/// a stale-attempt request that never ran must not disturb the retried op.
/// A cancelled op keeps its cancelled outcome.
pub(crate) fn mark_pending(id: u64, attempt: u64, block: AttachmentBlock) {
    if let Some(op) = operation(id)
        && let Ok(mut op) = op.lock()
        && op.attempt == attempt
    {
        op.pend(block);
    }
}

/// The command loop dropped a `SftpRemove` request before executing it.
/// The op keeps its phase; the in-flight marker clears so an explicit
/// remove retry can enqueue again. Only the owning attempt is released.
pub(crate) fn mark_remove_expired(id: u64, attempt: u64, block: AttachmentBlock) {
    if let Some(op) = operation(id)
        && let Ok(mut op) = op.lock()
        && op.remove_in_flight == Some(attempt)
    {
        op.remove_in_flight = None;
        op.note_block(block);
    }
}

/// The command loop dropped a `SftpVerify` request before executing it.
/// The op keeps its `Uploaded` phase; the marker clears so another
/// explicit re-verification can enqueue.
pub(crate) fn mark_verify_expired(id: u64, attempt: u64, block: AttachmentBlock) {
    if let Some(op) = operation(id)
        && let Ok(mut op) = op.lock()
        && op.verify_in_flight == Some(attempt)
    {
        op.verify_in_flight = None;
        op.note_block(block);
    }
}

/// Actor gate for `ControlCommand::SftpUpload`: the request must be the
/// current attempt for this owner and this connection generation, and the
/// op must still be awaiting launch. A request from a superseded attempt
/// is dropped silently — the newer attempt owns the op's state.
pub(crate) fn gate_launch(
    op: &Arc<Mutex<AttachmentOperation>>,
    owner: u64,
    generation: u64,
    attempt: u64,
) -> bool {
    let Ok(mut op) = op.lock() else {
        return false;
    };
    if op.attempt != attempt {
        return false;
    }
    if op.cancel_requested {
        op.phase = AttachmentPhase::Cancelled;
        return false;
    }
    if op.phase != AttachmentPhase::Uploading {
        return false;
    }
    if op.intent.owner_terminal != owner || op.fence.generation != generation {
        op.phase = AttachmentPhase::Pending;
        op.note_block(AttachmentBlock::StaleOperation);
        return false;
    }
    true
}

/// Actor gate for `ControlCommand::SftpRemove`: the request must still be
/// the in-flight remove for this attempt on this owner/generation.
/// A stale request clears its in-flight marker instead of running.
pub(crate) fn gate_remove(
    op: &Arc<Mutex<AttachmentOperation>>,
    owner: u64,
    generation: u64,
    attempt: u64,
) -> bool {
    let Ok(mut op) = op.lock() else {
        return false;
    };
    if op.remove_in_flight != Some(attempt) {
        return false;
    }
    if op.intent.owner_terminal != owner || op.fence.generation != generation {
        op.remove_in_flight = None;
        op.note_block(AttachmentBlock::StaleOperation);
        return false;
    }
    true
}

/// Actor gate for `ControlCommand::SftpVerify`: the request must still be
/// the in-flight re-verification for this attempt on this owner and this
/// connection generation.
pub(crate) fn gate_verify(
    op: &Arc<Mutex<AttachmentOperation>>,
    owner: u64,
    generation: u64,
    attempt: u64,
) -> bool {
    let Ok(mut op) = op.lock() else {
        return false;
    };
    if op.verify_in_flight != Some(attempt) {
        return false;
    }
    if op.phase != AttachmentPhase::Uploaded
        || op.intent.owner_terminal != owner
        || op.fence.generation != generation
    {
        op.verify_in_flight = None;
        op.note_block(AttachmentBlock::StaleOperation);
        return false;
    }
    true
}

/// The actor aborted while preparing this op's SFTP channel. Applies only
/// to the attempt it was launched for; the op may still be retried on a
/// later connection attempt.
pub(crate) fn launch_pending(
    op: &Arc<Mutex<AttachmentOperation>>,
    attempt: u64,
    block: AttachmentBlock,
) {
    if let Ok(mut op) = op.lock()
        && op.attempt == attempt
    {
        op.pend(block);
    }
}

/// The actor-side channel/subsystem setup failed hard (SFTP unavailable).
pub(crate) fn launch_failed(
    op: &Arc<Mutex<AttachmentOperation>>,
    attempt: u64,
    code: &'static str,
    message: impl Into<String>,
) {
    if let Ok(mut op) = op.lock()
        && op.attempt == attempt
    {
        op.fail(code, message);
    }
}

/// A remote-delete job stalled before its SFTP session existed. The file
/// state is unchanged; the in-flight marker clears so an explicit retry
/// can enqueue, and the reason stays visible without touching the phase.
pub(crate) fn remove_stalled(
    op: &Arc<Mutex<AttachmentOperation>>,
    attempt: u64,
    block: AttachmentBlock,
) {
    if let Ok(mut op) = op.lock()
        && op.remove_in_flight == Some(attempt)
    {
        op.remove_in_flight = None;
        op.note_block(block);
    }
}

/// The SFTP subsystem was rejected outright for a remote-delete job.
pub(crate) fn remove_failed(
    op: &Arc<Mutex<AttachmentOperation>>,
    attempt: u64,
    code: &'static str,
    message: impl Into<String>,
) {
    if let Ok(mut op) = op.lock()
        && op.remove_in_flight == Some(attempt)
    {
        op.remove_in_flight = None;
        op.error_code = Some(code);
        op.error_message = Some(message.into());
    }
}

/// A re-verification job stalled before its SFTP session existed. The op
/// keeps its uploaded state; the marker clears for a later explicit retry.
pub(crate) fn verify_stalled(
    op: &Arc<Mutex<AttachmentOperation>>,
    attempt: u64,
    block: AttachmentBlock,
) {
    if let Ok(mut op) = op.lock()
        && op.verify_in_flight == Some(attempt)
    {
        op.verify_in_flight = None;
        op.note_block(block);
    }
}

/// The SFTP subsystem was rejected outright for a re-verification job.
pub(crate) fn verify_failed(
    op: &Arc<Mutex<AttachmentOperation>>,
    attempt: u64,
    code: &'static str,
    message: impl Into<String>,
) {
    if let Ok(mut op) = op.lock()
        && op.verify_in_flight == Some(attempt)
    {
        op.verify_in_flight = None;
        op.error_code = Some(code);
        op.error_message = Some(message.into());
    }
}

/// The connection actor for `generation` ended (flow failure or shutdown)
/// and silently discarded any queued upload request. Mark surviving
/// still-`Uploading` ops for that generation pending so they are retryable
/// on the next actor instead of displaying a dead progress state. The
/// attempt counter also bumps so a detached task completing after the
/// actor died can never mutate the op's visible state.
pub(crate) fn generation_finished(owner: u64, generation: u64) {
    let Ok(operations) = operations().lock() else {
        return;
    };
    for op in operations.values() {
        let Ok(mut op) = op.lock() else {
            continue;
        };
        if op.intent.owner_terminal == owner && op.fence.generation == generation {
            // A detached task from this attempt is now orphaned; make sure
            // its eventual outcome is discarded even if the op is retried.
            op.attempt += 1;
            if op.remove_in_flight.is_some() {
                op.remove_in_flight = None;
                op.note_block(AttachmentBlock::StaleConnection);
            } else if op.verify_in_flight.is_some() {
                op.verify_in_flight = None;
                op.note_block(AttachmentBlock::StaleConnection);
            } else {
                op.pend(AttachmentBlock::StaleConnection);
            }
        }
    }
}

fn copy_bounded(out: &mut [u8], out_len: &mut u16, value: &str) {
    let mut end = value.len().min(out.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    out[..end].copy_from_slice(&value.as_bytes()[..end]);
    *out_len = u16::try_from(end).unwrap_or(u16::MAX);
}

/// Fixed-size, fully sanitized snapshot shared verbatim with the C ABI.
/// `remote_path`/`display_name` are already ASCII-safe or UTF-8 bounded;
/// `error_code`/`error_message` carry a pending reason while the phase is
/// `Pending` or an insert-blocked `Uploaded` operation.
#[repr(C)]
pub struct AttachmentSnapshot {
    pub phase: u32,
    pub flags: u32,
    pub attachment_id: u64,
    pub bytes_uploaded: u64,
    pub size_bytes: u64,
    pub remote_path_len: u16,
    pub remote_path: [u8; ATTACHMENT_PATH_CAPACITY],
    pub display_name_len: u16,
    pub display_name: [u8; ATTACHMENT_NAME_CAPACITY],
    pub error_code_len: u16,
    pub error_code: [u8; ATTACHMENT_CODE_CAPACITY],
    pub error_message_len: u16,
    pub error_message: [u8; ATTACHMENT_MSG_CAPACITY],
}

impl AttachmentSnapshot {
    fn empty() -> Self {
        Self {
            phase: 0,
            flags: 0,
            attachment_id: 0,
            bytes_uploaded: 0,
            size_bytes: 0,
            remote_path_len: 0,
            remote_path: [0; ATTACHMENT_PATH_CAPACITY],
            display_name_len: 0,
            display_name: [0; ATTACHMENT_NAME_CAPACITY],
            error_code_len: 0,
            error_code: [0; ATTACHMENT_CODE_CAPACITY],
            error_message_len: 0,
            error_message: [0; ATTACHMENT_MSG_CAPACITY],
        }
    }
}

/// UTC `(YYYYMMDD, HHMMSS)` for the generated remote name. Derived from
/// the system clock; collision-resistance comes from the random tail, not
/// the stamp.
fn stamp_from_millis(millis: u64) -> (u32, u32) {
    let seconds = millis / 1000;
    let days = seconds / 86_400;
    let second_of_day = seconds % 86_400;
    // Howard Hinnant's civil_from_days — days since epoch -> (y, m, d).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    let yyyymmdd = (year as u32) * 10_000 + (month as u32) * 100 + day as u32;
    let hhmmss = ((second_of_day / 3600) as u32) * 10_000
        + ((second_of_day / 60) % 60) as u32 * 100
        + (second_of_day % 60) as u32;
    (yyyymmdd, hhmmss)
}

/// Image type hint from the file's magic bytes, not its picked name. The
/// generated remote name only carries an extension the data actually
/// proves; recognized magic covers the common phone photo formats and a
/// sanitized picked extension is a last resort for other types.
fn image_extension(local_path: &str, picked_path: &str) -> String {
    let magic = std::fs::File::open(local_path)
        .and_then(|mut file| {
            let mut head = [0u8; 16];
            let mut read = 0usize;
            while read < head.len() {
                match file.read(&mut head[read..]) {
                    Ok(0) => break,
                    Ok(n) => read += n,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(head)
        })
        .unwrap_or([0u8; 16]);
    let detected = if magic.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("png")
    } else if magic.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if magic.starts_with(b"GIF87a") || magic.starts_with(b"GIF89a") {
        Some("gif")
    } else if magic.starts_with(b"RIFF") && magic[8..12] == *b"WEBP" {
        Some("webp")
    } else if magic[4..8] == *b"ftyp" {
        // ISOBMFF brands (HEIC/HEIF/AVIF) used by phone cameras.
        match &magic[8..12] {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"hevm" | b"hevs" => {
                Some("heic")
            }
            b"mif1" | b"msf1" => Some("heif"),
            b"avif" | b"avis" => Some("avif"),
            _ => None,
        }
    } else {
        None
    };
    detected
        .map(|ext| format!(".{ext}"))
        .or_else(|| {
            Path::new(picked_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .filter(|extension| {
                    !extension.is_empty()
                        && extension.len() <= MAX_EXT_BYTES
                        && extension
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                })
                .map(|extension| format!(".{extension}"))
        })
        .unwrap_or_default()
}

/// Generated remote file name: `meeterm-<YYYYMMDD>-<HHMMSS>-<16 hex>.<ext>`.
/// The picked file's own name never reaches the remote; the random tail
/// keeps names unpredictable and collision-resistant across restarts that
/// reset attachment ids. All characters stay inside `[a-z0-9.-]`.
fn remote_file_name(id: u64, extension: &str) -> String {
    let _ = id;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let (yyyymmdd, hhmmss) = stamp_from_millis(millis);
    let random = rand::random::<u64>();
    let mut name = format!("meeterm-{yyyymmdd:08}-{hhmmss:06}-{random:016x}{extension}");
    name.truncate(MAX_REMOTE_NAME_BYTES);
    name
}

/// The exact generated-name grammar `run_remove` requires before deleting:
/// `meeterm-` + 8 digits + `-` + 6 digits + `-` + 16 lowercase hex +
/// optional `.<a-z0-9>` extension. Anything else — including a name that
/// was never generated by this client — must not be deleted by us.
fn remote_basename_valid(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("meeterm-") else {
        return false;
    };
    let mut parts = rest.splitn(3, '-');
    let date = parts.next().unwrap_or_default();
    let time = parts.next().unwrap_or_default();
    let tail = parts.next().unwrap_or_default();
    let (random, extension) = match tail.split_once('.') {
        Some((random, extension)) => (random, Some(extension)),
        None => (tail, None),
    };
    date.len() == 8
        && date.bytes().all(|byte| byte.is_ascii_digit())
        && time.len() == 6
        && time.bytes().all(|byte| byte.is_ascii_digit())
        && random.len() == 16
        && random
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && extension.is_none_or(|extension| {
            !extension.is_empty()
                && extension.len() <= MAX_EXT_BYTES
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// A staged partial name is `.meeterm-partial-` + the same generated base
/// name; its validity is decided by the base-name check.
fn partial_name(remote_name: &str) -> String {
    format!(".meeterm-partial-{remote_name}")
}

/// The single line inserted into the fenced pane. Single-quoting keeps a
/// `$HOME` containing spaces safe. The path must never carry `'`, CR, LF,
/// or control characters — it is rejected *before* the line exists, never
/// escaped, because only generated names and pre-validated directories are
/// legitimate. No newline and no Enter is ever added; `paste_utf8` keeps
/// the line editable for the user.
fn insertion_line(remote_path: &str) -> Result<Vec<u8>, AttachmentError> {
    if remote_path.is_empty()
        || remote_path.contains('\'')
        || remote_path.chars().any(char::is_control)
    {
        return Err(AttachmentError::InvalidArgument);
    }
    let mut line = String::with_capacity(remote_path.len() + 2);
    line.push('\'');
    line.push_str(remote_path);
    line.push('\'');
    Ok(line.into_bytes())
}

/// Begin-level acceptance for an explicit `remote_dir`: clean absolute or
/// `~/`-prefixed, byte-bounded, no NUL. Character/component safety is
/// validated at transfer time where `remote_unsafe_path` can be reported
/// on the snapshot; existence, symlink, and writability checks all run
/// remotely.
fn validate_remote_dir(remote_dir: &str) -> Result<String, AttachmentError> {
    let trimmed = remote_dir.trim_end_matches('/');
    if trimmed.is_empty() || trimmed.len() > MAX_REMOTE_DIR_BYTES || trimmed.contains('\0') {
        return Err(AttachmentError::InvalidArgument);
    }
    if !trimmed.starts_with('/') && !trimmed.starts_with("~/") {
        return Err(AttachmentError::InvalidArgument);
    }
    if trimmed.len() <= 2 && trimmed.starts_with("~/") {
        return Err(AttachmentError::InvalidArgument);
    }
    Ok(trimmed.to_owned())
}

/// Split a remote path into absolute components. The leading empty
/// component represents the root marker and is skipped by the walker.
fn path_components(path: &str) -> Vec<String> {
    path.split('/').map(str::to_owned).collect()
}

/// Resolve the operation's remote directory into absolute components plus
/// policy flags. `home` is the verified `realpath(".")` result; `~` is
/// expanded *against it*, never client-side. Returns the components, how
/// many leading ones are already verified (home), whether missing
/// components may be created (default dir only), how many trailing ones
/// are app-private and must be forced to `0700`, and whether the leaf is
/// an explicit user directory that needs a writability probe.
fn remote_dir_components(
    spec: &TransferSpec,
    home: &str,
) -> Result<(Vec<String>, usize, bool, usize, bool), TransferOutcome> {
    let Some(dir) = &spec.remote_dir else {
        let mut components = path_components(home);
        components.extend(REMOTE_DIR_COMPONENTS.iter().map(|part| (*part).to_owned()));
        return Ok((
            components,
            path_components(home).len(),
            true,
            APP_DIR_COMPONENTS,
            false,
        ));
    };
    let unsafe_path = || {
        TransferOutcome::Failed(
            "remote_unsafe_path",
            "explicit remote directory is not a clean absolute path".to_owned(),
        )
    };
    // `'` / CR / LF / control characters can never appear: the resulting
    // path is single-quoted verbatim into the inserted line.
    if dir.chars().any(|ch| ch == '\'' || ch.is_control()) {
        return Err(unsafe_path());
    }
    let mut components: Vec<String>;
    let verified;
    if let Some(rest) = dir.strip_prefix("~/") {
        components = path_components(home.trim_end_matches('/'));
        verified = components.len();
        // `~//x` keeps a leading empty rest component — rejected by the
        // empty-component check below, same as `/a//b`.
        components.extend(path_components(rest));
    } else if dir.starts_with('/') {
        components = path_components(dir);
        verified = 0;
    } else {
        return Err(unsafe_path());
    }
    for (index, component) in components.iter().enumerate() {
        if index < verified {
            continue;
        }
        if index == 0 && component.is_empty() {
            continue; // leading '/' marker
        }
        if component.is_empty() || component == "." || component == ".." || component.len() > 255 {
            return Err(unsafe_path());
        }
    }
    Ok((components, verified, false, 0, true))
}

/// Exclusive-create probe proving the explicit leaf directory is actually
/// writable — directory `rwx` bits alone do not prove the SFTP user can
/// create files (ACLs, read-only mounts, quota). Denial maps to
/// `remote_permission_denied`; the probe name is ours and always removed.
async fn probe_dir_writable(session: &RawSftpSession, base: &str) -> Result<(), TransferOutcome> {
    let probe = format!("{base}/.meeterm-probe-{:016x}", rand::random::<u64>());
    match session
        .open(
            &probe,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            mode_only(REMOTE_FILE_MODE),
        )
        .await
    {
        Ok(handle) => {
            let _ = session.close(handle.handle).await;
            let _ = session.remove(&probe).await;
            Ok(())
        }
        Err(SftpError::Status(status)) if status.status_code == StatusCode::PermissionDenied => {
            Err(TransferOutcome::Failed(
                "remote_permission_denied",
                "explicit remote directory is not writable".to_owned(),
            ))
        }
        Err(error) => Err(map_transfer_error(error, "sftp_error")),
    }
}

fn validate_begin_args(
    local_path: &str,
    display_name: &str,
    size_bytes: u64,
) -> Result<std::fs::Metadata, AttachmentError> {
    if local_path.is_empty()
        || local_path.len() > MAX_LOCAL_PATH_BYTES
        || local_path.contains('\0')
        || display_name.len() > MAX_DISPLAY_NAME_BYTES
    {
        return Err(AttachmentError::InvalidArgument);
    }
    if size_bytes == 0 {
        return Err(AttachmentError::InvalidArgument);
    }
    if size_bytes > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::SourceTooLarge);
    }
    let metadata = std::fs::metadata(local_path).map_err(|_| AttachmentError::SourceUnreadable)?;
    // The adapter may hand over a symlink to the picked asset; reading its
    // target is fine. It must be a regular file of the announced size.
    if !metadata.is_file() || metadata.len() != size_bytes {
        return Err(AttachmentError::SourceUnreadable);
    }
    Ok(metadata)
}

fn live_op_count(operations: &HashMap<u64, Arc<Mutex<AttachmentOperation>>>, owner: u64) -> usize {
    operations
        .values()
        .filter(|op| {
            op.lock().map_or(0, |op| {
                usize::from(
                    op.intent.owner_terminal == owner
                        && !matches!(
                            op.phase,
                            AttachmentPhase::Failed | AttachmentPhase::Cancelled
                        ),
                )
            }) == 1
        })
        .count()
}

/// Begin one attachment from a destination intent created by
/// `attachment_intent`: validate the picked file, re-validate the intent's
/// stable identity against the *current* connection state, capture a fresh
/// execution fence, register the operation, and enqueue its SFTP upload on
/// the owning connection actor. If the recorded destination no longer
/// matches — a different Server/Session/runtime, a replaced or missing
/// pane — the call fails with `DestinationChanged`/`DestinationMissing`
/// and never retargets to whatever pane is currently selected.
///
/// `remote_dir` is the user's explicitly chosen remote directory (clean
/// absolute path, or `~/`-prefixed for the SFTP start dir), or `None` for
/// the app-private default `<sftp-start>/.local/share/meeterm/attachments`.
/// The returned id identifies the operation for polling, insert, retry,
/// cancel, delete, and dispose.
pub fn attachment_begin(
    intent_id: u64,
    local_path: &str,
    display_name: &str,
    remote_dir: Option<&str>,
    size_bytes: u64,
) -> Result<u64, AttachmentError> {
    validate_begin_args(local_path, display_name, size_bytes)?;
    let remote_dir = remote_dir.map(validate_remote_dir).transpose()?;
    let intent = intent(intent_id)?;
    let owner = intent.owner_terminal;
    let shared =
        crate::ssh::current_connection(owner).map_err(|_| AttachmentError::DestinationNotReady)?;
    let fence = shared
        .attachment_resolve_intent(&intent)
        .map_err(block_as_error)?;

    let id = NEXT_ATTACHMENT_ID.fetch_add(1, Ordering::AcqRel).max(1);
    // The remote extension reflects the file's magic bytes; the picked
    // name is only a fallback hint and never leaks into the remote path.
    let extension = image_extension(local_path, local_path);
    let remote_name = remote_file_name(id, &extension);
    let partial_name = partial_name(&remote_name);
    let display_name = if display_name.is_empty() {
        remote_name.clone()
    } else {
        display_name.to_owned()
    };

    let op = Arc::new(Mutex::new(AttachmentOperation {
        id,
        intent,
        fence,
        attempt: 1,
        spec: TransferSpec {
            local_path: local_path.to_owned(),
            remote_dir,
            remote_name,
            partial_name,
            size_bytes,
        },
        display_name,
        phase: AttachmentPhase::Uploading,
        reason_code: None,
        reason_message: None,
        error_code: None,
        error_message: None,
        remote_path: None,
        remote_base: None,
        bytes_uploaded: 0,
        cancel_requested: false,
        insert_enqueued: false,
        removed: false,
        remove_in_flight: None,
        verify_in_flight: None,
    }));
    {
        let mut operations = operations().lock().map_err(|_| AttachmentError::Internal)?;
        if live_op_count(&operations, owner) >= MAX_LIVE_OPS_PER_OWNER {
            return Err(AttachmentError::Busy);
        }
        operations.insert(id, Arc::clone(&op));
    }
    if let Err(block) = shared.attachment_enqueue_command(crate::ssh::ControlCommand::SftpUpload {
        attachment_id: id,
        attempt: 1,
    }) {
        mark_pending(id, 1, block);
    }
    Ok(id)
}

/// Explicit transfer retry / re-validation against the recorded intent.
/// `target_terminal_id` must be the pane terminal the intent captured —
/// the call never retargets. The intent's stable identity is resolved
/// against the *current* connection state and a fresh execution fence is
/// captured, so a connection recovery (new generation/epoch, same stable
/// destination) becomes usable again without a new intent.
///
/// An `Uploaded` op whose remote file exists is *not* silently accepted:
/// it is re-verified by a `SftpVerify` job (`lstat` + size + mode) before
/// it can be trusted again, while keeping `Uploaded` so the user can
/// insert. If the file is gone or replaced the op drops to `Pending` with
/// `remote_missing` and a later retry uploads it again. A `Pending`,
/// `Failed`, or explicitly-removed `Uploaded` op re-runs the full upload
/// path; the transfer task's same-endpoint reuse check still skips
/// re-sending a verified remote file.
pub fn attachment_retry_upload(
    target_terminal_id: u64,
    attachment_id: u64,
) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    enum Next {
        Upload,
        Verify,
    }
    let (owner, intent) = {
        let mut op = lock_operation(&op)?;
        if op.intent.target_terminal != target_terminal_id {
            return Err(reject_foreign_target(&mut op, target_terminal_id));
        }
        if op.cancel_requested {
            return Err(AttachmentError::InvalidState);
        }
        if op.remove_in_flight.is_some() {
            return Err(AttachmentError::Busy);
        }
        match op.phase {
            AttachmentPhase::Uploaded
                if op.remote_path.is_some() && !op.removed && op.verify_in_flight.is_none() =>
            {
                Next::Verify
            }
            AttachmentPhase::Uploaded if op.remote_path.is_some() && !op.removed => {
                // A verify for this attempt is already queued/running;
                // treat the repeat call as accepted work already done.
                return Ok(());
            }
            AttachmentPhase::Uploaded | AttachmentPhase::Pending | AttachmentPhase::Failed => {
                Next::Upload
            }
            AttachmentPhase::Uploading | AttachmentPhase::Inserted | AttachmentPhase::Cancelled => {
                return Err(AttachmentError::InvalidState);
            }
        };
        (op.intent.owner_terminal, op.intent.clone())
    };
    let shared = crate::ssh::current_connection(owner).map_err(|_| {
        if let Ok(mut op) = op.lock() {
            op.note_block(AttachmentBlock::StaleConnection);
        }
        AttachmentError::DestinationNotReady
    })?;
    let fence = match shared.attachment_resolve_intent(&intent) {
        Ok(fence) => fence,
        Err(block) => {
            if let Ok(mut op) = op.lock() {
                // A Failed op that cannot re-resolve keeps Failed but shows
                // the current reason; a Pending/Uploaded op keeps its phase.
                op.note_block(block);
            }
            return Err(block_as_error(block));
        }
    };
    let (attachment_attempt, command) = {
        let mut op = lock_operation(&op)?;
        if op.cancel_requested {
            return Err(AttachmentError::InvalidState);
        }
        if op.remove_in_flight.is_some() {
            return Err(AttachmentError::Busy);
        }
        let next = match op.phase {
            AttachmentPhase::Uploaded
                if op.remote_path.is_some() && !op.removed && op.verify_in_flight.is_none() =>
            {
                Next::Verify
            }
            AttachmentPhase::Uploaded if op.remote_path.is_some() && !op.removed => return Ok(()),
            AttachmentPhase::Uploaded | AttachmentPhase::Pending | AttachmentPhase::Failed => {
                Next::Upload
            }
            _ => return Err(AttachmentError::InvalidState),
        };
        op.attempt += 1;
        let attempt = op.attempt;
        op.fence = fence;
        op.clear_status();
        match next {
            Next::Verify => {
                op.verify_in_flight = Some(attempt);
                (
                    attempt,
                    crate::ssh::ControlCommand::SftpVerify {
                        attachment_id,
                        attempt,
                    },
                )
            }
            Next::Upload => {
                op.phase = AttachmentPhase::Uploading;
                op.bytes_uploaded = 0;
                op.remote_path = None;
                op.removed = false;
                op.cancel_requested = false;
                (
                    attempt,
                    crate::ssh::ControlCommand::SftpUpload {
                        attachment_id,
                        attempt,
                    },
                )
            }
        }
    };
    if let Err(block) = shared.attachment_enqueue_command(command) {
        mark_pending(attachment_id, attachment_attempt, block);
        if let Ok(mut op) = op.lock()
            && op.attempt == attachment_attempt
        {
            op.verify_in_flight = None;
        }
    }
    Ok(())
}

/// Explicit insert of an uploaded file: exactly one quoted remote path
/// line into the still-current fenced pane through `paste_utf8_at_epoch`.
/// Enter is never sent. A destination switch/replacement/disappearance,
/// transport/controller generation mismatch, or Herdr read-only state
/// records a pending reason and leaves the operation `Uploaded`.
pub fn attachment_insert(
    target_terminal_id: u64,
    attachment_id: u64,
) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let (owner, fence, remote_path) = {
        let mut op = lock_operation(&op)?;
        if op.intent.target_terminal != target_terminal_id {
            return Err(reject_foreign_target(&mut op, target_terminal_id));
        }
        match op.phase {
            // A duplicate tap after a successful insert is a no-op.
            AttachmentPhase::Inserted => return Ok(()),
            AttachmentPhase::Uploaded => {}
            _ => return Err(AttachmentError::InvalidState),
        }
        let Some(remote_path) = op.remote_path.clone() else {
            return Err(AttachmentError::InvalidState);
        };
        (op.intent.owner_terminal, op.fence.clone(), remote_path)
    };
    let shared = crate::ssh::current_connection(owner).map_err(|_| {
        if let Ok(mut op) = op.lock()
            && op.phase == AttachmentPhase::Uploaded
        {
            op.note_block(AttachmentBlock::StaleConnection);
        }
        AttachmentError::DestinationNotReady
    })?;
    // The remote path is generated by us but resolved by the server;
    // `'` / CR / LF / control characters are refused before the line
    // exists so the paste can never smuggle an Enter.
    let line = insertion_line(&remote_path).map_err(|_| AttachmentError::InvalidState)?;
    match shared.attachment_insert_line(&fence, &line) {
        Ok(_) => {
            if let Ok(mut op) = op.lock()
                && op.phase == AttachmentPhase::Uploaded
            {
                op.phase = AttachmentPhase::Inserted;
                op.insert_enqueued = true;
                op.clear_status();
            }
            Ok(())
        }
        Err(block) => {
            if let Ok(mut op) = op.lock()
                && op.phase == AttachmentPhase::Uploaded
            {
                op.note_block(block);
            }
            Err(AttachmentError::DestinationNotReady)
        }
    }
}

/// Cancel the operation. An in-flight transfer observes the flag, removes
/// its remote partial best-effort, and any delayed completion is discarded
/// without changing the visible state. `Inserted` can no longer be revoked:
/// the line already reached the native input queue.
pub fn attachment_cancel(attachment_id: u64) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let mut op = lock_operation(&op)?;
    match op.phase {
        AttachmentPhase::Inserted => Err(AttachmentError::InvalidState),
        AttachmentPhase::Failed | AttachmentPhase::Cancelled => Ok(()),
        _ => {
            op.cancel();
            Ok(())
        }
    }
}

/// Drop the operation record. An in-flight transfer is cancelled first and
/// keeps its own reference until the remote partial cleanup finished.
pub fn attachment_dispose(attachment_id: u64) -> Result<(), AttachmentError> {
    let Some(op) = operation(attachment_id) else {
        return Err(AttachmentError::UnknownAttachment);
    };
    if let Ok(mut op) = op.lock() {
        match op.phase {
            AttachmentPhase::Inserted => {}
            _ => op.cancel(),
        }
    }
    operations()
        .lock()
        .map_err(|_| AttachmentError::Internal)?
        .remove(&attachment_id);
    Ok(())
}

/// Explicit remote deletion of the files this operation created — the
/// published file and any `.meeterm-partial-*` staging remnant, on the
/// *same* SSH endpoint only. Never deletes anything outside the generated
/// names. Callable from `Uploaded`, `Inserted`, `Failed`, or `Cancelled`
/// (a `Pending`/`Uploading` op is still in flight and must be cancelled
/// first). Duplicate calls are idempotent; success sets
/// `ATTACHMENT_FLAG_REMOTE_REMOVED` on the snapshot while the phase is
/// kept (`inserted` cannot be revoked). There is no automatic deletion:
/// nothing is removed on insert, cancel, dispose, or app exit.
pub fn attachment_delete_remote(
    target_terminal_id: u64,
    attachment_id: u64,
) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let (owner, endpoint) = {
        let mut op = lock_operation(&op)?;
        if op.intent.target_terminal != target_terminal_id {
            return Err(reject_foreign_target(&mut op, target_terminal_id));
        }
        if op.removed {
            return Ok(());
        }
        if op.remove_in_flight.is_some() {
            return Err(AttachmentError::Busy);
        }
        match op.phase {
            AttachmentPhase::Pending | AttachmentPhase::Uploading => {
                return Err(AttachmentError::InvalidState);
            }
            AttachmentPhase::Uploaded
            | AttachmentPhase::Inserted
            | AttachmentPhase::Failed
            | AttachmentPhase::Cancelled => {}
        }
        (op.intent.owner_terminal, op.fence.endpoint.clone())
    };
    let shared = crate::ssh::current_connection(owner).map_err(|_| {
        if let Ok(mut op) = op.lock() {
            op.note_block(AttachmentBlock::StaleConnection);
        }
        AttachmentError::DestinationNotReady
    })?;
    // Deletion verifies the endpoint half of the intent only: the pane may
    // legitimately be gone while the remote file still exists and must be
    // cleanable. An endpoint/generation switch still refuses — the delete
    // must run on the same authenticated host that received the upload.
    if let Err(block) = shared.attachment_recheck_endpoint(&endpoint) {
        if let Ok(mut op) = op.lock() {
            op.note_block(block);
        }
        return Err(block_as_error(block));
    }
    let attempt = {
        let mut op = lock_operation(&op)?;
        if op.removed {
            return Ok(());
        }
        if op.remove_in_flight.is_some() {
            return Err(AttachmentError::Busy);
        }
        op.attempt += 1;
        op.remove_in_flight = Some(op.attempt);
        op.attempt
    };
    if let Err(block) = shared.attachment_enqueue_command(crate::ssh::ControlCommand::SftpRemove {
        attachment_id,
        attempt,
    }) {
        if let Ok(mut op) = op.lock()
            && op.remove_in_flight == Some(attempt)
        {
            op.remove_in_flight = None;
            op.note_block(block);
        }
        return Err(match block {
            AttachmentBlock::Busy => AttachmentError::Busy,
            _ => AttachmentError::DestinationNotReady,
        });
    }
    Ok(())
}

/// Poll one complete, sanitized, fixed-size snapshot.
pub fn attachment_snapshot(attachment_id: u64) -> Result<AttachmentSnapshot, AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let op = lock_operation(&op)?;
    let mut snapshot = AttachmentSnapshot::empty();
    snapshot.phase = op.phase as u32;
    snapshot.flags = (if op.insert_enqueued {
        ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED
    } else {
        0
    }) | (if op.removed {
        ATTACHMENT_FLAG_REMOTE_REMOVED
    } else {
        0
    });
    snapshot.attachment_id = op.id;
    snapshot.bytes_uploaded = op.bytes_uploaded;
    snapshot.size_bytes = op.spec.size_bytes;
    copy_bounded(
        &mut snapshot.remote_path,
        &mut snapshot.remote_path_len,
        op.remote_path.as_deref().unwrap_or_default(),
    );
    copy_bounded(
        &mut snapshot.display_name,
        &mut snapshot.display_name_len,
        &op.display_name,
    );
    let code = op.error_code.or(op.reason_code).unwrap_or_default();
    let message = op
        .error_message
        .as_deref()
        .or(op.reason_message)
        .unwrap_or_default();
    copy_bounded(&mut snapshot.error_code, &mut snapshot.error_code_len, code);
    copy_bounded(
        &mut snapshot.error_message,
        &mut snapshot.error_message_len,
        message,
    );
    Ok(snapshot)
}

pub const ATTACHMENT_SNAPSHOT_SIZE: usize = size_of::<AttachmentSnapshot>();

// ---------------------------------------------------------------------------
// SFTP transfer task
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum TransferOutcome {
    /// Confirmed remote final path.
    Uploaded(String),
    /// The generated remote names were deleted (or verified absent).
    Removed,
    /// The re-verification found the recorded remote file intact —
    /// same type, size, and private mode at the recorded path.
    Verified,
    /// The recorded remote file is gone or no longer matches its
    /// generated identity — insert must not use the stale path.
    RemoteMissing,
    /// Transient/structural blockage; the op stays retryable.
    Pending(AttachmentBlock),
    /// Terminal failure.
    Failed(&'static str, String),
    /// Cancel flag observed; partial was cleaned best-effort.
    Cancelled,
}

/// Which detached job produced an outcome — decided at spawn so a late
/// result can never be mistaken for a different job's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    Upload,
    Remove,
    Verify,
}

/// Spawn the detached transfer over an already-initialized SFTP stream.
/// Called by the connection actor after the subsystem handshake; this task
/// never blocks the interactive command loop. `attempt` identifies which
/// op attempt owns the result — a stale attempt's outcome is discarded.
pub(crate) fn start_transfer<S>(op: Arc<Mutex<AttachmentOperation>>, stream: S, attempt: u64)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let session = RawSftpSession::new(stream);
        session.set_timeout(REQUEST_TIMEOUT_SECS);
        let outcome = async {
            match session.init().await {
                Ok(version) => {
                    let statvfs = version.extensions.contains_key("statvfs@openssh.com");
                    run_transfer(&op, &session, statvfs, attempt).await
                }
                Err(error) => map_transfer_error(error, "sftp_unavailable"),
            }
        };
        let outcome =
            match tokio::time::timeout(Duration::from_secs(TRANSFER_TIMEOUT_SECS), outcome).await {
                Ok(outcome) => outcome,
                Err(_) => TransferOutcome::Pending(AttachmentBlock::Timeout),
            };
        apply_outcome(&op, outcome, attempt, JobKind::Upload);
        let _ = session.close_session();
    });
}

/// Spawn the detached remote-delete over an already-initialized SFTP
/// stream. Removes only the generated names this operation owns.
pub(crate) fn start_remove<S>(op: Arc<Mutex<AttachmentOperation>>, stream: S, attempt: u64)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let session = RawSftpSession::new(stream);
        session.set_timeout(REQUEST_TIMEOUT_SECS);
        let outcome = async {
            match session.init().await {
                Ok(_) => run_remove(&op, &session).await,
                Err(error) => map_transfer_error(error, "sftp_unavailable"),
            }
        };
        let outcome =
            match tokio::time::timeout(Duration::from_secs(TRANSFER_TIMEOUT_SECS), outcome).await {
                Ok(outcome) => outcome,
                Err(_) => TransferOutcome::Pending(AttachmentBlock::Timeout),
            };
        apply_outcome(&op, outcome, attempt, JobKind::Remove);
        let _ = session.close_session();
    });
}

/// Spawn the detached remote re-verification for an `Uploaded` op being
/// re-armed after a destination/connection change — `lstat` + size + mode
/// on the recorded path, no re-upload.
pub(crate) fn start_verify<S>(op: Arc<Mutex<AttachmentOperation>>, stream: S, attempt: u64)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let session = RawSftpSession::new(stream);
        session.set_timeout(REQUEST_TIMEOUT_SECS);
        let outcome = async {
            match session.init().await {
                Ok(_) => run_verify(&op, &session).await,
                Err(error) => map_transfer_error(error, "sftp_unavailable"),
            }
        };
        let outcome =
            match tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), outcome).await {
                Ok(outcome) => outcome,
                Err(_) => TransferOutcome::Pending(AttachmentBlock::Timeout),
            };
        apply_outcome(&op, outcome, attempt, JobKind::Verify);
        let _ = session.close_session();
    });
}

fn apply_outcome(
    op: &Arc<Mutex<AttachmentOperation>>,
    outcome: TransferOutcome,
    attempt: u64,
    job: JobKind,
) {
    let Ok(mut op) = op.lock() else {
        return;
    };
    match job {
        JobKind::Remove => {
            // Only the recorded remove attempt may land; a superseded or
            // expired request's late result is dropped entirely.
            if op.remove_in_flight != Some(attempt) {
                return;
            }
            op.remove_in_flight = None;
            // Verified deletion is recorded even when the user cancelled
            // mid-flight, but the phase itself is untouched — `inserted`
            // is not revoked and failure keeps the uploaded state.
            if matches!(outcome, TransferOutcome::Removed) {
                op.removed = true;
                op.remote_path = None;
            }
            if op.cancel_requested {
                op.phase = AttachmentPhase::Cancelled;
                return;
            }
            match outcome {
                TransferOutcome::Removed => op.clear_status(),
                TransferOutcome::Pending(block) => op.note_block(block),
                TransferOutcome::Failed(code, message) => {
                    op.error_code = Some(code);
                    op.error_message = Some(message);
                }
                TransferOutcome::Uploaded(_)
                | TransferOutcome::Verified
                | TransferOutcome::RemoteMissing
                | TransferOutcome::Cancelled => {
                    op.note_block(AttachmentBlock::Internal);
                }
            }
        }
        JobKind::Verify => {
            if op.verify_in_flight != Some(attempt) {
                return;
            }
            op.verify_in_flight = None;
            if op.cancel_requested {
                return;
            }
            match outcome {
                // The recorded remote file is still intact: keep the op
                // `Uploaded` with the re-fenced destination so insert can
                // proceed — no bytes were re-sent.
                TransferOutcome::Verified => op.clear_status(),
                TransferOutcome::RemoteMissing => {
                    // Gone or replaced — do not let a stale remote path be
                    // inserted; an explicit retry uploads again.
                    op.remote_path = None;
                    op.removed = false;
                    if op.phase == AttachmentPhase::Uploaded {
                        op.phase = AttachmentPhase::Pending;
                    }
                    op.note_block(AttachmentBlock::RemoteMissing);
                }
                TransferOutcome::Pending(block) => op.note_block(block),
                TransferOutcome::Failed(code, message) => {
                    op.error_code = Some(code);
                    op.error_message = Some(message);
                }
                TransferOutcome::Uploaded(_)
                | TransferOutcome::Removed
                | TransferOutcome::Cancelled => op.note_block(AttachmentBlock::Internal),
            }
        }
        JobKind::Upload => {
            // A completion/progress from a superseded attempt belongs to a
            // dead detached task — the retried operation must not move.
            if op.attempt != attempt {
                return;
            }
            if matches!(
                outcome,
                TransferOutcome::Removed
                    | TransferOutcome::Verified
                    | TransferOutcome::RemoteMissing
            ) {
                return;
            }
            if op.cancel_requested {
                // A delayed completion must not resurrect a cancelled
                // operation or change its visible state.
                op.phase = AttachmentPhase::Cancelled;
                return;
            }
            match outcome {
                TransferOutcome::Uploaded(path) => {
                    if op.phase == AttachmentPhase::Uploading {
                        op.phase = AttachmentPhase::Uploaded;
                        op.remote_path = Some(path);
                        op.clear_status();
                    }
                }
                TransferOutcome::Pending(block) => op.pend(block),
                TransferOutcome::Failed(code, message) => op.fail(code, message),
                TransferOutcome::Cancelled => {
                    if matches!(
                        op.phase,
                        AttachmentPhase::Uploading | AttachmentPhase::Pending
                    ) {
                        op.phase = AttachmentPhase::Cancelled;
                    }
                }
                TransferOutcome::Removed
                | TransferOutcome::Verified
                | TransferOutcome::RemoteMissing => {}
            }
        }
    }
}

fn op_cancelled(op: &Arc<Mutex<AttachmentOperation>>) -> bool {
    op.lock().map(|op| op.cancel_requested).unwrap_or(true)
}

/// Transfer progress reporter — like the outcome itself, progress from a
/// superseded attempt is discarded so the retried op keeps clean state.
fn op_progress(op: &Arc<Mutex<AttachmentOperation>>, bytes: u64, attempt: u64) {
    if let Ok(mut op) = op.lock()
        && op.attempt == attempt
        && op.phase == AttachmentPhase::Uploading
    {
        op.bytes_uploaded = bytes;
    }
}

fn map_transfer_error(error: SftpError, context: &'static str) -> TransferOutcome {
    match &error {
        SftpError::Status(status) => match status.status_code {
            StatusCode::PermissionDenied => TransferOutcome::Failed(
                "remote_permission_denied",
                "remote host denied the attachment operation".to_owned(),
            ),
            _ => TransferOutcome::Failed(context, sanitize_remote_message(&status.error_message)),
        },
        SftpError::Timeout => TransferOutcome::Pending(AttachmentBlock::Timeout),
        SftpError::IO(_) | SftpError::UnexpectedPacket | SftpError::UnexpectedBehavior(_) => {
            TransferOutcome::Pending(AttachmentBlock::StaleConnection)
        }
        SftpError::Limited(_) => TransferOutcome::Failed(
            "remote_no_space",
            "server limits rejected the write".to_owned(),
        ),
    }
}

/// Strip control characters and bound a remote error string. A remote
/// message is never trusted to be single-line or ASCII.
fn sanitize_remote_message(message: &str) -> String {
    let cleaned: String = message
        .chars()
        .map(|ch| {
            if ch.is_control() && !matches!(ch, ' ' | '\t') {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    let mut end = cleaned.len().min(200);
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_owned()
}

fn mode_only(mode: u32) -> FileAttributes {
    FileAttributes {
        permissions: Some(mode),
        ..FileAttributes::default()
    }
}

fn is_no_such_file(error: &SftpError) -> bool {
    matches!(
        error,
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile
    )
}

/// `lstat` file type plus uid plus permission bits for the reuse and safety
/// checks. `lstat` never follows the final component, so a symlinked
/// directory component is detected by `file_type() == Symlink` on its own
/// level — the server itself would follow it silently.
fn remote_dir_safe(attrs: &FileAttributes, expected_uid: Option<u32>) -> bool {
    attrs.file_type() == FileType::Dir
        && (expected_uid.is_none() || attrs.uid.is_none() || attrs.uid == expected_uid)
        && attrs.permissions.is_some_and(|mode| mode & 0o077 == 0)
}

fn remote_file_complete(attrs: &FileAttributes, size_bytes: u64) -> bool {
    attrs.file_type().is_file()
        && attrs.size == Some(size_bytes)
        && attrs
            .permissions
            .is_some_and(|mode| mode & 0o777 == REMOTE_FILE_MODE)
}

/// Ensure `components` resolves to a real directory. The first
/// `verified` components are trusted (the `realpath(".")` home); every
/// remaining component is lstat-checked — a symlink anywhere in the chain
/// is rejected because OpenSSH follows symlinks in path components
/// silently. Missing components are created with `0700` only when
/// `create_missing` is set (the app-private default chain); an explicit
/// user directory must already exist. The last `restrict_last`
/// components are app-owned (`meeterm/` and `attachments/`): **each** of
/// them is uid-checked against the home owner and forced to `0700` when
/// its mode deviates — a pre-existing `0755` `meeterm/` would otherwise
/// let a group/other writer replace `attachments/` underneath us.
/// Modes of non-app-owned components (`.local`, `.local/share`) and of an
/// explicit user directory are never changed.
async fn ensure_remote_dir(
    session: &RawSftpSession,
    components: &[String],
    verified: usize,
    create_missing: bool,
    restrict_last: usize,
    expected_uid: Option<u32>,
) -> Result<(), TransferOutcome> {
    let mut path = String::new();
    for (index, component) in components.iter().enumerate() {
        if index == 0 {
            path.push_str(component);
        } else {
            path.push('/');
            path.push_str(component);
        }
        if path.is_empty() || index < verified {
            continue;
        }
        let last = index + 1 == components.len();
        let app_owned = index + 1 > components.len().saturating_sub(restrict_last);
        let mut attrs = match session.lstat(&path).await {
            Ok(attrs) => attrs.attrs,
            Err(error) if is_no_such_file(&error) => {
                if !create_missing {
                    // An explicit directory must exist; we never create
                    // or fix up a user-chosen path.
                    return Err(TransferOutcome::Failed(
                        "remote_unsafe_path",
                        "explicit remote directory does not exist".to_owned(),
                    ));
                }
                session
                    .mkdir(&path, mode_only(REMOTE_DIR_MODE))
                    .await
                    .map_err(|error| map_transfer_error(error, "sftp_error"))?;
                session
                    .lstat(&path)
                    .await
                    .map_err(|error| map_transfer_error(error, "sftp_error"))?
                    .attrs
            }
            Err(error) => return Err(map_transfer_error(error, "sftp_error")),
        };
        if attrs.file_type() != FileType::Dir {
            // A symlink here is a traversal attempt surface: the OpenSSH
            // server would happily follow it on any later open. Refuse
            // rather than rely on server-side path handling.
            return Err(TransferOutcome::Failed(
                "remote_unsafe_path",
                "a path component is not a real directory".to_owned(),
            ));
        }
        if app_owned || last {
            // App-owned components are restricted wherever they sit; an
            // explicit leaf still gets the uid check (it is never chmod'd).
            if expected_uid.is_some() && attrs.uid.is_some() && attrs.uid != expected_uid {
                return Err(TransferOutcome::Failed(
                    "remote_unsafe_path",
                    "attachment directory is owned by another user".to_owned(),
                ));
            }
        }
        if app_owned && attrs.permissions.is_none_or(|mode| mode & 0o077 != 0) {
            session
                .setstat(&path, mode_only(REMOTE_DIR_MODE))
                .await
                .map_err(|error| map_transfer_error(error, "sftp_error"))?;
            attrs = session
                .lstat(&path)
                .await
                .map_err(|error| map_transfer_error(error, "sftp_error"))?
                .attrs;
            if !remote_dir_safe(&attrs, expected_uid) {
                return Err(TransferOutcome::Failed(
                    "remote_permission_denied",
                    "attachment directory permissions could not be restricted".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

async fn cleanup_partial(session: &RawSftpSession, partial: &str) {
    // Best-effort only: the private `.partial-*` namespace is ours, so a
    // leftover is always safe to remove on the next attempt.
    let _ = session.remove(partial).await;
}

async fn run_transfer(
    op: &Arc<Mutex<AttachmentOperation>>,
    session: &RawSftpSession,
    statvfs_ext: bool,
    attempt: u64,
) -> TransferOutcome {
    let spec = {
        let Ok(op) = op.lock() else {
            return TransferOutcome::Cancelled;
        };
        op.spec.clone()
    };
    macro_rules! cancel_check {
        () => {
            if op_cancelled(op) {
                return TransferOutcome::Cancelled;
            }
        };
    }
    macro_rules! sftp {
        ($future:expr) => {
            match $future.await {
                Ok(value) => value,
                Err(error) => return map_transfer_error(error, "sftp_error"),
            }
        };
    }
    cancel_check!();

    // Resolve the remote start directory without ever constructing `~`
    // client-side. realpath output is server-controlled, so anything that
    // could smuggle a control character into the inserted line is refused.
    let home = {
        let name = sftp!(session.realpath("."));
        let Some(home) = name.files.first().map(|file| file.filename.clone()) else {
            return TransferOutcome::Failed("remote_unsafe_path", "no remote home".to_owned());
        };
        if !home.starts_with('/')
            || home.len() > 2048
            || home.chars().any(char::is_control)
            || home.split('/').any(|part| part == "..")
        {
            return TransferOutcome::Failed(
                "remote_unsafe_path",
                "remote home path is not a clean absolute path".to_owned(),
            );
        }
        home.trim_end_matches('/').to_owned()
    };
    let home_attrs = sftp!(session.lstat(&home)).attrs;
    if home_attrs.file_type() != FileType::Dir {
        return TransferOutcome::Failed(
            "remote_unsafe_path",
            "remote home is not a directory".to_owned(),
        );
    }
    // Default: app-private directory under the SFTP start dir (created
    // and restricted). Explicit: a clean absolute or `~/`-prefixed path
    // the user chose; every component still gets lstat'd (symlinks
    // rejected) but nothing is created or chmod'ed, and writability is
    // proven by an exclusive-create probe.
    let (components, verified, create_missing, restrict_last, probe) =
        match remote_dir_components(&spec, &home) {
            Ok(resolved) => resolved,
            Err(outcome) => return outcome,
        };
    let base = components.join("/");
    if let Err(outcome) = ensure_remote_dir(
        session,
        &components,
        verified,
        create_missing,
        restrict_last,
        home_attrs.uid,
    )
    .await
    {
        return outcome;
    }
    if probe && let Err(outcome) = probe_dir_writable(session, &base).await {
        return outcome;
    }
    // Record the canonical base this upload established — the delete path
    // re-resolves and requires byte equality instead of trusting whatever
    // the directory resolves to later.
    if let Ok(mut op) = op.lock()
        && op.attempt == attempt
    {
        op.remote_base = Some(base.clone());
    }
    cancel_check!();

    // Advisory capacity check where the OpenSSH extension exists.
    if statvfs_ext && let Ok(stat) = session.statvfs(&base).await {
        let available = stat.blocks_avail.saturating_mul(stat.fragment_size.max(1));
        if available < spec.size_bytes.saturating_add(REMOTE_SPACE_SLACK) {
            return TransferOutcome::Failed(
                "remote_no_space",
                "remote filesystem reports insufficient space".to_owned(),
            );
        }
    }

    let final_path = format!("{base}/{}", spec.remote_name);
    let partial_path = format!("{base}/{}", spec.partial_name);

    // Same-endpoint reuse: an earlier interrupted retry may already have
    // published this exact file. Only a fully verified file qualifies.
    match session.lstat(&final_path).await {
        Ok(attrs) if remote_file_complete(&attrs.attrs, spec.size_bytes) => {
            return TransferOutcome::Uploaded(final_path);
        }
        Ok(_) => {
            return TransferOutcome::Failed(
                "remote_name_collision",
                "attachment name already exists on the remote".to_owned(),
            );
        }
        Err(error) if is_no_such_file(&error) => {}
        Err(error) => return map_transfer_error(error, "sftp_error"),
    }
    cancel_check!();

    // Revalidate the local file right before streaming: the adapter must
    // keep the picked file stable for the upload's lifetime.
    let mut file = match tokio::fs::File::open(&spec.local_path).await {
        Ok(file) => file,
        Err(_) => {
            return TransferOutcome::Failed(
                "source_unreadable",
                "the selected image file could not be opened".to_owned(),
            );
        }
    };
    match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() == spec.size_bytes => {}
        _ => {
            return TransferOutcome::Failed(
                "source_unreadable",
                "the selected image changed before upload".to_owned(),
            );
        }
    }

    // Remove a stale partial from an earlier cancelled attempt, then
    // exclusive-create ours. EXCLUDE makes a concurrent second writer fail
    // rather than truncate.
    cleanup_partial(session, &partial_path).await;
    let handle = match session
        .open(
            &partial_path,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            mode_only(REMOTE_FILE_MODE),
        )
        .await
    {
        Ok(handle) => handle.handle,
        Err(error) => return map_transfer_error(error, "sftp_error"),
    };

    let mut buffer = vec![0u8; WRITE_CHUNK_BYTES];
    let mut offset = 0u64;
    let upload = async {
        loop {
            if op_cancelled(op) {
                return TransferOutcome::Cancelled;
            }
            let read = match file.read(&mut buffer).await {
                Ok(read) => read,
                Err(_) => {
                    return TransferOutcome::Failed(
                        "source_unreadable",
                        "the selected image could not be read".to_owned(),
                    );
                }
            };
            if read == 0 {
                break;
            }
            if offset.saturating_add(read as u64) > spec.size_bytes {
                return TransferOutcome::Failed(
                    "source_unreadable",
                    "the selected image grew during upload".to_owned(),
                );
            }
            if let Err(error) = session
                .write(handle.clone(), offset, buffer[..read].to_vec())
                .await
            {
                return map_transfer_error(error, "sftp_error");
            }
            offset += read as u64;
            op_progress(op, offset, attempt);
        }
        if offset != spec.size_bytes {
            return TransferOutcome::Failed(
                "source_unreadable",
                "the selected image shrank during upload".to_owned(),
            );
        }
        // The CLOSE reply is the only signal the remote flushed/committed
        // the staged bytes — publish is forbidden until it succeeds.
        let close = sftp!(session.close(handle.clone()));
        if close.status_code != StatusCode::Ok {
            return TransferOutcome::Failed(
                "sftp_error",
                "remote did not confirm the file close".to_owned(),
            );
        }
        // Confirm the staged bytes before publishing the final name.
        match session.lstat(&partial_path).await {
            Ok(attrs)
                if attrs.attrs.file_type().is_file()
                    && attrs.attrs.size == Some(spec.size_bytes) => {}
            Ok(_) => {
                return TransferOutcome::Failed(
                    "sftp_error",
                    "partial file metadata did not verify".to_owned(),
                );
            }
            Err(error) => return map_transfer_error(error, "sftp_error"),
        }
        // SFTP v3 rename never overwrites; a colliding final name keeps the
        // existing verified file instead of losing bytes.
        match session.rename(&partial_path, &final_path).await {
            Ok(_) => {}
            Err(error) => {
                // A rename failure can mean the final appeared concurrently;
                // only a verified identical file is acceptable.
                if let Ok(attrs) = session.lstat(&final_path).await
                    && remote_file_complete(&attrs.attrs, spec.size_bytes)
                {
                    return TransferOutcome::Uploaded(final_path);
                }
                return map_transfer_error(error, "remote_name_collision");
            }
        }
        match session.lstat(&final_path).await {
            Ok(attrs) if remote_file_complete(&attrs.attrs, spec.size_bytes) => {
                TransferOutcome::Uploaded(final_path)
            }
            Ok(_) => TransferOutcome::Failed(
                "sftp_error",
                "uploaded file metadata did not verify".to_owned(),
            ),
            Err(error) => map_transfer_error(error, "sftp_error"),
        }
    };
    let outcome = upload.await;
    if !matches!(outcome, TransferOutcome::Uploaded(_)) {
        cleanup_partial(session, &partial_path).await;
    }
    outcome
}

/// Re-resolve the base directory the same way the upload did — including
/// `~/` expansion against `realpath(".")` — without failing when it is
/// already gone: deletion must be idempotent. The caller still validates
/// every resolved component before touching anything inside.
async fn remove_base(session: &RawSftpSession, spec: &TransferSpec) -> Option<String> {
    let resolve_home = || async {
        let name = session.realpath(".").await.ok()?;
        let home = name.files.first().map(|file| file.filename.clone())?;
        if !home.starts_with('/') || home.chars().any(char::is_control) {
            return None;
        }
        Some(home.trim_end_matches('/').to_owned())
    };
    match &spec.remote_dir {
        Some(dir) => {
            if let Some(rest) = dir.strip_prefix("~/") {
                let home = resolve_home().await?;
                Some(format!("{home}/{rest}"))
            } else if dir.starts_with('/') {
                Some(dir.clone())
            } else {
                None
            }
        }
        None => {
            let home = resolve_home().await?;
            let mut components = vec![home];
            components.extend(REMOTE_DIR_COMPONENTS.iter().map(|part| (*part).to_owned()));
            Some(components.join("/"))
        }
    }
}

/// Walk every component of `base` with `lstat` and require a real
/// directory at each level — the same guarantee `ensure_remote_dir`
/// established at upload time. A symlink, a non-directory, or a missing
/// component in the middle means the parent was replaced: deletion must
/// refuse rather than follow a redirect into foreign files. A base that
/// vanished entirely is reported as `Ok(false)` — nothing left to delete.
/// Any other `Ok(true)` means the full chain is verified real.
async fn remote_base_verified(
    session: &RawSftpSession,
    base: &str,
) -> Result<bool, TransferOutcome> {
    let components = path_components(base);
    let mut path = String::new();
    for (index, component) in components.iter().enumerate() {
        if index == 0 {
            path.push_str(component);
        } else {
            path.push('/');
            path.push_str(component);
        }
        if path.is_empty() {
            continue;
        }
        match session.lstat(&path).await {
            Ok(attrs) if attrs.attrs.file_type() == FileType::Dir => {}
            // A symlink or a non-directory component is a parent swap —
            // refuse, never follow.
            Ok(_) => {
                return Err(TransferOutcome::Failed(
                    "remote_unsafe_path",
                    "a remote directory component is not a real directory".to_owned(),
                ));
            }
            Err(error) if is_no_such_file(&error) => return Ok(false),
            Err(error) => return Err(map_transfer_error(error, "sftp_error")),
        }
    }
    Ok(true)
}

/// One remote-delete step: the path must be absent, or a regular file
/// with `0600` — never a symlink, directory, or a foreign mode. Anything
/// else refuses the delete rather than trusting server-side expansion.
async fn remove_checked(
    session: &RawSftpSession,
    path: &str,
    what: &'static str,
) -> Result<(), TransferOutcome> {
    match session.lstat(path).await {
        Err(error) if is_no_such_file(&error) => Ok(()),
        Err(error) => Err(map_transfer_error(error, "sftp_error")),
        Ok(attrs) => {
            let attrs = &attrs.attrs;
            if !attrs.file_type().is_file()
                || attrs
                    .permissions
                    .is_none_or(|mode| mode & 0o777 != REMOTE_FILE_MODE)
            {
                return Err(TransferOutcome::Failed(
                    "remote_unsafe_path",
                    format!("remote {what} is not a private generated file"),
                ));
            }
            session
                .remove(path)
                .await
                .map(|_| ())
                .map_err(|error| map_transfer_error(error, "sftp_error"))
        }
    }
}

/// Explicit remote deletion restricted to the names this operation
/// generated: the published file and its `.meeterm-partial-*` remnant —
/// only for names matching the generated grammar — and, only for the
/// app-private default, the (empty) attachments directory itself. Before
/// anything is removed the whole base path is re-validated: the resolved
/// base must equal the canonical directory the upload recorded, and every
/// component must still be a real directory — a symlinked or replaced
/// parent is refused with `remote_unsafe_path` instead of being followed
/// into a foreign directory. Every step is idempotent so a repeated
/// remove or a partially cleaned state still converges to `Removed`.
async fn run_remove(
    op: &Arc<Mutex<AttachmentOperation>>,
    session: &RawSftpSession,
) -> TransferOutcome {
    let (spec, recorded_base) = {
        let Ok(op) = op.lock() else {
            return TransferOutcome::Cancelled;
        };
        (op.spec.clone(), op.remote_base.clone())
    };
    // The name grammar is validated before any delete touches the remote;
    // a name we did not generate is never removed by this client.
    if !remote_basename_valid(&spec.remote_name) {
        return TransferOutcome::Failed(
            "remote_unsafe_path",
            "remote file name is outside the generated grammar".to_owned(),
        );
    }
    let Some(base) = remove_base(session, &spec).await else {
        return TransferOutcome::Pending(AttachmentBlock::StaleConnection);
    };
    // The upload recorded the canonical directory it wrote into; a base
    // that resolves differently now (realpath moved, dir replaced) is
    // never followed.
    if let Some(recorded) = recorded_base
        && recorded != base
    {
        return TransferOutcome::Failed(
            "remote_unsafe_path",
            "remote directory no longer resolves to the recorded base".to_owned(),
        );
    }
    match remote_base_verified(session, &base).await {
        Ok(true) => {}
        // The base is gone entirely — nothing of ours can remain.
        Ok(false) => return TransferOutcome::Removed,
        Err(outcome) => return outcome,
    }
    let final_path = format!("{base}/{}", spec.remote_name);
    let partial_path = format!("{base}/{}", spec.partial_name);

    if let Err(outcome) = remove_checked(session, &final_path, "file").await {
        return outcome;
    }
    if let Err(outcome) = remove_checked(session, &partial_path, "partial file").await {
        return outcome;
    }
    if spec.remote_dir.is_none() {
        // rmdir fails harmlessly while the directory is non-empty; never
        // descend into or delete contents that are not ours.
        let _ = session.rmdir(&base).await;
    }
    // Verify the published name is really gone before flagging removal.
    match session.lstat(&final_path).await {
        Err(error) if is_no_such_file(&error) => TransferOutcome::Removed,
        Ok(_) => TransferOutcome::Failed(
            "sftp_error",
            "remote file is still present after deletion".to_owned(),
        ),
        Err(error) => map_transfer_error(error, "sftp_error"),
    }
}

/// Remote re-verification for an `Uploaded` op whose destination fence was
/// refreshed after recovery — the file must still be a regular file with
/// the recorded size and the private `0600` mode at the recorded path.
/// Anything else means the file is gone or was replaced: the op drops to
/// `Pending(remote_missing)` so no stale path is ever inserted, and an
/// explicit retry uploads it again. No bytes are transferred here.
async fn run_verify(
    op: &Arc<Mutex<AttachmentOperation>>,
    session: &RawSftpSession,
) -> TransferOutcome {
    let (remote_path, size_bytes) = {
        let Ok(op) = op.lock() else {
            return TransferOutcome::Cancelled;
        };
        match (op.remote_path.clone(), op.phase) {
            (Some(path), AttachmentPhase::Uploaded) => (path, op.spec.size_bytes),
            // Nothing to verify (removed/never uploaded) — still a clean
            // verify outcome; apply_outcome leaves the phase untouched.
            _ => return TransferOutcome::Verified,
        }
    };
    match session.lstat(&remote_path).await {
        Ok(attrs) if remote_file_complete(&attrs.attrs, size_bytes) => TransferOutcome::Verified,
        // Exists but wrong type/size/mode — it is not the file we wrote.
        Ok(_) => TransferOutcome::RemoteMissing,
        Err(error) if is_no_such_file(&error) => TransferOutcome::RemoteMissing,
        // Timeouts and channel failures are transient — keep Uploaded and
        // let a later explicit retry re-verify.
        Err(error) => map_transfer_error(error, "sftp_error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(pane: u64, native: u64) -> DestinationFence {
        DestinationFence {
            generation: 7,
            operation_epoch: 3,
            pane_id: pane,
            native_terminal: native,
            herdr_terminal_id: None,
            endpoint: AttachmentEndpoint {
                host: "example.test".to_owned(),
                port: 22,
                username: "user".to_owned(),
                backend: Backend::Tmux,
                runtime: Some("meeterm".to_owned()),
            },
        }
    }

    fn test_op(phase: AttachmentPhase) -> Arc<Mutex<AttachmentOperation>> {
        Arc::new(Mutex::new(AttachmentOperation {
            id: 41,
            intent: AttachmentIntent {
                id: 1,
                target_terminal: 90,
                owner_terminal: 9,
                endpoint: fence(12, 90).endpoint,
                pane_id: 12,
                herdr_terminal_id: None,
            },
            fence: fence(12, 90),
            attempt: 1,
            spec: TransferSpec {
                local_path: "/tmp/picked.jpg".to_owned(),
                remote_dir: None,
                remote_name: "meeterm-20260101-120000-0123456789abcdef.png".to_owned(),
                partial_name: ".meeterm-partial-meeterm-20260101-120000-0123456789abcdef.png"
                    .to_owned(),
                size_bytes: 10,
            },
            display_name: "picked.jpg".to_owned(),
            phase,
            reason_code: None,
            reason_message: None,
            error_code: None,
            error_message: None,
            remote_path: None,
            remote_base: None,
            bytes_uploaded: 0,
            cancel_requested: false,
            insert_enqueued: false,
            removed: false,
            remove_in_flight: None,
            verify_in_flight: None,
        }))
    }

    #[test]
    fn remote_file_name_matches_generated_grammar() {
        let name = remote_file_name(7, ".png");
        assert!(remote_basename_valid(&name), "generated name: {name}");
        assert!(
            name.bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-')),
            "name must stay inside [a-z0-9.-]: {name}"
        );
        assert!(!name.contains("picked") && !name.contains(' '));
        assert_ne!(
            remote_file_name(7, ".png"),
            remote_file_name(8, ".png"),
            "random tail keeps same-second names distinct"
        );
        // Two calls with the same id still differ — process restarts that
        // reset the id sequence cannot collide on names.
        assert_ne!(
            remote_file_name(7, ".png"),
            remote_file_name(7, ".png"),
            "random tail keeps same-id names distinct"
        );
    }

    #[test]
    fn remote_basename_valid_is_strict() {
        for name in [
            "meeterm-20260101-120000-0123456789abcdef.png",
            "meeterm-19991231-235959-deadbeefcafebabe",
        ] {
            assert!(remote_basename_valid(name), "accept {name}");
        }
        for name in [
            "",
            "att-1-abcdef.png",
            "meeterm-20260101-120000-0123456789abc.png", // 15 hex
            "meeterm-20260101-120000-0123456789abcdefg.png", // 17 hex
            "meeterm-2026010-120000-0123456789abcdef.png", // short date
            "meeterm-20260101-12000-0123456789abcdef.png", // short time
            "meeterm-20260101-120000-0123456789ABCDEF.png", // uppercase hex
            "meeterm_20260101_120000_0123456789abcdef.png",
            "meeterm-20260101-120000-0123456789abcdef.p'ng",
            "meeterm-20260101-120000-0123456789abcdef.png/extra",
            "meeterm-20260101-120000-0123456789abcdef.pn g",
            "meeterm-20260101-120000-0123456789abcdef.PNG",
            ".meeterm-partial-meeterm-20260101-120000-0123456789abcdef.png",
        ] {
            assert!(!remote_basename_valid(name), "reject {name:?}");
        }
    }

    #[test]
    fn stamp_from_millis_is_utc() {
        // 1970-01-01 00:00:00 UTC
        assert_eq!(stamp_from_millis(0), (19700101, 0));
        // 2000-02-29 23:59:59 UTC (leap day)
        assert_eq!(stamp_from_millis(951_868_799_000), (20000229, 235959));
        // 2024-12-31 23:59:59 UTC
        assert_eq!(stamp_from_millis(1_735_689_599_000), (20241231, 235959));
        // 2025-01-01 00:00:00 UTC
        assert_eq!(stamp_from_millis(1_735_689_600_000), (20250101, 0));
    }

    #[test]
    fn image_extension_comes_from_magic() {
        let dir = std::env::temp_dir().join(format!("att-magic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("picked image.jpg"); // lying picked name
        std::fs::write(
            &png,
            [
                &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..],
                &[0u8; 8][..],
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(
            image_extension(png.to_str().unwrap(), png.to_str().unwrap()),
            ".png",
            "magic wins over picked extension"
        );
        let jpg = dir.join("picked.png");
        std::fs::write(&jpg, [0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0]).unwrap();
        assert_eq!(
            image_extension(jpg.to_str().unwrap(), jpg.to_str().unwrap()),
            ".jpg"
        );
        // Unknown magic falls back to a clean picked extension only.
        let other = dir.join("payload.HEIC");
        std::fs::write(&other, b"not-an-image").unwrap();
        assert_eq!(
            image_extension(other.to_str().unwrap(), other.to_str().unwrap()),
            "",
            "uppercase picked extension is dropped, never lowercased in"
        );
        let unknown = dir.join("payload.bin");
        std::fs::write(&unknown, b"???").unwrap();
        assert_eq!(
            image_extension(unknown.to_str().unwrap(), unknown.to_str().unwrap()),
            ".bin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_remote_dir_acceptance_shape() {
        assert_eq!(
            validate_remote_dir("/var/tmp/attachments").unwrap(),
            "/var/tmp/attachments"
        );
        assert_eq!(validate_remote_dir("/home/u/dir/").unwrap(), "/home/u/dir");
        assert_eq!(validate_remote_dir("~/my dir").unwrap(), "~/my dir");
        for dir in [
            "",
            "relative/dir",
            "~",
            "~/",
            "a/b",
            "~/..bad".repeat(60).as_str(),
        ] {
            assert!(validate_remote_dir(dir).is_err(), "must reject {dir:?}");
        }
    }

    fn spec_with_dir(dir: Option<&str>) -> TransferSpec {
        TransferSpec {
            local_path: "/tmp/x.png".to_owned(),
            remote_dir: dir.map(str::to_owned),
            remote_name: "meeterm-20260101-120000-0123456789abcdef.png".to_owned(),
            partial_name: ".meeterm-partial-meeterm-20260101-120000-0123456789abcdef.png"
                .to_owned(),
            size_bytes: 1,
        }
    }

    #[test]
    fn remote_dir_components_expands_tilde_against_home() {
        let (components, verified, create, restrict, probe) =
            remote_dir_components(&spec_with_dir(Some("~/my dir/deep")), "/home/u")
                .expect("tilde dir resolves");
        assert_eq!(components.join("/"), "/home/u/my dir/deep");
        // `/` marker + `home` + `u` — the realpath-verified prefix.
        assert_eq!(verified, 3, "home components are pre-verified");
        assert!(!create && restrict == 0 && probe, "explicit dir is probed");

        // Default dir: home-verified prefix, creatable, app-private tail.
        let (components, verified, create, restrict, probe) =
            remote_dir_components(&spec_with_dir(None), "/home/u").expect("default dir");
        assert_eq!(
            components.join("/"),
            "/home/u/.local/share/meeterm/attachments"
        );
        assert_eq!(verified, 3);
        assert!(create && restrict == APP_DIR_COMPONENTS && !probe);
    }

    #[test]
    fn remote_dir_components_rejects_unsafe() {
        for dir in [
            "/a/../b",
            "/a/./b",
            "/a//b",
            "/a\nb",
            "/a\rb",
            "/a'b",
            "~/x/../y",
            "~/../escape",
            "relative/dir",
        ] {
            let spec = spec_with_dir(Some(dir));
            match remote_dir_components(&spec, "/home/u") {
                Err(TransferOutcome::Failed(code, _)) => {
                    assert_eq!(code, "remote_unsafe_path", "reject {dir:?}")
                }
                _ => panic!("{dir:?} must fail with remote_unsafe_path"),
            }
        }
    }

    #[test]
    fn insertion_line_is_single_line_quoted() {
        let line = insertion_line("/home/a b/.local/share/meeterm/attachments/m.png").unwrap();
        assert_eq!(line, b"'/home/a b/.local/share/meeterm/attachments/m.png'");
        assert!(!line.contains(&b'\n') && !line.contains(&b'\r'));
    }

    #[test]
    fn insertion_line_rejects_dangerous_paths_before_generating() {
        // `'`, CR, LF, and every control character are refused *before* a
        // line exists — the pane must never receive an escaped-smuggle.
        for path in [
            "/home/o'x/m.png",
            "/home/u/m.png\n",
            "/home/u/m.png\r",
            "/home/u/\u{7}m.png",
            "/home/u/m.p\u{1b}ng",
        ] {
            assert!(
                insertion_line(path).is_err(),
                "must reject {path:?} before generating the line"
            );
        }
        for byte in 0u8..=0x1f {
            let path = format!("/home/u/{}.png", byte as char);
            assert!(
                insertion_line(&path).is_err(),
                "control byte {byte:#x} must be rejected"
            );
        }
    }

    #[test]
    fn pend_never_resurrects_terminal_states() {
        let op = test_op(AttachmentPhase::Cancelled);
        {
            let mut op = op.lock().unwrap();
            op.pend(AttachmentBlock::Timeout);
            assert_eq!(op.phase, AttachmentPhase::Cancelled);
            assert!(op.reason_code.is_none());
        }
        let op = test_op(AttachmentPhase::Failed);
        {
            let mut op = op.lock().unwrap();
            op.fail("x", "y");
            assert_eq!(op.phase, AttachmentPhase::Failed);
        }
    }

    #[test]
    fn cancel_marks_flag_and_phase() {
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.cancel();
            assert_eq!(op.phase, AttachmentPhase::Cancelled);
            assert!(op.cancel_requested);
        }
        assert!(op_cancelled(&op));
    }

    #[test]
    fn delayed_uploaded_after_cancel_is_discarded() {
        let op = test_op(AttachmentPhase::Cancelled);
        {
            let mut op = op.lock().unwrap();
            op.cancel_requested = true;
        }
        apply_outcome(
            &op,
            TransferOutcome::Uploaded("/remote/x".to_owned()),
            1,
            JobKind::Upload,
        );
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Cancelled);
        assert!(op.remote_path.is_none());
    }

    #[test]
    fn uploaded_outcome_sets_path_and_clears_status() {
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.note_block(AttachmentBlock::Timeout);
        }
        apply_outcome(
            &op,
            TransferOutcome::Uploaded("/remote/final".to_owned()),
            1,
            JobKind::Upload,
        );
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Uploaded);
        assert_eq!(op.remote_path.as_deref(), Some("/remote/final"));
        assert!(op.reason_code.is_none());
    }

    /// FP-008 regression (reviewer repro, attempt-aware): a detached
    /// transfer task from the superseded attempt must never move the
    /// retried op — its completion and progress are both dropped.
    #[test]
    fn stale_attempt_outcome_cannot_finish_retry() {
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.attempt += 1;
        }
        apply_outcome(
            &op,
            TransferOutcome::Uploaded("/old-generation/path.png".to_owned()),
            1,
            JobKind::Upload,
        );
        op_progress(&op, 5, 1);
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Uploading);
        assert!(op.remote_path.is_none());
        assert_eq!(op.bytes_uploaded, 0);
    }

    #[test]
    fn gate_launch_rejects_wrong_generation() {
        let op = test_op(AttachmentPhase::Uploading);
        assert!(!gate_launch(&op, 9, 8, 1));
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Pending);
        assert_eq!(op.reason_code, Some("stale_operation"));
    }

    #[test]
    fn gate_launch_rejects_stale_attempt() {
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.attempt += 1;
        }
        assert!(!gate_launch(&op, 9, 7, 1));
    }

    #[test]
    fn gate_launch_rejects_cancelled() {
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.cancel();
        }
        assert!(!gate_launch(&op, 9, 7, 1));
        assert_eq!(op.lock().unwrap().phase, AttachmentPhase::Cancelled);
    }

    #[test]
    fn gate_launch_accepts_current() {
        let op = test_op(AttachmentPhase::Uploading);
        assert!(gate_launch(&op, 9, 7, 1));
    }

    #[test]
    fn verify_outcome_missing_remote_drops_to_pending() {
        let op = test_op(AttachmentPhase::Uploaded);
        {
            let mut op = op.lock().unwrap();
            op.remote_path = Some("/remote/final".to_owned());
            op.verify_in_flight = Some(2);
            op.attempt = 2;
        }
        apply_outcome(&op, TransferOutcome::RemoteMissing, 2, JobKind::Verify);
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Pending);
        assert_eq!(op.reason_code, Some("remote_missing"));
        assert!(op.remote_path.is_none());
        assert!(op.verify_in_flight.is_none());
    }

    #[test]
    fn verify_outcome_verified_keeps_uploaded() {
        let op = test_op(AttachmentPhase::Uploaded);
        {
            let mut op = op.lock().unwrap();
            op.remote_path = Some("/remote/final".to_owned());
            op.verify_in_flight = Some(2);
            op.attempt = 2;
            op.note_block(AttachmentBlock::StaleOperation);
        }
        apply_outcome(&op, TransferOutcome::Verified, 2, JobKind::Verify);
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Uploaded);
        assert_eq!(op.remote_path.as_deref(), Some("/remote/final"));
        assert!(op.reason_code.is_none());
    }

    #[test]
    fn stale_verify_outcome_is_dropped() {
        let op = test_op(AttachmentPhase::Uploaded);
        {
            let mut op = op.lock().unwrap();
            op.remote_path = Some("/remote/final".to_owned());
            op.verify_in_flight = Some(2);
        }
        // A completion carrying an earlier attempt must not clear the
        // in-flight marker or the remote-missing state.
        apply_outcome(&op, TransferOutcome::RemoteMissing, 1, JobKind::Verify);
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Uploaded);
        assert_eq!(op.verify_in_flight, Some(2));
    }

    #[test]
    fn stale_remove_outcome_is_dropped() {
        let op = test_op(AttachmentPhase::Uploaded);
        {
            let mut op = op.lock().unwrap();
            op.remove_in_flight = Some(2);
        }
        apply_outcome(&op, TransferOutcome::Removed, 1, JobKind::Remove);
        let op = op.lock().unwrap();
        assert!(!op.removed);
        assert_eq!(op.remove_in_flight, Some(2));
    }

    #[test]
    fn snapshot_is_bounded_and_sanitized() {
        let op = test_op(AttachmentPhase::Pending);
        {
            let mut op = op.lock().unwrap();
            op.note_block(AttachmentBlock::InputNotReady);
            op.display_name = "日本語の名前".repeat(40);
        }
        operations().lock().unwrap().insert(41, op);
        let snapshot = attachment_snapshot(41).unwrap();
        assert_eq!(snapshot.phase, AttachmentPhase::Pending as u32);
        assert_eq!(snapshot.error_code_len as usize, "input_not_ready".len());
        assert_eq!(
            &snapshot.error_code[..snapshot.error_code_len as usize],
            b"input_not_ready"
        );
        assert!(usize::from(snapshot.display_name_len) <= ATTACHMENT_NAME_CAPACITY);
        operations().lock().unwrap().remove(&41);
    }

    // In-process SFTP server over a duplex stream (the reviewer's repro
    // harness): it exercises ensure_remote_dir / run_transfer / run_remove /
    // run_verify against real lstat/mkdir/setstat/close replies instead of
    // mocked outcomes, so path-security regressions surface end to end.
    use russh_sftp::protocol::{Attrs, Data, File as SftpFile, Handle, Name, Status};
    use std::fs::{self, File};
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    struct FixtureSftp {
        root: PathBuf,
        files: HashMap<String, File>,
        next_handle: u64,
        close_failure: bool,
    }

    impl FixtureSftp {
        fn new(root: PathBuf, close_failure: bool) -> Self {
            Self {
                root,
                files: HashMap::new(),
                next_handle: 0,
                close_failure,
            }
        }

        fn map(&self, path: &str) -> PathBuf {
            let clean: Vec<&str> = path
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .collect();
            self.root.join(clean.join("/"))
        }
    }

    fn sftp_status(id: u32, code: StatusCode) -> Status {
        Status {
            id,
            status_code: code,
            error_message: String::new(),
            language_tag: String::new(),
        }
    }

    fn sftp_io_error(error: &std::io::Error) -> StatusCode {
        match error.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
            std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
            _ => StatusCode::Failure,
        }
    }

    impl russh_sftp::server::Handler for FixtureSftp {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
            let resolved = if path == "." || path.is_empty() {
                "/home".to_owned()
            } else {
                path
            };
            Ok(Name {
                id,
                files: vec![SftpFile::dummy(resolved)],
            })
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            fs::symlink_metadata(self.map(&path))
                .map(|metadata| Attrs {
                    id,
                    attrs: (&metadata).into(),
                })
                .map_err(|error| sftp_io_error(&error))
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            fs::metadata(self.map(&path))
                .map(|metadata| Attrs {
                    id,
                    attrs: (&metadata).into(),
                })
                .map_err(|error| sftp_io_error(&error))
        }

        async fn mkdir(
            &mut self,
            id: u32,
            path: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let mapped = self.map(&path);
            fs::create_dir(&mapped).map_err(|error| sftp_io_error(&error))?;
            if let Some(mode) = attrs.permissions {
                let _ = fs::set_permissions(&mapped, fs::Permissions::from_mode(mode));
            }
            Ok(sftp_status(id, StatusCode::Ok))
        }

        async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
            fs::remove_dir(self.map(&path))
                .map(|()| sftp_status(id, StatusCode::Ok))
                .map_err(|error| sftp_io_error(&error))
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            pflags: OpenFlags,
            attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            let mapped = self.map(&filename);
            self.next_handle += 1;
            let token = format!("file-{}", self.next_handle);
            let mut options = fs::OpenOptions::new();
            options
                .read(pflags.contains(OpenFlags::READ))
                .write(pflags.contains(OpenFlags::WRITE))
                .append(pflags.contains(OpenFlags::APPEND));
            if pflags.contains(OpenFlags::EXCLUDE) {
                options.create_new(true);
            } else {
                options
                    .create(pflags.contains(OpenFlags::CREATE))
                    .truncate(pflags.contains(OpenFlags::TRUNCATE));
            }
            match options.open(&mapped) {
                Ok(file) => {
                    if let Some(mode) = attrs.permissions {
                        let _ = fs::set_permissions(&mapped, fs::Permissions::from_mode(mode));
                    }
                    self.files.insert(token.clone(), file);
                    Ok(Handle { id, handle: token })
                }
                Err(error) => Err(sftp_io_error(&error)),
            }
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            if self.files.remove(&handle).is_some() && !self.close_failure {
                Ok(sftp_status(id, StatusCode::Ok))
            } else {
                Err(StatusCode::Failure)
            }
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let Some(file) = self.files.get_mut(&handle) else {
                return Err(StatusCode::Failure);
            };
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(offset))
                .and_then(|_| file.write_all(&data))
                .map(|()| sftp_status(id, StatusCode::Ok))
                .map_err(|error| sftp_io_error(&error))
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let Some(file) = self.files.get_mut(&handle) else {
                return Err(StatusCode::Failure);
            };
            use std::io::Seek;
            let mut buffer = vec![0u8; len as usize];
            let read = file
                .seek(std::io::SeekFrom::Start(offset))
                .and_then(|_| file.read(&mut buffer))
                .map_err(|error| sftp_io_error(&error))?;
            buffer.truncate(read);
            Ok(Data { id, data: buffer })
        }

        async fn setstat(
            &mut self,
            id: u32,
            path: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            if let Some(mode) = attrs.permissions {
                fs::set_permissions(self.map(&path), fs::Permissions::from_mode(mode))
                    .map_err(|error| sftp_io_error(&error))?;
            }
            Ok(sftp_status(id, StatusCode::Ok))
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
            fs::remove_file(self.map(&filename))
                .map(|()| sftp_status(id, StatusCode::Ok))
                .map_err(|error| sftp_io_error(&error))
        }

        /// SFTP v3 rename never overwrites — the client relies on this to
        /// publish the staged partial without clobbering an existing file.
        async fn rename(
            &mut self,
            id: u32,
            oldpath: String,
            newpath: String,
        ) -> Result<Status, Self::Error> {
            let old_mapped = self.map(&oldpath);
            let new_mapped = self.map(&newpath);
            if new_mapped.exists() || new_mapped.symlink_metadata().is_ok() {
                return Err(StatusCode::Failure);
            }
            fs::rename(&old_mapped, &new_mapped)
                .map(|()| sftp_status(id, StatusCode::Ok))
                .map_err(|error| sftp_io_error(&error))
        }
    }

    async fn fixture_sftp(root: PathBuf, close_failure: bool) -> RawSftpSession {
        let (client, server) = tokio::io::duplex(65536);
        let handler = FixtureSftp::new(root, close_failure);
        tokio::spawn(russh_sftp::server::run(server, handler));
        let session = RawSftpSession::new(client);
        session.init().await.expect("fixture sftp init");
        session
    }

    fn fixture_root(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("meeterm-att-test-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).expect("create fixture root");
        path
    }

    /// FP-009: a pre-existing `meeterm/` with a permissive mode must be
    /// forced to 0700 while `.local`/`share` keep their modes untouched.
    #[tokio::test]
    async fn app_owned_dir_components_are_restricted() {
        let root = fixture_root("dir-mode");
        let local = root.join("home/.local");
        let share = local.join("share");
        let meeterm = share.join("meeterm");
        fs::create_dir_all(meeterm.join("attachments")).unwrap();
        fs::set_permissions(&local, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&share, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&meeterm, fs::Permissions::from_mode(0o755)).unwrap();
        let session = fixture_sftp(root.clone(), false).await;
        let components = path_components("/home/.local/share/meeterm/attachments");
        ensure_remote_dir(&session, &components, 2, true, APP_DIR_COMPONENTS, None)
            .await
            .expect("app-owned components accepted");
        let mode = |path: &PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&meeterm), 0o700, "meeterm/ must be restricted");
        assert_eq!(mode(&meeterm.join("attachments")), 0o700);
        assert_eq!(mode(&local), 0o755, ".local mode must not be touched");
        assert_eq!(mode(&share), 0o755, "share mode must not be touched");
        let _ = session.close_session();
        fs::remove_dir_all(&root).ok();
    }

    /// FP-009: a symlinked component anywhere in the app chain is refused.
    #[tokio::test]
    async fn app_owned_dir_rejects_symlink_component() {
        let root = fixture_root("dir-symlink");
        let share = root.join("home/.local/share");
        fs::create_dir_all(&share).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), share.join("meeterm")).unwrap();
        let session = fixture_sftp(root.clone(), false).await;
        let components = path_components("/home/.local/share/meeterm/attachments");
        let outcome =
            ensure_remote_dir(&session, &components, 2, true, APP_DIR_COMPONENTS, None).await;
        assert!(
            matches!(
                outcome,
                Err(TransferOutcome::Failed("remote_unsafe_path", _))
            ),
            "symlinked meeterm/ must fail remote_unsafe_path"
        );
        let _ = session.close_session();
        fs::remove_dir_all(&root).ok();
    }

    /// FP-010: deletion refuses a symlinked parent and never follows it
    /// into foreign files; a base that resolves to a different directory
    /// than the upload recorded is refused the same way.
    #[tokio::test]
    async fn delete_rejects_symlink_and_replaced_parent() {
        let root = fixture_root("delete-symlink");
        let foreign_dir = root.join("foreign");
        fs::create_dir_all(&foreign_dir).unwrap();
        std::os::unix::fs::symlink(&foreign_dir, root.join("chosen")).unwrap();
        let op = test_op(AttachmentPhase::Uploaded);
        {
            let mut op = op.lock().unwrap();
            op.spec.remote_dir = Some("/chosen".to_owned());
        }
        let foreign = foreign_dir.join(&op.lock().unwrap().spec.remote_name);
        fs::write(&foreign, b"FOREIGN FILE").unwrap();
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
        let session = fixture_sftp(root.clone(), false).await;
        let outcome = run_remove(&op, &session).await;
        assert!(
            matches!(outcome, TransferOutcome::Failed("remote_unsafe_path", _)),
            "symlink parent must fail remote_unsafe_path, got {outcome:?}"
        );
        assert!(foreign.exists(), "foreign file must not be deleted");
        // A base recorded at upload that no longer resolves identically is
        // refused even when the current path is a real directory.
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        {
            let mut op = op.lock().unwrap();
            op.spec.remote_dir = Some("/real".to_owned());
            op.remote_base = Some("/home/.local/share/meeterm/attachments".to_owned());
        }
        let outcome = run_remove(&op, &session).await;
        assert!(
            matches!(outcome, TransferOutcome::Failed("remote_unsafe_path", _)),
            "replaced base must fail remote_unsafe_path, got {outcome:?}"
        );
        let _ = session.close_session();
        fs::remove_dir_all(&root).ok();
    }

    /// FP-011: a failed CLOSE must not publish — the outcome is an error
    /// and the final generated name never appears remotely.
    #[tokio::test]
    async fn upload_requires_successful_close() {
        let root = fixture_root("close-failure");
        fs::create_dir_all(root.join("home")).unwrap();
        let local = root.join("image.png");
        fs::write(&local, b"1234567890").unwrap();
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.spec.local_path = local.to_str().unwrap().to_owned();
        }
        let session = fixture_sftp(root.clone(), true).await;
        let outcome = run_transfer(&op, &session, false, 1).await;
        assert!(
            !matches!(outcome, TransferOutcome::Uploaded(_)),
            "close failure must not report Uploaded, got {outcome:?}"
        );
        let published = root
            .join("home/.local/share/meeterm/attachments")
            .join(&op.lock().unwrap().spec.remote_name);
        assert!(!published.exists(), "final name must not be published");
        let _ = session.close_session();
        fs::remove_dir_all(&root).ok();
    }

    /// FP-007: a successful close publishes end to end, and a later
    /// re-verification lstat's the recorded path instead of re-uploading.
    #[tokio::test]
    async fn verify_confirms_recorded_file_and_detects_loss() {
        let root = fixture_root("verify");
        fs::create_dir_all(root.join("home")).unwrap();
        let local = root.join("image.png");
        fs::write(&local, b"1234567890").unwrap();
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.spec.local_path = local.to_str().unwrap().to_owned();
        }
        let session = fixture_sftp(root.clone(), false).await;
        let outcome = run_transfer(&op, &session, false, 1).await;
        let TransferOutcome::Uploaded(path) = outcome else {
            panic!("fixture upload must publish, got {outcome:?}");
        };
        {
            let mut op = op.lock().unwrap();
            op.phase = AttachmentPhase::Uploaded;
            op.remote_path = Some(path.clone());
        }
        assert!(
            matches!(run_verify(&op, &session).await, TransferOutcome::Verified),
            "intact remote file verifies"
        );
        fs::remove_file(root.join(path.trim_start_matches('/'))).unwrap();
        assert!(
            matches!(
                run_verify(&op, &session).await,
                TransferOutcome::RemoteMissing
            ),
            "vanished remote file must be remote_missing"
        );
        let _ = session.close_session();
        fs::remove_dir_all(&root).ok();
    }
}
