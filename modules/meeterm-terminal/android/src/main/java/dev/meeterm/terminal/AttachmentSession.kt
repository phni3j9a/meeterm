package dev.meeterm.terminal

/**
 * Issue #28 one-attachment session model.
 *
 * JS sees only narrow result maps; file paths stay in the staging/prepared
 * name fields, and [snapshot] returns display metadata plus the app-local
 * preview URI for the pending image.
 */
internal data class AttachmentTargetIdentity(
  val terminalId: String,
  val paneId: String,
  val workspaceId: String,
  val backend: String,
  val runtime: String,
  val host: String,
  val port: Int,
) {
  companion object {
    fun fromMap(raw: Map<String, Any?>?): AttachmentTargetIdentity? {
      if (raw == null) return null
      val terminalId = raw["terminalId"] as? String ?: return null
      val paneId = raw["paneId"] as? String ?: return null
      val workspaceId = raw["workspaceId"] as? String ?: return null
      val backend = raw["backend"] as? String ?: return null
      val runtime = raw["runtime"] as? String ?: return null
      val host = raw["host"] as? String ?: return null
      val port = (raw["port"] as? Number)?.toInt() ?: return null
      return AttachmentTargetIdentity(
        terminalId = terminalId,
        paneId = paneId,
        workspaceId = workspaceId,
        backend = backend,
        runtime = runtime,
        host = host,
        port = port,
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

internal enum class AttachmentSessionStatus { IDLE, STAGED, PREPARED, UPLOADED }

internal data class AttachmentSession(
  val target: AttachmentTargetIdentity,
  var stagingFileName: String? = null,
  var prepared: AttachmentPreparedImage? = null,
  var remotePath: String? = null,
  var lastErrorCode: String = "",
  var lastMessage: String = "",
) {
  val status: AttachmentSessionStatus
    get() = when {
      prepared != null && remotePath != null -> AttachmentSessionStatus.UPLOADED
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
    "remotePath" to (remotePath ?: ""),
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

  fun uploaded(remotePath: String): Map<String, Any?> = mapOf(
    "status" to "uploaded",
    "remotePath" to remotePath,
  )

  fun deleted(): Map<String, Any?> = mapOf("status" to "deleted")

  fun inserted(): Map<String, Any?> = mapOf("status" to "inserted")
}
