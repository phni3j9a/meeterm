package dev.meeterm.terminal

import android.content.Context
import expo.modules.kotlin.Promise
import java.io.IOException

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
      val kept = session?.let {
        setOfNotNull(it.stagingFileName, it.prepared?.fileName)
      } ?: emptySet()
      // Create the directory/store first so a first-launch reclaim still
      // sweeps files the previous process left behind. `reclaimed` flips only
      // after the sweep actually ran — a failing store retries next launch.
      try {
        store(context).reclaimStale(kept)
      } catch (e: IOException) {
        return
      } catch (e: RuntimeException) {
        return
      }
      reclaimed = true
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
      // older staging, prepared file, or live operation from this session.
      val stale = active.stagingFileName
      val stalePrepared = active.prepared?.fileName
      active.stagingFileName = token
      active.prepared = null
      active.clearError()
      retireOperationLocked(active)
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

  /** Explicit Discard: cancel/dispose the core op, then delete local files. */
  fun discard(context: Context) {
    val active = synchronized(lock) {
      val current = session
      session = null
      current
    } ?: return
    val op = active.machine.operation
    if (op != null) {
      if (op.canCancel) AttachmentCoreBridge.cancel(op.attachmentId)
      AttachmentCoreBridge.dispose(op.attachmentId)
    }
    active.machine.clear()
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

  /**
   * Explicit Upload: `meeterm_attachment_begin` over the fenced connection.
   * A second upload is refused while the previous operation is live.
   */
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
    val previousOpId = synchronized(lock) { active.machine.operation?.attachmentId }
    if (!active.machine.canBeginUpload()) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "An attachment upload is already in progress.",
      )
    }
    val file = store(context).preparedFile(prepared.fileName)
      ?: return AttachmentResults.error(
        AttachmentLimits.ERROR_MISSING,
        "The prepared image is missing; prepare it again.",
      )
    val attachmentId = AttachmentCoreBridge.begin(
      ensureNativeHandle(active.target.terminalId),
      file.absolutePath,
      prepared.fileName,
      // The JNI `remote_dir` is nullable: null selects the app-private
      // default, while an empty string is a validation rejection.
      remoteDirectory.trim().ifEmpty { null },
      prepared.byteCount,
    ) ?: return AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING)
    if (attachmentId == 0L) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "The core could not start the upload; check the connection.",
      )
    }
    val recorded = synchronized(lock) {
      val ok = active.machine.recordBegin(attachmentId, prepared.byteCount, prepared.fileName)
      if (ok) active.clearError()
      ok
    }
    if (recorded && previousOpId != null && previousOpId != attachmentId) {
      // The replaced op is terminal by definition; release its core record.
      AttachmentCoreBridge.dispose(previousOpId)
    }
    if (!recorded) {
      // A racing upload won the slot; release the orphaned core op.
      AttachmentCoreBridge.dispose(attachmentId)
      return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "An attachment upload is already in progress.",
      )
    }
    refreshSnapshot(active)
    return AttachmentResults.accepted(attachmentId)
  }

  /** Poll the live core operation and fold it into the session machine. */
  fun attachmentSnapshot(): Map<String, Any?> {
    val active = synchronized(lock) { session }
      ?: return mapOf("status" to "idle")
    val op = synchronized(lock) { active.machine.operation }
      ?: return mapOf("status" to "idle")
    val fresh = AttachmentCoreBridge.snapshot(op.attachmentId)
      ?: return AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING)
    synchronized(lock) {
      if (active.machine.operation?.attachmentId == fresh.attachmentId) {
        active.machine.applySnapshot(fresh)
      }
    }
    return AttachmentResults.snapshotResult(fresh)
  }

  /** Explicit transfer retry on a pending/failed operation. */
  fun retryUpload(terminalId: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
    if (active == null || active.target.terminalId != terminalId) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_TARGET,
        "The attachment belongs to a different terminal.",
      )
    }
    val op = active.machine.operation
    if (op == null || !op.canRetryUpload) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "There is no upload to retry.",
      )
    }
    val result = AttachmentCoreBridge.retryUpload(
      ensureNativeHandle(terminalId),
      op.attachmentId,
    )
    if (result["status"] == "accepted") refreshSnapshot(active)
    return result
  }

  /** Explicit cancel of a pending/uploading operation. */
  fun cancel(): Map<String, Any?> {
    val active = synchronized(lock) { session }
      ?: return AttachmentResults.unavailable(AttachmentLimits.REASON_NO_ATTACHMENT)
    val op = active.machine.operation
    if (op == null || !op.canCancel) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "There is no upload to cancel.",
      )
    }
    val result = AttachmentCoreBridge.cancel(op.attachmentId)
    if (result["status"] == "accepted") {
      synchronized(lock) { active.machine.markCancelled() }
    }
    return result
  }

  /** Explicit server-side delete of the completed remote file. */
  fun deleteRemote(terminalId: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
    if (active == null || active.target.terminalId != terminalId) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_TARGET,
        "The attachment belongs to a different terminal.",
      )
    }
    val op = active.machine.operation
    if (op == null || !op.canDeleteRemote) {
      return AttachmentResults.error(
        AttachmentLimits.ERROR_STATE,
        "There is no uploaded file to delete.",
      )
    }
    val result = AttachmentCoreBridge.deleteRemote(
      ensureNativeHandle(active.target.terminalId),
      op.attachmentId,
    )
    if (result["status"] == "accepted") refreshSnapshot(active)
    return result
  }

  fun insert(terminalId: String, context: Context): Map<String, Any?> {
    val active = synchronized(lock) { session }
    val op = active?.machine?.operation
    val verdict = AttachmentInsertionPolicy.insert(
      composing = AttachmentCompositionGuard.isComposing(terminalId),
      hasActiveSession = active != null,
      hasPreparedImage = op?.canInsert == true,
      hasUploadedPath = op?.phase == AttachmentOpPhase.UPLOADED,
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
      AttachmentInsertionPolicy.Verdict.ReadyToInsert -> {
        val result = AttachmentCoreBridge.insert(
          ensureNativeHandle(terminalId),
          op!!.attachmentId,
        )
        if (result["status"] == "accepted") {
          refreshSnapshot(active!!)
          return AttachmentResults.inserted()
        }
        result
      }
    }
  }

  private fun refreshSnapshot(active: AttachmentSession) {
    val id = synchronized(lock) { active.machine.operation?.attachmentId } ?: return
    val fresh = AttachmentCoreBridge.snapshot(id) ?: return
    synchronized(lock) {
      if (active.machine.operation?.attachmentId == fresh.attachmentId) {
        active.machine.applySnapshot(fresh)
      }
    }
  }

  /** Cancel + dispose + clear a live op while `lock` is held. */
  private fun retireOperationLocked(active: AttachmentSession) {
    val op = active.machine.operation ?: return
    if (op.canCancel) AttachmentCoreBridge.cancel(op.attachmentId)
    AttachmentCoreBridge.dispose(op.attachmentId)
    active.machine.clear()
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
    "target" to null,
    "operation" to null,
    "errorCode" to "",
    "message" to "",
  )
}
