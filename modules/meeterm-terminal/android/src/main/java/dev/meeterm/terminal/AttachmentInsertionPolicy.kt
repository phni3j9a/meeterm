package dev.meeterm.terminal

/**
 * IME-safe attachment insertion decision.
 *
 * The composition check is first-class: an active Japanese conversion keeps
 * the request `held` and never commits, clears, or forwards preedit text.
 * Identity mismatches and Phase-A core unavailability are separate results so
 * the sheet can explain each one distinctly.
 */
internal object AttachmentInsertionPolicy {
  sealed class Verdict {
    data object HeldComposing : Verdict()
    data object HeldNoAttachment : Verdict()
    data class Rejected(val errorCode: String, val message: String) : Verdict()
    data object ReadyToInsert : Verdict()
  }

  fun begin(
    composing: Boolean,
    hasActiveSession: Boolean,
  ): Verdict = when {
    composing -> Verdict.HeldComposing
    hasActiveSession -> Verdict.ReadyToInsert
    else -> Verdict.Rejected(
      AttachmentLimits.ERROR_STATE,
      "No attachment session is active.",
    )
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
      AttachmentLimits.ERROR_TARGET,
      "The attachment belongs to a different terminal.",
    )
    hasUploadedPath || hasPreparedImage -> Verdict.ReadyToInsert
    else -> Verdict.Rejected(
      AttachmentLimits.ERROR_MISSING,
      "The prepared image is missing; prepare it again.",
    )
  }
}
