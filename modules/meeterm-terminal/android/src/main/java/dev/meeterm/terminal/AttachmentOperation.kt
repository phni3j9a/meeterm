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
  CANCELLED(5);

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

  /** flags & 0x2: the meeterm-created remote file was explicitly deleted. */
  val remoteRemoved: Boolean get() = flags and 0x2 != 0

  /**
   * flags & 0x4: a job (upload / verify+insert / remove) is in flight on
   * this operation. The core clears the previous reason at job start and
   * drops the flag when that attempt's outcome lands — UI busy/pending
   * display and polling key off this bit, never off a stale errorCode.
   */
  val jobInFlight: Boolean get() = flags and 0x4 != 0

  val wirePhase: String get() = when (phase) {
    AttachmentOpPhase.PENDING -> "pending"
    AttachmentOpPhase.UPLOADING -> "uploading"
    AttachmentOpPhase.UPLOADED -> "uploaded"
    AttachmentOpPhase.INSERTED -> "inserted"
    AttachmentOpPhase.FAILED -> "failed"
    AttachmentOpPhase.CANCELLED -> "cancelled"
  }

  /**
   * Explicit Retry upload: pending/failed ops, plus an uploaded op whose
   * remote file was deleted — the core falls that case through to a
   * re-upload of the same operation. Refused while a job is in flight.
   */
  val canRetryUpload: Boolean
    get() = !jobInFlight &&
      (phase == AttachmentOpPhase.PENDING ||
        phase == AttachmentOpPhase.FAILED ||
        (phase == AttachmentOpPhase.UPLOADED && remoteRemoved))

  /**
   * Insert is one verified job: intent check → fresh fence → remote
   * lstat → single-line paste. It is allowed on an uploaded op only —
   * an inserted op is never re-inserted (the path stays in the terminal
   * input for the user to review) — and never while another job runs.
   */
  val canInsert: Boolean
    get() = !remoteRemoved && !jobInFlight && phase == AttachmentOpPhase.UPLOADED

  /** Cancel applies while transfer work may still be running. */
  val canCancel: Boolean
    get() = phase == AttachmentOpPhase.PENDING || phase == AttachmentOpPhase.UPLOADING

  /**
   * Remote delete covers every phase that may still own a file; refused
   * while a job is in flight (one in-flight job per operation).
   */
  val canDeleteRemote: Boolean
    get() = !remoteRemoved && !jobInFlight &&
      (phase == AttachmentOpPhase.UPLOADED ||
        phase == AttachmentOpPhase.INSERTED ||
        phase == AttachmentOpPhase.FAILED ||
        phase == AttachmentOpPhase.CANCELLED)
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

/**
 * Decoder for the JNI `attachmentSnapshot` flat string array:
 * [phase, flags, attachmentId, bytesUploaded, sizeBytes, remotePath,
 *  displayName, errorCode, errorMessage]. An empty/short array or an
 * unknown phase returns null.
 */
internal object AttachmentOperationCodec {
  const val FIELD_COUNT = 9

  fun decode(fields: Array<String>): AttachmentOperation? {
    if (fields.size < FIELD_COUNT) return null
    val phaseValue = fields[0].toIntOrNull() ?: return null
    val phase = AttachmentOpPhase.fromWire(phaseValue) ?: return null
    return AttachmentOperation(
      attachmentId = fields[2].toLongOrNull() ?: return null,
      phase = phase,
      flags = fields[1].toIntOrNull() ?: 0,
      bytesUploaded = fields[3].toLongOrNull() ?: 0L,
      sizeBytes = fields[4].toLongOrNull() ?: 0L,
      remotePath = fields[5],
      displayName = fields[6],
      errorCode = fields[7],
      errorMessage = fields[8],
    )
  }
}
