import Foundation

/**
 * IME-safe attachment insertion decision.
 *
 * An active marked-text composition keeps the request `held` and is never
 * committed, cleared, or forwarded. Identity mismatches and Phase-A core
 * unavailability stay distinct so the sheet can explain each one.
 */
enum AttachmentInsertionPolicy {
  enum Verdict: Equatable {
    case heldComposing
    case heldNoAttachment
    case rejected(String, String)
    case readyToInsert
  }

  static func insert(
    composing: Bool,
    hasActiveSession: Bool,
    hasPreparedImage: Bool,
    hasUploadedPath: Bool,
    sessionTerminalId: String?,
    requestTerminalId: String
  ) -> Verdict {
    if composing { return .heldComposing }
    guard let sessionTerminalId = sessionTerminalId, hasActiveSession else {
      return .heldNoAttachment
    }
    guard sessionTerminalId == requestTerminalId else {
      return .rejected(
        AttachmentLimits.errorDestinationChanged,
        "The attachment belongs to a different terminal."
      )
    }
    if hasUploadedPath || hasPreparedImage { return .readyToInsert }
    return .rejected(
      AttachmentLimits.errorMissing,
      "The prepared image is missing; prepare it again."
    )
  }
}
