package dev.meeterm.terminal

import android.content.Context
import expo.modules.kotlin.Promise

/**
 * Process-wide attachment session owner for Issue #28.
 *
 * A single pending attachment is allowed at a time; `begin` replaces any
 * previous one after deleting its files. The session files live under
 * `cacheDir/attachments/` and are reclaimed on startup when no session
 * references them.
 */
internal object AttachmentController {
  private val lock = Any()
  private var store: AttachmentStore? = null
  private var session: AttachmentSession? = null
  private var reclaimed = false

  fun store(context: Context): AttachmentStore {
    synchronized(lock) {
      val existing = store
      if (existing != null) return existing
      val created = AttachmentStore(context.applicationContext)
      store = created
      return created
    }
  }

  fun picker(appContext: expo.modules.kotlin.AppContext, context: Context): AttachmentPicker =
    AttachmentPicker(appContext, store(context))

  /** Startup reclaim; safe to call repeatedly. */
  fun reclaimStaleFiles(context: Context) {
    synchronized(lock) {
      if (reclaimed) return
      reclaimed = true
      val kept = session?.let {
        setOfNotNull(it.stagingFileName, it.prepared?.fileName)
      } ?: emptySet()
      store(context).reclaimStale(kept)
    }
  }

  fun begin(
    terminalId: String,
    target: AttachmentTargetIdentity,
    context: Context,
  ): Map<String, Any?> {
    if (AttachmentCompositionGuard.isComposing(terminalId)) {
      return AttachmentResults.held(AttachmentLimits.REASON_COMPOSING)
    }
    synchronized(lock) {
      val previous = session
      session = AttachmentSession(target)
      previous?.let {
        store(context).delete(it.stagingFileName, it.prepared?.fileName)
      }
    }
    return mapOf("status" to "ready")
  }

  fun onPicked(token: String, byteCount: Long, context: Context) {
    synchronized(lock) {
      val active = session ?: return
      if (!AttachmentFileNames.isStagingName(token)) return
      // The picker stages before this runs; a fresh pick invalidates any
      // older staging or prepared files from the same session.
      val stale = active.stagingFileName
      val stalePrepared = active.prepared?.fileName
      active.stagingFileName = token
      active.prepared = null
      active.remotePath = null
      active.clearError()
      store(context).delete(
        stale?.takeIf { it != token },
        stalePrepared?.takeIf { it != token },
      )
    }
  }

