import ExpoModulesCore
import Foundation
import UIKit

/**
 * Process-wide attachment session owner for Issue #28.
 *
 * A single pending attachment is allowed at a time; `begin` replaces any
 * previous one after deleting its files. Session files live under
 * `Caches/attachments/` and are reclaimed on startup when no session
 * references them.
 */
final class AttachmentController {
  static let shared = AttachmentController()

  private let lock = NSLock()
  private var store: AttachmentStore?
  private var picker: AttachmentPicker?
  private var session: AttachmentSession?
  private var reclaimed = false

  private func requireStore() throws -> AttachmentStore {
    lock.lock()
    defer { lock.unlock() }
    if let existing = store { return existing }
    let created = try AttachmentStore()
    store = created
    return created
  }

  /// Startup reclaim; safe to call repeatedly.
  func reclaimStaleFiles() {
    lock.lock()
    defer { lock.unlock() }
    guard !reclaimed else { return }
    reclaimed = true
    guard let existing = store else { return }
    var kept = Set<String>()
    if let staging = session?.stagingFileName { kept.insert(staging) }
    if let prepared = session?.prepared?.fileName { kept.insert(prepared) }
    existing.reclaimStale(keeping: kept)
  }

  func begin(terminalId: String, target: AttachmentTargetIdentity) throws -> [String: Any] {
    if AttachmentCompositionGuard.shared.isComposing(terminalId: terminalId) {
      return AttachmentResults.held(AttachmentLimits.reasonComposing)
    }
    let existingStore = try requireStore()
    lock.lock()
    let previous = session
    session = AttachmentSession(target: target)
    lock.unlock()
    existingStore.delete(previous?.stagingFileName, previous?.prepared?.fileName)
    return ["status": "ready"]
  }

  func pick(
    source: String,
    promise: Promise,
    viewController: () -> UIViewController?
  ) throws {
    let parsed: AttachmentPicker.Source
    switch source {
    case "photos": parsed = .photos
    case "files": parsed = .files
    default:
      promise.resolve(AttachmentResults.error(
        AttachmentLimits.errorArgument,
        "Unknown attachment source."
      ))
      return
    }
    let existingStore = try requireStore()
    lock.lock()
    if picker == nil { picker = AttachmentPicker(store: existingStore) }
    let activePicker = picker!
    lock.unlock()
    activePicker.pick(source: parsed, from: viewController()) { [weak self] result in
      if (result["status"] as? String) == "picked",
         let token = result["token"] as? String {
        self?.onPicked(token: token)
      }
      promise.resolve(result)
    }
  }

  private func onPicked(token: String) {
    lock.lock()
    defer { lock.unlock() }
    guard let active = session, AttachmentFileNames.isStagingName(token) else { return }
    // A fresh pick invalidates any older staging or prepared files from the
    // same session.
    let stale = active.stagingFileName
    let stalePrepared = active.prepared?.fileName
    active.stagingFileName = token
    active.prepared = nil
    active.clearError()
    // A fresh pick also retires any live core operation from this session.
    retireOperationLocked(active)
    store?.delete(stale == token ? nil : stale, stalePrepared == token ? nil : stalePrepared)
  }

