import Foundation

/**
 * Reserved junction for the W2 Rust attachment contract.
 *
 * Phase A keeps every remote operation report-only: the calls exist at the
 * JS boundary, but until `meeterm_core` ships the attachment upload/insert/
 * delete FFI each one answers `unavailable` with `core_contract_pending`.
 * Wire the real `MeetermCore.*` calls inside this enum only.
 */
enum AttachmentCoreBridge {
  static func upload(
    terminalHandle: UInt64,
    localPath: String,
    remoteDirectory: String
  ) -> [String: Any] {
    AttachmentResults.unavailable(AttachmentLimits.reasonCorePending)
  }

  static func delete(terminalHandle: UInt64, remotePath: String) -> [String: Any] {
    AttachmentResults.unavailable(AttachmentLimits.reasonCorePending)
  }

  static func insert(terminalHandle: UInt64, remotePath: String?) -> [String: Any] {
    AttachmentResults.unavailable(AttachmentLimits.reasonCorePending)
  }
}
