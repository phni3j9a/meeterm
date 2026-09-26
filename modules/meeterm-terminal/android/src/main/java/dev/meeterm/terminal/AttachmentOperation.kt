package dev.meeterm.terminal

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets

/**
 * Rust-owned attachment operation state (attachment-ffi contract).
 *
 * The core operation lifecycle is authoritative; this model only decodes the
 * fixed-size snapshot record and pins the adapter-side transition guards:
 * a second upload is refused while an op is live, snapshots from a stale
 * attachment id are dropped, and insert/delete capabilities follow the phase.
 */
internal enum class AttachmentOpPhase(val wireValue: Int) {
  PENDING(0),
  UPLOADING(1),
  UPLOADED(2),
  INSERTED(3),
  FAILED(4),
  CANCELLED(5),
  DELETED(6);

  companion object {
    fun fromWire(value: Int): AttachmentOpPhase? =
      values().firstOrNull { it.wireValue == value }
  }
}

internal data class AttachmentOperation(
  val attachmentId: Long,
  val phase: AttachmentOpPhase,
  val flags: Int,
  val bytesUploaded: Long,
  val sizeBytes: Long,
  val remotePath: String,
  val displayName: String,
  val errorCode: String,
  val errorMessage: String,
) {
  /** flags & 0x1: the input queue accepted the line; delivery unconfirmed. */
  val insertUnconfirmed: Boolean get() = flags and 0x1 != 0

  val wirePhase: String get() = when (phase) {
    AttachmentOpPhase.PENDING -> "pending"
    AttachmentOpPhase.UPLOADING -> "uploading"
    AttachmentOpPhase.UPLOADED -> "uploaded"
    AttachmentOpPhase.INSERTED -> "inserted"
    AttachmentOpPhase.FAILED -> "failed"
    AttachmentOpPhase.CANCELLED -> "cancelled"
    AttachmentOpPhase.DELETED -> "deleted"
  }

  /** Explicit Retry upload is offered only for pending/failed ops. */
  val canRetryUpload: Boolean
    get() = phase == AttachmentOpPhase.PENDING || phase == AttachmentOpPhase.FAILED

  /** Insert is allowed while uploaded; an inserted op accepts it idempotently. */
  val canInsert: Boolean
    get() = phase == AttachmentOpPhase.UPLOADED || phase == AttachmentOpPhase.INSERTED

  /** Cancel applies while transfer work may still be running. */
  val canCancel: Boolean
    get() = phase == AttachmentOpPhase.PENDING || phase == AttachmentOpPhase.UPLOADING

  /** M4 remote delete covers every phase that may own a completed file. */
  val canDeleteRemote: Boolean
    get() = phase == AttachmentOpPhase.UPLOADED ||
      phase == AttachmentOpPhase.INSERTED ||
      phase == AttachmentOpPhase.FAILED ||
      phase == AttachmentOpPhase.CANCELLED

  val isTerminal: Boolean
    get() = phase == AttachmentOpPhase.INSERTED ||
      phase == AttachmentOpPhase.DELETED
}

/**
 * Tracks the live core operation. The snapshot is authoritative, but adapter
 * guards keep a stale attachment id or a second upload from corrupting state.
 */
internal class AttachmentOpMachine {
  var operation: AttachmentOperation? = null
    private set

  /** A new upload may only replace a fully terminal (or absent) operation. */
  fun canBeginUpload(): Boolean {
    val op = operation ?: return true
    return op.phase == AttachmentOpPhase.CANCELLED ||
      op.phase == AttachmentOpPhase.DELETED ||
      op.phase == AttachmentOpPhase.FAILED
  }

  /** Record the attachment id accepted by the core; false means refused. */
  fun recordBegin(attachmentId: Long, sizeBytes: Long, displayName: String): Boolean {
    if (attachmentId <= 0 || !canBeginUpload()) return false
    operation = AttachmentOperation(
      attachmentId = attachmentId,
      phase = AttachmentOpPhase.UPLOADING,
      flags = 0,
      bytesUploaded = 0,
      sizeBytes = sizeBytes,
      remotePath = "",
      displayName = displayName,
      errorCode = "",
      errorMessage = "",
    )
    return true
  }

  /** Apply the authoritative core snapshot; stale ids are dropped. */
  fun applySnapshot(snapshot: AttachmentOperation): Boolean {
    val current = operation ?: return false
    if (current.attachmentId != snapshot.attachmentId) return false
    // A locally-cancelled op keeps its phase until the core confirms the
    // cancel (or finishes); late upload bytes never resurrect it.
    if (current.phase == AttachmentOpPhase.CANCELLED &&
      snapshot.phase == AttachmentOpPhase.UPLOADING
    ) {
      return true
    }
    operation = snapshot
    return true
  }

  /** Local cancel mark; the core drops delayed completions itself. */
  fun markCancelled() {
    operation = operation?.copy(phase = AttachmentOpPhase.CANCELLED)
  }

  fun clear() {
    operation = null
  }
}

/** Fixed-offset decoder for `meeterm_attachment_snapshot_t` (1000 bytes). */
internal object AttachmentOperationCodec {
  private const val REMOTE_PATH_LEN = 32
  private const val REMOTE_PATH = 34
  private const val DISPLAY_NAME_LEN = 546
  private const val DISPLAY_NAME = 548
  private const val ERROR_CODE_LEN = 676
  private const val ERROR_CODE = 678
  private const val ERROR_MESSAGE_LEN = 742
  private const val ERROR_MESSAGE = 744
  const val RECORD_SIZE = 1000

  fun decode(bytes: ByteArray): AttachmentOperation? {
    if (bytes.size < RECORD_SIZE) return null
    val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
    val phaseValue = buffer.getInt(0)
    val phase = AttachmentOpPhase.fromWire(phaseValue) ?: return null
    val flags = buffer.getInt(4)
    val attachmentId = buffer.getLong(8)
    val bytesUploaded = buffer.getLong(16)
    val sizeBytes = buffer.getLong(24)
    return AttachmentOperation(
      attachmentId = attachmentId,
      phase = phase,
      flags = flags,
      bytesUploaded = bytesUploaded,
      sizeBytes = sizeBytes,
      remotePath = field(buffer, bytes, REMOTE_PATH_LEN, REMOTE_PATH, 512),
      displayName = field(buffer, bytes, DISPLAY_NAME_LEN, DISPLAY_NAME, 128),
      errorCode = field(buffer, bytes, ERROR_CODE_LEN, ERROR_CODE, 64),
      errorMessage = field(buffer, bytes, ERROR_MESSAGE_LEN, ERROR_MESSAGE, 256),
    )
  }

  private fun field(
    buffer: ByteBuffer,
    bytes: ByteArray,
    lengthOffset: Int,
    dataOffset: Int,
    capacity: Int,
  ): String {
    val length = (buffer.getShort(lengthOffset).toInt() and 0xFFFF).coerceAtMost(capacity)
    val end = dataOffset + length
    if (end > bytes.size) return ""
    return String(bytes, dataOffset, length, StandardCharsets.UTF_8)
  }
}
