import Foundation

/** One-tap image session. File names remain native and never cross to JS. */
/**
 * Pane-scoped identity for display and session binding. The Rust core
 * resolves the owning SSH endpoint/runtime/remote pane itself from the
 * pane's native terminal id (`meeterm_attachment_intent`), so the adapter
 * never records or reuses an SSH owner id.
 */
struct AttachmentTargetIdentity {
  let terminalId: String
  let paneId: String
  let workspaceId: String

  static func from(_ raw: [String: Any]?) -> AttachmentTargetIdentity? {
    guard let raw = raw,
          let terminalId = raw["terminalId"] as? String,
          let paneId = raw["paneId"] as? String,
          let workspaceId = raw["workspaceId"] as? String else {
      return nil
    }
    return AttachmentTargetIdentity(
      terminalId: terminalId,
      paneId: paneId,
      workspaceId: workspaceId
    )
  }
}

struct AttachmentPreparedImage {
  let fileName: String
  let byteCount: Int64
}

final class AttachmentSession {
  let target: AttachmentTargetIdentity
  var stagingFileName: String?
  var prepared: AttachmentPreparedImage?
  /// Live `meeterm_attachment_intent` id for the recorded pane terminal; 0
  /// means no intent is held. Replaced by the next `begin`.
  var intentId: UInt64 = 0

  /// Live Rust-owned operation; its snapshot stays authoritative.
  let machine = AttachmentOpMachine()

  init(target: AttachmentTargetIdentity) { self.target = target }

}

/// Result-dictionary builders matching Attachment*.types.ts one-for-one.
enum AttachmentResults {
  static func held(_ reason: String) -> [String: Any] {
    ["status": "held", "reason": reason]
  }

  static func unavailable(_ reason: String) -> [String: Any] {
    ["status": "unavailable", "reason": reason]
  }

  static func error(_ errorCode: String, _ message: String) -> [String: Any] {
    ["status": "error", "errorCode": errorCode, "message": message]
  }

  static func picked(token: String) -> [String: Any] {
    ["status": "picked", "token": token]
  }

  static func canceled() -> [String: Any] { ["status": "canceled"] }

  static func prepared() -> [String: Any] { ["status": "prepared"] }

  /// Uniform accepted answer carrying the decimal u64 attachment id.
  static func accepted(_ attachmentId: UInt64) -> [String: Any] {
    ["status": "accepted", "attachmentId": String(attachmentId)]
  }

  /// Core snapshot fields, mirroring AttachmentOperationSnapshot (TS).
  static func operation(_ op: AttachmentOperation) -> [String: Any] {
    [
      "phase": op.phase.wireName,
      "attachmentId": String(op.attachmentId),
      "bytesUploaded": op.bytesUploaded,
      "sizeBytes": op.sizeBytes,
      "remotePath": op.remotePath,
      "displayName": op.displayName,
      "errorCode": op.errorCode,
      "errorMessage": op.errorMessage,
      "insertUnconfirmed": op.insertUnconfirmed,
      "remoteRemoved": op.remoteRemoved,
      "jobInFlight": op.jobInFlight,
    ]
  }

  static func snapshotResult(_ op: AttachmentOperation?) -> [String: Any] {
    guard let op = op else { return ["status": "idle"] }
    return ["status": "snapshot", "operation": operation(op)]
  }

  /// Contract return code → sanitized snake_case name.
  static func returnCodeError(_ code: Int32) -> String {
    switch code {
    case -1: return "invalid_argument"
    case -2: return "unknown_terminal"
    case -3: return "unknown_attachment"
    case -4: return "invalid_state"
    case -5: return "source_unreadable"
    case -6: return "source_too_large"
    case -7: return "destination_not_ready"
    case -8: return "busy"
    case -9: return "internal_error"
    case -10: return "unknown_intent"
    case -11: return "destination_changed"
    case -12: return "destination_missing"
    default: return "native_error"
    }
  }
}
