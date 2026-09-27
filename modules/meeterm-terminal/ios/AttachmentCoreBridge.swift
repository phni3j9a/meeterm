import Foundation

/**
 * Exclusive seam between the adapter and the Rust attachment ABI
 * (attachment-ffi.md + Main amendments). Every core call passes through this
 * enum, which maps one-to-one onto `MeetermCore.attachment*` — no other Swift
 * file may reference the C symbols. A negative contract code is surfaced with
 * its sanitized error name.
 */
enum AttachmentCoreBridge {
  private static func actionResult(_ code: Int32, attachmentId: UInt64) -> [String: Any] {
    code == 0
      ? AttachmentResults.accepted(attachmentId)
      : AttachmentResults.error(
          AttachmentResults.returnCodeError(code),
          "The attachment request was rejected by the core."
        )
  }

  /**
   * `meeterm_attachment_intent`: record the destination intent for the
   * picked pane's native terminal. The core resolves the owning SSH
   * connection itself — the adapter never passes an owner id.
   * Returns the opaque intent id (>0) or 0 when the pane is not a usable
   * destination.
   */
  static func intent(targetTerminalId: UInt64) -> UInt64 {
    MeetermCore.attachmentIntent(targetTerminalId: targetTerminalId)
  }

  /// `meeterm_attachment_intent_dispose`; idempotent.
  static func intentDispose(intentId: UInt64) {
    _ = MeetermCore.attachmentIntentDispose(intentId: intentId)
  }

  /// `meeterm_attachment_begin`. An empty remoteDirectory selects the core
  /// default `~/.local/share/meeterm/attachments`. Returns the attachment id
  /// (>0) or nil on synchronous rejection (unknown intent, unreadable file,
  /// changed/missing destination, or a second live op).
  static func begin(
    intentId: UInt64,
    localPath: String,
    displayName: String,
    remoteDirectory: String,
    sizeBytes: UInt64
  ) -> UInt64? {
    let attachmentId = MeetermCore.attachmentBegin(
      intentId: intentId,
      localPath: localPath,
      displayName: displayName,
      remoteDirectory: remoteDirectory,
      sizeBytes: sizeBytes
    )
    return attachmentId > 0 ? attachmentId : nil
  }

  /// `meeterm_attachment_retry_upload`; uniform accepted/error map.
  /// `targetTerminalId` must be the intent's recorded pane terminal.
  static func retryUpload(terminalId: UInt64, attachmentId: UInt64) -> [String: Any] {
    actionResult(
      MeetermCore.attachmentRetryUpload(targetTerminalId: terminalId, attachmentId: attachmentId),
      attachmentId: attachmentId
    )
  }

  /// `meeterm_attachment_insert`; uniform accepted/error map.
  static func insert(terminalId: UInt64, attachmentId: UInt64) -> [String: Any] {
    actionResult(
      MeetermCore.attachmentInsert(targetTerminalId: terminalId, attachmentId: attachmentId),
      attachmentId: attachmentId
    )
  }

  /// `meeterm_attachment_cancel`; uniform accepted/error map.
  static func cancel(attachmentId: UInt64) -> [String: Any] {
    actionResult(MeetermCore.attachmentCancel(attachmentId: attachmentId), attachmentId: attachmentId)
  }

  /// `meeterm_attachment_dispose`; uniform accepted/error map.
  static func dispose(attachmentId: UInt64) -> [String: Any] {
    actionResult(MeetermCore.attachmentDispose(attachmentId: attachmentId), attachmentId: attachmentId)
  }

  /// `meeterm_attachment_delete_remote`; uniform accepted/error map.
  /// `targetTerminalId` must be the intent's recorded pane terminal.
  static func deleteRemote(terminalId: UInt64, attachmentId: UInt64) -> [String: Any] {
    actionResult(
      MeetermCore.attachmentDeleteRemote(targetTerminalId: terminalId, attachmentId: attachmentId),
      attachmentId: attachmentId
    )
  }

  /// `meeterm_attachment_snapshot` decoded by the pure codec.
  static func snapshot(attachmentId: UInt64) -> AttachmentOperation? {
    guard let bytes = MeetermCore.attachmentSnapshot(attachmentId: attachmentId) else {
      return nil
    }
    return AttachmentOperationCodec.decode(bytes)
  }
}
