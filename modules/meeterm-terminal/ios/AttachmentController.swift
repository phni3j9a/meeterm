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
    active.remotePath = nil
    active.clearError()
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

  func discard() throws {
    lock.lock()
    let active = session
    session = nil
    lock.unlock()
    guard let active = active else { return }
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
        "remotePath": "",
        "errorCode": "",
        "message": "",
      ]
    }
    let preview = active.prepared?.fileName
      .flatMap { try? requireStore().previewUri($0) } ?? ""
    return active.snapshot(previewUri: preview)
  }

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
    let result = AttachmentCoreBridge.upload(
      terminalHandle: try ensureHandle(active.target.terminalId),
      localPath: url.path,
      remoteDirectory: remoteDirectory
    )
    if (result["status"] as? String) == "uploaded",
       let remotePath = result["remotePath"] as? String, !remotePath.isEmpty {
      lock.lock()
      active.remotePath = remotePath
      lock.unlock()
    }
    return result
  }

  func deleteRemote(terminalId: String, remotePath: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    guard let active = active, active.target.terminalId == terminalId else {
      return AttachmentResults.error(
        AttachmentLimits.errorTarget,
        "The attachment belongs to a different terminal."
      )
    }
    let result = AttachmentCoreBridge.delete(
      terminalHandle: try ensureHandle(active.target.terminalId),
      remotePath: remotePath
    )
    if (result["status"] as? String) == "deleted" {
      lock.lock()
      active.remotePath = nil
      lock.unlock()
    }
    return result
  }

  func insert(terminalId: String) throws -> [String: Any] {
    lock.lock()
    let active = session
    lock.unlock()
    let existingStore = try? requireStore()
    let hasPrepared = active?.prepared
      .map { existingStore?.preparedURL($0.fileName) != nil } ?? false
    let verdict = AttachmentInsertionPolicy.insert(
      composing: AttachmentCompositionGuard.shared.isComposing(terminalId: terminalId),
      hasActiveSession: active != nil,
      hasPreparedImage: hasPrepared,
      hasUploadedPath: active?.remotePath != nil,
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
      return AttachmentCoreBridge.insert(
        terminalHandle: try ensureHandle(terminalId),
        remotePath: active?.remotePath
      )
    }
  }

  private func ensureHandle(_ terminalId: String) throws -> UInt64 {
    let existing = TerminalRegistry.handle(for: terminalId)
    if existing != 0 { return existing }
    return TerminalRegistry.ensure(terminalId: terminalId, columns: 80, rows: 24)
  }
}
