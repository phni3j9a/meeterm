import Foundation

/**
 * Issue #28 one-attachment session model.
 *
 * JS sees only narrow result dictionaries; the staging/prepared file names are
 * kept here, and `snapshot` exposes display metadata plus the app-local
 * preview file URL.
 */
struct AttachmentTargetIdentity {
  let terminalId: String
  let paneId: String
  let workspaceId: String
  let backend: String
  let runtime: String
  let host: String
  let port: Int

  static func from(_ raw: [String: Any]?) -> AttachmentTargetIdentity? {
    guard let raw = raw,
          let terminalId = raw["terminalId"] as? String,
          let paneId = raw["paneId"] as? String,
          let workspaceId = raw["workspaceId"] as? String,
          let backend = raw["backend"] as? String,
          let runtime = raw["runtime"] as? String,
          let host = raw["host"] as? String,
          let port = raw["port"] as? Int ?? (raw["port"] as? NSNumber)?.intValue else {
      return nil
    }
    return AttachmentTargetIdentity(
      terminalId: terminalId,
      paneId: paneId,
      workspaceId: workspaceId,
      backend: backend,
      runtime: runtime,
      host: host,
      port: port
    )
  }
}

struct AttachmentPreparedImage {
  let fileName: String
  let format: AttachmentImageFormat
  let width: Int
  let height: Int
  let byteCount: Int64
  let sourceByteCount: Int64
}

enum AttachmentSessionStatus: String {
  case idle
  case staged
  case prepared
}

final class AttachmentSession {
  let target: AttachmentTargetIdentity
  var stagingFileName: String?
  var prepared: AttachmentPreparedImage?
  var lastErrorCode = ""
  var lastMessage = ""

  /// Live Rust-owned operation; its snapshot stays authoritative.
  let machine = AttachmentOpMachine()

  init(target: AttachmentTargetIdentity) { self.target = target }

  var status: AttachmentSessionStatus {
    if prepared != nil { return .prepared }
    if stagingFileName != nil { return .staged }
    return .idle
  }

  func recordError(_ errorCode: String, _ message: String) {
    lastErrorCode = errorCode
    lastMessage = message
  }

  func clearError() {
    lastErrorCode = ""
    lastMessage = ""
  }

  func snapshot(previewUri: String) -> [String: Any] {
    [
      "status": status.rawValue,
      "fileId": prepared?.fileName ?? "",
      "previewUri": prepared != nil ? previewUri : "",
      "format": prepared?.format.rawValue ?? "",
      "width": prepared?.width ?? 0,
      "height": prepared?.height ?? 0,
      "byteCount": prepared?.byteCount ?? 0,
      "sourceByteCount": prepared?.sourceByteCount ?? 0,
      "target": [
        "terminalId": target.terminalId,
        "paneId": target.paneId,
        "workspaceId": target.workspaceId,
        "backend": target.backend,
        "runtime": target.runtime,
        "host": target.host,
        "port": target.port,
      ],
      "operation": machine.operation.map(AttachmentResults.operation) ?? NSNull(),
      "errorCode": lastErrorCode,
      "message": lastMessage,
    ]
  }
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

  static func picked(token: String, byteCount: Int64) -> [String: Any] {
    ["status": "picked", "token": token, "byteCount": byteCount]
  }

  static func canceled() -> [String: Any] { ["status": "canceled"] }

  static func prepared(_ image: AttachmentPreparedImage, previewUri: String) -> [String: Any] {
    [
      "status": "prepared",
      "fileId": image.fileName,
      "previewUri": previewUri,
      "format": image.format.rawValue,
      "width": image.width,
      "height": image.height,
      "byteCount": image.byteCount,
      "sourceByteCount": image.sourceByteCount,
    ]
  }

  /// Uniform accepted answer carrying the decimal u64 attachment id.
  static func accepted(_ attachmentId: UInt64) -> [String: Any] {
    ["status": "accepted", "attachmentId": String(attachmentId)]
  }

  static func inserted() -> [String: Any] { ["status": "inserted"] }

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
    default: return "native_error"
    }
  }
}
