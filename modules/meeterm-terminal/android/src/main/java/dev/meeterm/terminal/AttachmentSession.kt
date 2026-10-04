package dev.meeterm.terminal

/**
 * One-tap attachment session. File names stay native and are not sent to JS.
 *
 * [AttachmentTargetIdentity] is pane-scoped identity for session binding and
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
  val byteCount: Long,
)

internal data class AttachmentSession(
  val target: AttachmentTargetIdentity,
  var stagingFileName: String? = null,
  var prepared: AttachmentPreparedImage? = null,
  /**
   * Live `meeterm_attachment_intent` id for the recorded pane terminal; 0
   * means no intent is held (core contract pending or the pane refused).
   * Released when a fresh `begin` replaces this session.
   */
  var intentId: Long = 0,
) {
  /** Live Rust-owned operation; its snapshot stays authoritative. */
  val machine = AttachmentOpMachine()
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

  fun picked(token: String): Map<String, Any?> = mapOf(
    "status" to "picked",
    "token" to token,
  )

  fun canceled(): Map<String, Any?> = mapOf("status" to "canceled")

  fun prepared(): Map<String, Any?> = mapOf("status" to "prepared")

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
