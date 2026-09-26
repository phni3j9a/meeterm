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
            Self::Internal => "native attachment state is unavailable",
        }
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
    owner: u64,
    fence: DestinationFence,
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
    bytes_uploaded: u64,
    cancel_requested: bool,
    insert_enqueued: bool,
    /// The uploaded remote file was explicitly deleted.
    removed: bool,
    /// A `SftpRemove` request owns this op right now; guards against
    /// duplicate remove enqueues.
    remove_in_flight: bool,
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
static NEXT_ATTACHMENT_ID: AtomicU64 = AtomicU64::new(1);

fn operations() -> &'static Mutex<HashMap<u64, Arc<Mutex<AttachmentOperation>>>> {
    ATTACHMENTS.get_or_init(|| Mutex::new(HashMap::new()))
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

/// Mark one in-flight op pending after its actor-side request was dropped
/// (stale epoch, not-ready gate, or a dead command channel). A cancelled op
/// keeps its cancelled outcome.
pub(crate) fn mark_pending(id: u64, block: AttachmentBlock) {
    if let Some(op) = operation(id)
        && let Ok(mut op) = op.lock()
    {
        op.pend(block);
    }
}

/// The command loop dropped a `SftpRemove` request before executing it.
/// The op keeps its phase; the in-flight marker clears so an explicit
/// remove retry can enqueue again.
pub(crate) fn mark_remove_expired(id: u64, block: AttachmentBlock) {
    if let Some(op) = operation(id)
        && let Ok(mut op) = op.lock()
    {
        op.remove_in_flight = false;
        op.note_block(block);
    }
}

/// Actor gate for `ControlCommand::SftpUpload`: the op must still belong to
/// this owner and this connection generation and still be awaiting launch.
/// Returns `true` only when the SFTP channel may be opened for it.
pub(crate) fn gate_launch(
    op: &Arc<Mutex<AttachmentOperation>>,
    owner: u64,
    generation: u64,
) -> bool {
    let Ok(mut op) = op.lock() else {
        return false;
    };
    if op.cancel_requested {
        op.phase = AttachmentPhase::Cancelled;
        return false;
    }
    if op.phase != AttachmentPhase::Uploading {
        return false;
    }
    if op.owner != owner || op.fence.generation != generation {
        op.phase = AttachmentPhase::Pending;
        op.note_block(AttachmentBlock::StaleOperation);
        return false;
    }
    true
}

/// Actor gate for `ControlCommand::SftpRemove`: same owner/generation check
/// as upload, plus the remove request must still be the one in flight.
/// A stale request clears its in-flight marker instead of running.
pub(crate) fn gate_remove(
    op: &Arc<Mutex<AttachmentOperation>>,
    owner: u64,
    generation: u64,
) -> bool {
    let Ok(mut op) = op.lock() else {
        return false;
    };
    if !op.remove_in_flight {
        return false;
    }
    if op.owner != owner || op.fence.generation != generation {
        op.remove_in_flight = false;
        op.note_block(AttachmentBlock::StaleOperation);
        return false;
    }
    true
}

/// The actor aborted while preparing this op's SFTP channel. The op may
/// still be retried on a later connection attempt.
pub(crate) fn launch_pending(op: &Arc<Mutex<AttachmentOperation>>, block: AttachmentBlock) {
    if let Ok(mut op) = op.lock() {
        op.pend(block);
    }
}

/// The actor-side channel/subsystem setup failed hard (SFTP unavailable).
pub(crate) fn launch_failed(
    op: &Arc<Mutex<AttachmentOperation>>,
    code: &'static str,
    message: impl Into<String>,
) {
    if let Ok(mut op) = op.lock() {
        op.fail(code, message);
    }
}

/// A remote-delete job stalled before its SFTP session existed. The file
/// state is unchanged; the in-flight marker clears so an explicit retry
/// can enqueue, and the reason stays visible without touching the phase.
pub(crate) fn remove_stalled(op: &Arc<Mutex<AttachmentOperation>>, block: AttachmentBlock) {
    if let Ok(mut op) = op.lock() {
        op.remove_in_flight = false;
        op.note_block(block);
    }
}

/// The SFTP subsystem was rejected outright for a remote-delete job.
pub(crate) fn remove_failed(
    op: &Arc<Mutex<AttachmentOperation>>,
    code: &'static str,
    message: impl Into<String>,
) {
    if let Ok(mut op) = op.lock() {
        op.remove_in_flight = false;
        op.error_code = Some(code);
        op.error_message = Some(message.into());
    }
}

