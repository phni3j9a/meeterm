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
  case uploaded
}

final class AttachmentSession {
  let target: AttachmentTargetIdentity
  var stagingFileName: String?
  var prepared: AttachmentPreparedImage?
  var remotePath: String?
  var lastErrorCode = ""
  var lastMessage = ""

  init(target: AttachmentTargetIdentity) { self.target = target }

  var status: AttachmentSessionStatus {
    if prepared != nil, remotePath != nil { return .uploaded }
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
      "remotePath": remotePath ?? "",
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

  static func uploaded(_ remotePath: String) -> [String: Any] {
    ["status": "uploaded", "remotePath": remotePath]
  }

  static func deleted() -> [String: Any] { ["status": "deleted"] }

  static func inserted() -> [String: Any] { ["status": "inserted"] }
}
