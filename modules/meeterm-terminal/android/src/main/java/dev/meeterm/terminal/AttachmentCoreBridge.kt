package dev.meeterm.terminal

/**
 * Junction for the W2 Rust attachment contract (attachment-ffi.md plus the
 * Main amendments). Every core call passes through this object.
 *
 * Until W2's jni.rs lands on this branch the `external fun` symbols are
 * unresolved: each entry catches `UnsatisfiedLinkError` and reports
 * `unavailable` (`core_contract_pending`) so the rest of the flow degrades
 * instead of crashing. Once the .so ships the functions, the calls pass
 * straight through.
 */
internal object AttachmentCoreBridge {

  /** Attachment op result codes, mirroring the contract's error ordering. */
  private fun actionResult(code: Int, attachmentId: Long): Map<String, Any?> =
    if (code == 0) {
      AttachmentResults.accepted(attachmentId)
    } else {
      AttachmentResults.error(
        attachmentReturnCodeName(code),
        "The attachment request was rejected by the core.",
      )
    }

  private fun attachmentReturnCodeName(code: Int): String = when (code) {
    -1 -> "invalid_argument"
    -2 -> "unknown_terminal"
    -3 -> "unknown_attachment"
    -4 -> "invalid_state"
    -5 -> "source_unreadable"
    -6 -> "source_too_large"
    -7 -> "destination_not_ready"
    -8 -> "busy"
    else -> "native_error"
  }

  private fun <T> pending(action: () -> T, fallback: () -> T): T =
    try {
      action()
    } catch (_: UnsatisfiedLinkError) {
      fallback()
    }

  /**
   * `meeterm_attachment_begin`. An empty remoteDirectory selects the core
   * default `~/.local/share/meeterm/attachments`.
   * Returns the attachment id (>0), 0 on synchronous rejection, or null when
   * the core contract is not linked yet.
   */
  fun begin(
    terminalHandle: Long,
    localPath: String,
    displayName: String,
    remoteDirectory: String,
    sizeBytes: Long,
  ): Long? = pending(
    {
      MeetermNative.attachmentBegin(
        terminalHandle, localPath, displayName, remoteDirectory, sizeBytes,
      )
    },
    { null },
  )

  /** `meeterm_attachment_retry_upload`; uniform accepted/error map. */
  fun retryUpload(terminalHandle: Long, attachmentId: Long): Map<String, Any?> =
    pending(
      { actionResult(MeetermNative.attachmentRetryUpload(terminalHandle, attachmentId), attachmentId) },
      { AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING) },
    )

  /** `meeterm_attachment_insert`; uniform accepted/error map. */
  fun insert(terminalHandle: Long, attachmentId: Long): Map<String, Any?> =
    pending(
      { actionResult(MeetermNative.attachmentInsert(terminalHandle, attachmentId), attachmentId) },
      { AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING) },
    )

  /** `meeterm_attachment_cancel`; uniform accepted/error map. */
  fun cancel(attachmentId: Long): Map<String, Any?> =
    pending(
      { actionResult(MeetermNative.attachmentCancel(attachmentId), attachmentId) },
      { AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING) },
    )

  /** `meeterm_attachment_dispose`; uniform accepted/error map. */
  fun dispose(attachmentId: Long): Map<String, Any?> =
    pending(
      { actionResult(MeetermNative.attachmentDispose(attachmentId), attachmentId) },
      { AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING) },
    )

  /** `meeterm_attachment_delete_remote`; uniform accepted/error map. */
  fun deleteRemote(terminalHandle: Long, attachmentId: Long): Map<String, Any?> =
    pending(
      { actionResult(MeetermNative.attachmentDeleteRemote(terminalHandle, attachmentId), attachmentId) },
      { AttachmentResults.unavailable(AttachmentLimits.REASON_CORE_PENDING) },
    )

  /** `meeterm_attachment_snapshot`; decoded record, null when unknown. */
  fun snapshot(attachmentId: Long): AttachmentOperation? =
    pending(
      { MeetermNative.attachmentSnapshot(attachmentId)?.let(AttachmentOperationCodec::decode) },
      { null },
    )
}