/// The connection actor for `generation` ended (flow failure or shutdown)
/// and silently discarded any queued upload request. Mark surviving
/// still-`Uploading` ops for that generation pending so they are retryable
/// on the next actor instead of displaying a dead progress state.
pub(crate) fn generation_finished(owner: u64, generation: u64) {
    let Ok(operations) = operations().lock() else {
        return;
    };
    for op in operations.values() {
        let Ok(mut op) = op.lock() else {
            continue;
        };
        if op.owner == owner && op.fence.generation == generation {
            if op.remove_in_flight {
                op.remove_in_flight = false;
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
                    op.owner == owner
                        && !matches!(
                            op.phase,
                            AttachmentPhase::Failed | AttachmentPhase::Cancelled
                        ),
                )
            }) == 1
        })
        .count()
}

/// Begin one attachment: validate the picked file, capture the destination
/// fence, register the operation, and enqueue its SFTP upload on the
/// current connection actor. The returned id identifies the operation for
/// polling, insert, retry, cancel, remove, and dispose.
///
/// `remote_dir` is the user's explicitly chosen remote directory (clean
/// absolute path), or `None` for the app-private default
/// `<sftp-start>/.local/share/meeterm/attachments`.
pub fn attachment_begin(
    terminal_id: u64,
    local_path: &str,
    display_name: &str,
    size_bytes: u64,
    remote_dir: Option<&str>,
) -> Result<u64, AttachmentError> {
    validate_begin_args(local_path, display_name, size_bytes)?;
    let remote_dir = remote_dir.map(validate_remote_dir).transpose()?;
    registry::shared_terminal(terminal_id).map_err(|_| AttachmentError::UnknownTerminal)?;
    let shared = crate::ssh::current_connection(terminal_id)
        .map_err(|_| AttachmentError::DestinationNotReady)?;
    let fence = shared
        .attachment_capture_fence()
        .map_err(|_| AttachmentError::DestinationNotReady)?;

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
        owner: terminal_id,
        fence,
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
        bytes_uploaded: 0,
        cancel_requested: false,
        insert_enqueued: false,
        removed: false,
        remove_in_flight: false,
    }));
    {
        let mut operations = operations().lock().map_err(|_| AttachmentError::Internal)?;
        if live_op_count(&operations, terminal_id) >= MAX_LIVE_OPS_PER_OWNER {
            return Err(AttachmentError::Busy);
        }
        operations.insert(id, Arc::clone(&op));
    }
    if let Err(block) = shared
        .attachment_enqueue_command(crate::ssh::ControlCommand::SftpUpload { attachment_id: id })
    {
        mark_pending(id, block);
    }
    Ok(id)
}

/// Explicit transfer retry. The same destination pane identity is
/// re-fenced against the *current* connection state: the pane must still
/// exist, the Herdr stable terminal identity must still match, and the
/// endpoint must still be the one the operation captured. A still-valid
/// uploaded file on the same endpoint short-circuits through the transfer
/// task's reuse check instead of re-sending bytes.
pub fn attachment_retry_upload(
    terminal_id: u64,
    attachment_id: u64,
) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let (owner, fence) = {
        let op = lock_operation(&op)?;
        if op.owner != terminal_id {
            return Err(AttachmentError::InvalidArgument);
        }
        match op.phase {
            // Still verified remotely: nothing to do. An explicitly removed
            // file falls through and re-uploads instead.
            AttachmentPhase::Uploaded if op.remote_path.is_some() && !op.removed => {
                return Ok(());
            }
            AttachmentPhase::Uploaded | AttachmentPhase::Pending | AttachmentPhase::Failed => {}
            AttachmentPhase::Uploading | AttachmentPhase::Inserted | AttachmentPhase::Cancelled => {
                return Err(AttachmentError::InvalidState);
            }
        }
        (op.owner, op.fence.clone())
    };
    let shared = crate::ssh::current_connection(owner).map_err(|error| {
        if let Ok(mut op) = op.lock() {
            op.note_block(AttachmentBlock::StaleConnection);
        }
        let _ = error;
        AttachmentError::DestinationNotReady
    })?;
    let fence = match shared.attachment_recheck_fence(&fence) {
        Ok(fence) => fence,
        Err(block) => {
            if let Ok(mut op) = op.lock() {
                // A Failed op that cannot re-fence stays Failed but shows the
                // current reason; a Pending op keeps its phase either way.
                op.note_block(block);
            }
            return Err(AttachmentError::DestinationNotReady);
        }
    };
    {
        let mut op = lock_operation(&op)?;
        if op.cancel_requested {
            return Err(AttachmentError::InvalidState);
        }
        match op.phase {
            AttachmentPhase::Uploaded if op.remote_path.is_some() && !op.removed => {
                return Ok(());
            }
            AttachmentPhase::Uploaded | AttachmentPhase::Pending | AttachmentPhase::Failed => {}
            _ => return Err(AttachmentError::InvalidState),
        }
        op.fence = fence;
        op.phase = AttachmentPhase::Uploading;
        op.bytes_uploaded = 0;
        op.remote_path = None;
        op.removed = false;
        op.clear_status();
        op.cancel_requested = false;
    }
    if let Err(block) =
        shared.attachment_enqueue_command(crate::ssh::ControlCommand::SftpUpload { attachment_id })
    {
        mark_pending(attachment_id, block);
    }
    Ok(())
}

