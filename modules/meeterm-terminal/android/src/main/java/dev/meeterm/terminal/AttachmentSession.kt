package dev.meeterm.terminal

/**
 * Issue #28 one-attachment session model.
 *
 * JS sees only narrow result maps; file paths stay in the staging/prepared
 * name fields, and [snapshot] returns display metadata plus the app-local
 * preview URI for the pending image.
 *
 * [AttachmentTargetIdentity] is pane-scoped identity for display and
 * session binding. The Rust core resolves the owning SSH endpoint, runtime,
 * and remote pane itself from the pane's native terminal id
 * (`meeterm_attachment_intent`), so the adapter never records or reuses an
 * SSH owner id.
 */
internal data class AttachmentTargetIdentity(
  val terminalId: String,
  val paneId: String,
  val workspaceId: String,
) {
  companion object {
    fun fromMap(raw: Map<String, Any?>?): AttachmentTargetIdentity? {
      if (raw == null) return null
      val terminalId = raw["terminalId"] as? String ?: return null
      val paneId = raw["paneId"] as? String ?: return null
      val workspaceId = raw["workspaceId"] as? String ?: return null
      return AttachmentTargetIdentity(
        terminalId = terminalId,
        paneId = paneId,
        workspaceId = workspaceId,
      )
    }
  }
}

internal data class AttachmentPreparedImage(
  val fileName: String,
  val format: AttachmentImageFormat,
  val width: Int,
  val height: Int,
  val byteCount: Long,
  val sourceByteCount: Long,
)

internal enum class AttachmentSessionStatus { IDLE, STAGED, PREPARED }

internal data class AttachmentSession(
  val target: AttachmentTargetIdentity,
  var stagingFileName: String? = null,
  var prepared: AttachmentPreparedImage? = null,
  var lastErrorCode: String = "",
  var lastMessage: String = "",
  /**
   * Live `meeterm_attachment_intent` id for the recorded pane terminal; 0
   * means no intent is held (core contract pending or the pane refused).
   * Kept for the process lifetime across sheet unmounts; released only by
   * Discard or a fresh `begin`.
   */
  var intentId: Long = 0,
) {
  /** Live Rust-owned operation; its snapshot stays authoritative. */
  val machine = AttachmentOpMachine()

  val status: AttachmentSessionStatus
    get() = when {
      prepared != null -> AttachmentSessionStatus.PREPARED
      stagingFileName != null -> AttachmentSessionStatus.STAGED
      else -> AttachmentSessionStatus.IDLE
    }

  fun recordError(errorCode: String, message: String) {
    lastErrorCode = errorCode
    lastMessage = message
  }

  fun clearError() {
    lastErrorCode = ""
    lastMessage = ""
  }

  fun snapshot(previewUri: String): Map<String, Any?> = mapOf(
    "status" to status.name.lowercase(),
    "fileId" to (prepared?.fileName ?: ""),
    "previewUri" to if (prepared != null) previewUri else "",
    "format" to (prepared?.format?.name?.lowercase() ?: ""),
    "width" to (prepared?.width ?: 0),
    "height" to (prepared?.height ?: 0),
    "byteCount" to (prepared?.byteCount ?: 0L),
    "sourceByteCount" to (prepared?.sourceByteCount ?: 0L),
    "target" to mapOf(
      "terminalId" to target.terminalId,
      "paneId" to target.paneId,
      "workspaceId" to target.workspaceId,
    ),
    "operation" to machine.operation?.let(AttachmentResults::operation),
    "errorCode" to lastErrorCode,
    "message" to lastMessage,
  )
}

/** Result-map builders matching Attachment*.types.ts one-for-one. */
internal object AttachmentResults {
  fun held(reason: String): Map<String, Any?> = mapOf(
    "status" to "held",
    "reason" to reason,
  )

  fun unavailable(reason: String): Map<String, Any?> = mapOf(
    "status" to "unavailable",
    "reason" to reason,
  )

  fun error(errorCode: String, message: String): Map<String, Any?> = mapOf(
    "status" to "error",
    "errorCode" to errorCode,
    "message" to message,
  )

  fun picked(token: String, byteCount: Long): Map<String, Any?> = mapOf(
    "status" to "picked",
    "token" to token,
    "byteCount" to byteCount,
  )

  fun canceled(): Map<String, Any?> = mapOf("status" to "canceled")

  fun prepared(
    image: AttachmentPreparedImage,
    previewUri: String,
  ): Map<String, Any?> = mapOf(
    "status" to "prepared",
    "fileId" to image.fileName,
    "previewUri" to previewUri,
    "format" to image.format.name.lowercase(),
    "width" to image.width,
    "height" to image.height,
    "byteCount" to image.byteCount,
    "sourceByteCount" to image.sourceByteCount,
  )

  /** Uniform accepted answer carrying the decimal u64 attachment id. */
  fun accepted(attachmentId: Long): Map<String, Any?> = mapOf(
    "status" to "accepted",
    "attachmentId" to java.lang.Long.toUnsignedString(attachmentId),
  )

  /** Core snapshot fields, mirroring AttachmentOperationSnapshot (TS). */
  fun operation(op: AttachmentOperation): Map<String, Any?> = mapOf(
    "phase" to op.wirePhase,
    "attachmentId" to java.lang.Long.toUnsignedString(op.attachmentId),
    "bytesUploaded" to op.bytesUploaded.toDouble(),
    "sizeBytes" to op.sizeBytes.toDouble(),
    "remotePath" to op.remotePath,
    "displayName" to op.displayName,
    "errorCode" to op.errorCode,
    "errorMessage" to op.errorMessage,
    "insertUnconfirmed" to op.insertUnconfirmed,
    "remoteRemoved" to op.remoteRemoved,
    "jobInFlight" to op.jobInFlight,
  )

  fun snapshotResult(op: AttachmentOperation?): Map<String, Any?> =
    if (op == null) {
      mapOf("status" to "idle")
    } else {
      mapOf("status" to "snapshot", "operation" to operation(op))
    }
}