  func prepare(token: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active else {
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "No attachment session is active."
      )
    }
    guard active.stagingFileName == token else {
      return AttachmentResults.error(
        AttachmentLimits.errorMissing,
        "The picked image is no longer available."
      )
    }
    let existingStore = try requireStore()
    switch AttachmentNormalize(store: existingStore).normalize(stagingFileName: token) {
    case .ok(let image):
      lock.lock()
      active.prepared = image
      active.stagingFileName = nil
      active.clearError()
      lock.unlock()
      existingStore.delete(token)
      return AttachmentResults.prepared(
        image,
        previewUri: existingStore.previewUri(image.fileName)
      )
    case .rejected(let errorCode, let message):
      lock.lock()
      active.recordError(errorCode, message)
      lock.unlock()
      return AttachmentResults.error(errorCode, message)
    }
  }

  /// Explicit Discard: cancel/dispose the core op, then delete local files.
  func discard() throws {
    lock.lock()
    let active = session
    session = nil
    lock.unlock()
    guard let active = active else { return }
    lock.lock()
    retireOperationLocked(active)
    lock.unlock()
    try requireStore().delete(active.stagingFileName, active.prepared?.fileName)
  }

  func snapshot() throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active else {
      return [
        "status": "idle",
        "fileId": "",
        "previewUri": "",
        "format": "",
        "width": 0,
        "height": 0,
        "byteCount": 0,
        "sourceByteCount": 0,
        "target": NSNull(),
        "operation": NSNull(),
        "errorCode": "",
        "message": "",
      ]
    }
    let preview = active.prepared?.fileName
      .flatMap { try? requireStore().previewUri($0) } ?? ""
    return active.snapshot(previewUri: preview)
  }

  /**
   * Explicit Upload: `meeterm_attachment_begin` over the fenced connection.
   * A second upload is refused while the previous operation is live.
   */
  func upload(terminalId: String, remoteDirectory: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active else {
      return AttachmentResults.unavailable(AttachmentLimits.reasonNoAttachment)
    }
    guard active.target.terminalId == terminalId else {
      return AttachmentResults.error(
        AttachmentLimits.errorTarget,
        "The attachment belongs to a different terminal."
      )
    }
    guard let prepared = active.prepared,
          let url = try requireStore().preparedURL(prepared.fileName) else {
      return AttachmentResults.error(
        AttachmentLimits.errorMissing,
        "The prepared image is missing; prepare it again."
      )
    }
    lock.lock()
    let previousOpId = active.machine.operation?.attachmentId
    lock.unlock()
    guard active.machine.canBeginUpload() else {
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "An attachment upload is already in progress."
      )
    }
    guard let attachmentId = AttachmentCoreBridge.begin(
      terminalId: try ensureHandle(active.target.terminalId),
      localPath: url.path,
      displayName: prepared.fileName,
      remoteDirectory: remoteDirectory,
      sizeBytes: UInt64(clamping: prepared.byteCount)
    ) else {
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "The core could not start the upload; check the connection."
      )
    }
    lock.lock()
    let recorded = active.machine.recordBegin(
      attachmentId: attachmentId,
      sizeBytes: UInt64(clamping: prepared.byteCount),
      displayName: prepared.fileName
    )
    if recorded { active.clearError() }
    lock.unlock()
    if !recorded {
      // A racing upload won the slot; release the orphaned core op.
      _ = AttachmentCoreBridge.dispose(attachmentId: attachmentId)
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "An attachment upload is already in progress."
      )
    }
    if let previousOpId = previousOpId, previousOpId != attachmentId {
      // The replaced op is terminal by definition; release its core record.
      _ = AttachmentCoreBridge.dispose(attachmentId: previousOpId)
    }
    refreshSnapshot(active)
    return AttachmentResults.accepted(attachmentId)
  }

  /// Poll the live core operation and fold it into the session machine.
  func attachmentSnapshot() -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let operation = active?.machine.operation else {
      return ["status": "idle"]
    }
    guard let fresh = AttachmentCoreBridge.snapshot(attachmentId: operation.attachmentId) else {
      return AttachmentResults.unavailable(AttachmentLimits.reasonCorePending)
    }
    lock.lock()
    if active?.machine.operation?.attachmentId == fresh.attachmentId {
      active?.machine.applySnapshot(fresh)
    }
    lock.unlock()
    return AttachmentResults.snapshotResult(fresh)
  }

  /// Explicit transfer retry on a pending/failed operation.
  func retryUpload(terminalId: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active, active.target.terminalId == terminalId else {
      return AttachmentResults.error(
        AttachmentLimits.errorTarget,
        "The attachment belongs to a different terminal."
      )
    }
    guard let operation = active.machine.operation, operation.canRetryUpload else {
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "There is no upload to retry."
      )
    }
    let result = AttachmentCoreBridge.retryUpload(
      terminalId: try ensureHandle(terminalId),
      attachmentId: operation.attachmentId
    )
    if (result["status"] as? String) == "accepted" { refreshSnapshot(active) }
    return result
  }

  /// Explicit cancel of a pending/uploading operation.
  func cancel() -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active else {
      return AttachmentResults.unavailable(AttachmentLimits.reasonNoAttachment)
    }
    guard let operation = active.machine.operation, operation.canCancel else {
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "There is no upload to cancel."
      )
    }
    let result = AttachmentCoreBridge.cancel(attachmentId: operation.attachmentId)
    if (result["status"] as? String) == "accepted" {
      lock.lock()
      active.machine.markCancelled()
      lock.unlock()
    }
    return result
  }

  /// Explicit server-side delete of the completed remote file.
  func deleteRemote(terminalId: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active, active.target.terminalId == terminalId else {
      return AttachmentResults.error(
        AttachmentLimits.errorTarget,
        "The attachment belongs to a different terminal."
      )
    }
    guard let operation = active.machine.operation, operation.canDeleteRemote else {
      return AttachmentResults.error(
        AttachmentLimits.errorState,
        "There is no uploaded file to delete."
      )
    }
    let result = AttachmentCoreBridge.deleteRemote(
      terminalId: try ensureHandle(active.target.terminalId),
      attachmentId: operation.attachmentId
    )
    if (result["status"] as? String) == "accepted" { refreshSnapshot(active) }
    return result
  }

  func insert(terminalId: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    let operation = active?.machine.operation
    let verdict = AttachmentInsertionPolicy.insert(
      composing: AttachmentCompositionGuard.shared.isComposing(terminalId: terminalId),
      hasActiveSession: active != nil,
      hasPreparedImage: operation?.canInsert == true,
      hasUploadedPath: operation?.phase == .uploaded,
      sessionTerminalId: active?.target.terminalId,
      requestTerminalId: terminalId
    )
    switch verdict {
    case .heldComposing:
      return AttachmentResults.held(AttachmentLimits.reasonComposing)
    case .heldNoAttachment:
      return AttachmentResults.held(AttachmentLimits.reasonNoAttachment)
    case .rejected(let errorCode, let message):
      return AttachmentResults.error(errorCode, message)
    case .readyToInsert:
      guard let operation = operation, let active = active else {
        return AttachmentResults.held(AttachmentLimits.reasonNoAttachment)
      }
      let result = AttachmentCoreBridge.insert(
        terminalId: try ensureHandle(terminalId),
        attachmentId: operation.attachmentId
      )
      if (result["status"] as? String) == "accepted" {
        refreshSnapshot(active)
        return AttachmentResults.inserted()
      }
      return result
    }
  }

  /// Fold the latest core snapshot into the session when the id still matches.
  private func refreshSnapshot(_ active: AttachmentSession) {
    lock.lock()
    let attachmentId = active.machine.operation?.attachmentId
    lock.unlock()
    guard let attachmentId = attachmentId,
          let fresh = AttachmentCoreBridge.snapshot(attachmentId: attachmentId) else {
      return
    }
    lock.lock()
    if active.machine.operation?.attachmentId == fresh.attachmentId {
      active.machine.applySnapshot(fresh)
    }
    lock.unlock()
  }

  /// Cancel + dispose + clear a live op while `lock` is held.
  private func retireOperationLocked(_ active: AttachmentSession) {
    guard let operation = active.machine.operation else { return }
    if operation.canCancel {
      _ = AttachmentCoreBridge.cancel(attachmentId: operation.attachmentId)
    }
    _ = AttachmentCoreBridge.dispose(attachmentId: operation.attachmentId)
    active.machine.clear()
  }

  private func ensureHandle(_ terminalId: String) throws -> UInt64 {
    let existing = TerminalRegistry.handle(for: terminalId)
    if existing != 0 { return existing }
    return TerminalRegistry.ensure(terminalId: terminalId, columns: 80, rows: 24)
  }
}