/// Explicit insert of an uploaded file: exactly one quoted remote path
/// line into the still-current fenced pane through `paste_utf8_at_epoch`.
/// Enter is never sent. A destination switch/replacement/disappearance,
/// transport/controller generation mismatch, or Herdr read-only state
/// records a pending reason and leaves the operation `Uploaded`.
pub fn attachment_insert(terminal_id: u64, attachment_id: u64) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let (owner, fence, remote_path) = {
        let op = lock_operation(&op)?;
        if op.owner != terminal_id {
            return Err(AttachmentError::InvalidArgument);
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
        (op.owner, op.fence.clone(), remote_path)
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
    terminal_id: u64,
    attachment_id: u64,
) -> Result<(), AttachmentError> {
    let op = operation(attachment_id).ok_or(AttachmentError::UnknownAttachment)?;
    let (owner, fence) = {
        let op = lock_operation(&op)?;
        if op.owner != terminal_id {
            return Err(AttachmentError::InvalidArgument);
        }
        if op.removed {
            return Ok(());
        }
        if op.remove_in_flight {
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
        (op.owner, op.fence.clone())
    };
    let shared = crate::ssh::current_connection(owner).map_err(|_| {
        if let Ok(mut op) = op.lock() {
            op.note_block(AttachmentBlock::StaleConnection);
        }
        AttachmentError::DestinationNotReady
    })?;
    if let Err(block) = shared.attachment_recheck_endpoint(&fence.endpoint) {
        if let Ok(mut op) = op.lock() {
            op.note_block(block);
        }
        return Err(AttachmentError::DestinationNotReady);
    }
    {
        let mut op = lock_operation(&op)?;
        if op.removed {
            return Ok(());
        }
        op.remove_in_flight = true;
    }
    if let Err(block) =
        shared.attachment_enqueue_command(crate::ssh::ControlCommand::SftpRemove { attachment_id })
    {
        if let Ok(mut op) = op.lock() {
            op.remove_in_flight = false;
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
    /// Transient/structural blockage; the op stays retryable.
    Pending(AttachmentBlock),
    /// Terminal failure.
    Failed(&'static str, String),
    /// Cancel flag observed; partial was cleaned best-effort.
    Cancelled,
}

/// Spawn the detached transfer over an already-initialized SFTP stream.
/// Called by the connection actor after the subsystem handshake; this task
/// never blocks the interactive command loop.
pub(crate) fn start_transfer<S>(op: Arc<Mutex<AttachmentOperation>>, stream: S)
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
                    run_transfer(&op, &session, statvfs).await
                }
                Err(error) => map_transfer_error(error, "sftp_unavailable"),
            }
        };
        let outcome =
            match tokio::time::timeout(Duration::from_secs(TRANSFER_TIMEOUT_SECS), outcome).await {
                Ok(outcome) => outcome,
                Err(_) => TransferOutcome::Pending(AttachmentBlock::Timeout),
            };
        apply_outcome(&op, outcome);
        let _ = session.close_session();
    });
}

/// Spawn the detached remote-delete over an already-initialized SFTP
/// stream. Removes only the generated names this operation owns.
pub(crate) fn start_remove<S>(op: Arc<Mutex<AttachmentOperation>>, stream: S)
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
        apply_outcome(&op, outcome);
        let _ = session.close_session();
    });
}