  fun prepare(token: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
      ?: return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "No attachment session is active.",
      )
    val staging = active.stagingFileName
    if (staging == null || staging != token) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_MISSING,
        "The picked image is no longer available.",
      )
    }
    when (val result = AttachmentNormalize(store(context)).normalize(staging)) {
      is AttachmentNormalize.Result.Ok -> {
        synchronized(lock) {
          active.prepared = result.image
          active.clearError()
        }
        // Staging is no longer needed once the normalized file exists; the
        // session keeps only the previewable prepared file.
        store(context).delete(staging)
        synchronized(lock) { active.stagingFileName = null }
        return AttachmentResults.prepared(
          result.image,
          store(context).previewUri(result.image.fileName),
        )
      }
      is AttachmentNormalize.Result.Rejected -> {
        synchronized(lock) {
          active.recordError(result.errorCode, result.message)
        }
        return AttachmentResults.error(result.errorCode, result.message)
      }
    }
  }

  fun discard(context: Context) {
    val active = synchronized(lock) {
      val current = session
      session = null
      current
    } ?: return
    store(context).delete(active.stagingFileName, active.prepared?.fileName)
  }

  fun snapshot(context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session } ?: return emptySessionSnapshot()
    val preview = active.prepared?.fileName?.let { store(context).previewUri(it) } ?: ""
    return synchronized(lock) { active.snapshot(preview) }
  }

  fun pick(source: String, promise: Promise, appContext: expo.modules.kotlin.AppContext, context: Context) {
    val parsed = when (source) {
      "photos" -> AttachmentPicker.Source.PHOTOS
      "files" -> AttachmentPicker.Source.FILES
      else -> {
        promise.resolve(
          AttachmentResults.error(
            AttachmentLimits.ERROR_ARGUMENT,
            "Unknown attachment source.",
          ),
        )
        return
      }
    }
    // Update the session before JS observes the picked token, so prepare
    // cannot reference a staging name the session never saw.
    val wrapped = object : Promise {
      override fun resolve(value: Any?) {
        val map = value as? Map<*, *>
        if (map?.get("status") == "picked") {
          val token = map["token"] as? String
          val byteCount = (map["byteCount"] as? Number)?.toLong() ?: 0L
          if (token != null) onPicked(token, byteCount, context)
        }
        promise.resolve(value)
      }

      override fun reject(code: String?, message: String?, cause: Throwable?) {
        promise.reject(code, message, cause)
      }
    }
    picker(appContext, context).pick(parsed, wrapped)
  }

  fun upload(terminalId: String, remoteDirectory: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
      ?: return AttachmentResults.unavailable(AttachmentLimits.REASON_NO_ATTACHMENT)
    val prepared = active.prepared
      ?: return AttachmentResults.error(
        AttachmentLimits.ERROR_MISSING,
        "The prepared image is missing; prepare it again.",
      )
    if (active.target.terminalId != terminalId) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_TARGET,
        "The attachment belongs to a different terminal.",
      )
    }
    val file = store(context).preparedFile(prepared.fileName)
      ?: return AttachmentResults.error(
        AttachmentLimits.ERROR_MISSING,
        "The prepared image is missing; prepare it again.",
      )
    val result = AttachmentCoreBridge.upload(
      ensureNativeHandle(active.target.terminalId),
      file.absolutePath,
      remoteDirectory,
    )
    val remotePath = result["remotePath"] as? String
    if (result["status"] == "uploaded" && !remotePath.isNullOrEmpty()) {
      synchronized(lock) { active.remotePath = remotePath }
    }
    return result
  }

  fun deleteRemote(terminalId: String, remotePath: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
    if (active == null || active.target.terminalId != terminalId) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_TARGET,
        "The attachment belongs to a different terminal.",
      )
    }
    val result = AttachmentCoreBridge.delete(
      ensureNativeHandle(active.target.terminalId),
      remotePath,
    )
    if (result["status"] == "deleted") {
      synchronized(lock) { active.remotePath = null }
    }
    return result
  }

  fun insert(terminalId: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
    val verdict = AttachmentInsertionPolicy.insert(
      composing = AttachmentCompositionGuard.isComposing(terminalId),
      hasActiveSession = active != null,
      hasPreparedImage = active?.prepared != null &&
        active.prepared?.fileName?.let { store(context).preparedFile(it)?.isFile } == true,
      hasUploadedPath = active?.remotePath != null,
      sessionTerminalId = active?.target?.terminalId,
      requestTerminalId = terminalId,
    )
    return when (verdict) {
      AttachmentInsertionPolicy.Verdict.HeldComposing ->
        AttachmentResults.held(AttachmentLimits.REASON_COMPOSING)
      AttachmentInsertionPolicy.Verdict.HeldNoAttachment ->
        AttachmentResults.held(AttachmentLimits.REASON_NO_ATTACHMENT)
      is AttachmentInsertionPolicy.Verdict.Rejected ->
        AttachmentResults.error(verdict.errorCode, verdict.message)
      AttachmentInsertionPolicy.Verdict.ReadyToInsert ->
        AttachmentCoreBridge.insert(
          ensureNativeHandle(terminalId),
          active?.remotePath,
        )
    }
  }

  private fun ensureNativeHandle(terminalId: String): Long {
    val existing = TerminalRegistry.handleFor(terminalId)
    if (existing != 0L) return existing
    return TerminalRegistry.ensure(terminalId, 80, 24)
  }

  private fun emptySessionSnapshot(): Map<String, Any?> = mapOf(
    "status" to "idle",
    "fileId" to "",
    "previewUri" to "",
    "format" to "",
    "width" to 0,
    "height" to 0,
    "byteCount" to 0L,
    "sourceByteCount" to 0L,
    "remotePath" to "",
    "errorCode" to "",
    "message" to "",
  )
}
