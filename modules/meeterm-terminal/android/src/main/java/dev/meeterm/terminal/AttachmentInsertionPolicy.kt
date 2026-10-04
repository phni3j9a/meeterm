package dev.meeterm.terminal

/**
 * IME-safe attachment insertion decision.
 *
 * The composition check is first-class: an active Japanese conversion keeps
 * the request `held` and never commits, clears, or forwards preedit text.
 * Identity mismatches and core unavailability remain distinct results.
 */
internal object AttachmentInsertionPolicy {
  sealed class Verdict {
    data object HeldComposing : Verdict()
    data object HeldNoAttachment : Verdict()
    data class Rejected(val errorCode: String, val message: String) : Verdict()
    data object ReadyToInsert : Verdict()
  }

  fun insert(
    composing: Boolean,
    hasActiveSession: Boolean,
    hasPreparedImage: Boolean,
    hasUploadedPath: Boolean,
    sessionTerminalId: String?,
    requestTerminalId: String,
  ): Verdict = when {
    composing -> Verdict.HeldComposing
    sessionTerminalId == null || !hasActiveSession -> Verdict.HeldNoAttachment
    sessionTerminalId != requestTerminalId -> Verdict.Rejected(
      AttachmentLimits.ERROR_DESTINATION_CHANGED,
      "The attachment belongs to a different terminal.",
    )
    hasUploadedPath || hasPreparedImage -> Verdict.ReadyToInsert
    else -> Verdict.Rejected(
      AttachmentLimits.ERROR_MISSING,
      "The prepared image is missing; prepare it again.",
    )
  }
}
