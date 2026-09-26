package dev.meeterm.terminal

/**
 * Reserved junction for the W2 Rust attachment contract.
 *
 * Phase A keeps every remote operation report-only: the calls exist at the JS
 * boundary, but until `meeterm_core` ships the attachment upload/insert/delete
 * FFI each one answers `unavailable` with `core_contract_pending`. Wire the
 * real `MeetermNative.*` calls inside this object only.
 */
internal object AttachmentCoreBridge {
  fun upload(handle: Long, localPath: String, remoteDirectory: String): Map<String, Any?> =
    AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING)

  fun delete(handle: Long, remotePath: String): Map<String, Any?> =
    AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING)

  fun insert(handle: Long, remotePath: String?): Map<String, Any?> =
    AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING)
}