fn apply_outcome(op: &Arc<Mutex<AttachmentOperation>>, outcome: TransferOutcome) {
    let Ok(mut op) = op.lock() else {
        return;
    };
    let was_remove = op.remove_in_flight;
    op.remove_in_flight = false;
    if was_remove {
        // Remote-delete outcome: verified deletion is recorded even when
        // the user cancelled mid-flight, but the phase itself is untouched
        // — `inserted` is not revoked and failure keeps the uploaded state.
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
            TransferOutcome::Uploaded(_) | TransferOutcome::Cancelled => {
                op.note_block(AttachmentBlock::Internal);
            }
        }
        return;
    }
    if matches!(outcome, TransferOutcome::Removed) {
        // A remove can never land on an upload op — ignore a stray report.
        return;
    }
    if op.cancel_requested {
        // A delayed completion must not resurrect a cancelled operation or
        // change its visible state.
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
        TransferOutcome::Removed => {}
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
    }
}

fn op_cancelled(op: &Arc<Mutex<AttachmentOperation>>) -> bool {
    op.lock().map(|op| op.cancel_requested).unwrap_or(true)
}

fn op_progress(op: &Arc<Mutex<AttachmentOperation>>, bytes: u64) {
    if let Ok(mut op) = op.lock() {
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
/// components are app-owned and get forced to `0700` when they deviate;
/// an explicit directory's modes are never changed. The leaf is
/// uid-checked against the home owner.
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
        let attrs = match session.lstat(&path).await {
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
        if !last {
            continue;
        }
        if expected_uid.is_some() && attrs.uid.is_some() && attrs.uid != expected_uid {
            return Err(TransferOutcome::Failed(
                "remote_unsafe_path",
                "attachment directory is owned by another user".to_owned(),
            ));
        }
        if app_owned && attrs.permissions.is_none_or(|mode| mode & 0o077 != 0) {
            session
                .setstat(&path, mode_only(REMOTE_DIR_MODE))
                .await
                .map_err(|error| map_transfer_error(error, "sftp_error"))?;
            let attrs = session
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
            op_progress(op, offset);
        }
        if offset != spec.size_bytes {
            return TransferOutcome::Failed(
                "source_unreadable",
                "the selected image shrank during upload".to_owned(),
            );
        }
        let _ = session.close(handle.clone()).await;
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

/// Resolve the base directory exactly as the upload did — including `~/`
/// expansion against `realpath(".")` — without failing when it is already
/// gone: deletion must be idempotent.
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
/// app-private default, the (empty) attachments directory itself. Every
/// step is idempotent so a repeated remove or a partially cleaned state
/// still converges to `Removed`.
async fn run_remove(
    op: &Arc<Mutex<AttachmentOperation>>,
    session: &RawSftpSession,
) -> TransferOutcome {
    let spec = {
        let Ok(op) = op.lock() else {
            return TransferOutcome::Cancelled;
        };
        op.spec.clone()
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
            owner: 9,
            fence: fence(12, 90),
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
            bytes_uploaded: 0,
            cancel_requested: false,
            insert_enqueued: false,
            removed: false,
            remove_in_flight: false,
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
        apply_outcome(&op, TransferOutcome::Uploaded("/remote/x".to_owned()));
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
        apply_outcome(&op, TransferOutcome::Uploaded("/remote/final".to_owned()));
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Uploaded);
        assert_eq!(op.remote_path.as_deref(), Some("/remote/final"));
        assert!(op.reason_code.is_none());
    }

    #[test]
    fn gate_launch_rejects_wrong_generation() {
        let op = test_op(AttachmentPhase::Uploading);
        assert!(!gate_launch(&op, 9, 8));
        let op = op.lock().unwrap();
        assert_eq!(op.phase, AttachmentPhase::Pending);
        assert_eq!(op.reason_code, Some("stale_operation"));
    }

    #[test]
    fn gate_launch_rejects_cancelled() {
        let op = test_op(AttachmentPhase::Uploading);
        {
            let mut op = op.lock().unwrap();
            op.cancel();
        }
        assert!(!gate_launch(&op, 9, 7));
        assert_eq!(op.lock().unwrap().phase, AttachmentPhase::Cancelled);
    }

    #[test]
    fn gate_launch_accepts_current() {
        let op = test_op(AttachmentPhase::Uploading);
        assert!(gate_launch(&op, 9, 7));
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
}
