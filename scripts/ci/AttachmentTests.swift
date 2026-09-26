import Foundation
import ImageIO
import UIKit
import XCTest

/// Issue #28 attachment pure-layer coverage, injected into the disposable
/// UI-test target beside the production sources it exercises. The ImageIO
/// pipeline runs on device; these tests pin the validation rules that guard
/// it and the marked-text hold that protects IME composition.
final class AttachmentTests: XCTestCase {
  private var recordedIssue = false

  override func record(_ issue: XCTIssue) {
    recordedIssue = true
    appendValidation("result=failed source_line=\(issue.sourceCodeContext.location?.lineNumber ?? 0)")
    super.record(issue)
  }

  private func appendValidation(_ line: String) {
    guard let directory = ProcessInfo.processInfo.environment["MEETERM_IOS_ARTIFACT_DIR"] else { return }
    let path = URL(fileURLWithPath: directory).appendingPathComponent("ios-attachment-validation.txt")
    let data = Data((line + "\n").utf8)
    if let handle = try? FileHandle(forWritingTo: path) {
      handle.seekToEndOfFile()
      handle.write(data)
      try? handle.close()
    } else {
      try? data.write(to: path, options: .atomic)
    }
  }

  private func png(width: Int, height: Int) -> Data {
    var bytes = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
    bytes.append(contentsOf: [0, 0, 0, 13])
    bytes.append("IHDR".data(using: .ascii)!)
    bytes.append(UInt32(width).bigEndianData)
    bytes.append(UInt32(height).bigEndianData)
    bytes.append(Data(repeating: 0, count: 5))
    return bytes
  }

  /// Minimal JFIF layout: SOI, APP0, then a baseline SOF0 frame header.
  private func jpeg(width: Int, height: Int) -> Data {
    var bytes = Data(repeating: 0, count: 64)
    bytes[0] = 0xFF
    bytes[1] = 0xD8
    bytes[2] = 0xFF
    bytes[3] = 0xE0
    bytes[4] = 0x00
    bytes[5] = 0x10
    bytes.replaceSubrange(6..<10, with: "JFIF".data(using: .ascii)!)
    let offset = 2 + 2 + 0x10
    bytes[offset] = 0xFF
    bytes[offset + 1] = 0xC0
    bytes[offset + 2] = 0x00
    bytes[offset + 3] = 0x11
    bytes[offset + 4] = 8
    bytes[offset + 5] = UInt8(height >> 8)
    bytes[offset + 6] = UInt8(height & 0xFF)
    bytes[offset + 7] = UInt8(width >> 8)
    bytes[offset + 8] = UInt8(width & 0xFF)
    return bytes
  }

  func testSniffsPngAndJpegWithDimensions() {
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(png(width: 640, height: 480)),
      .ok(AttachmentImageHeader(format: .png, width: 640, height: 480))
    )
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(jpeg(width: 1024, height: 768)),
      .ok(AttachmentImageHeader(format: .jpeg, width: 1024, height: 768))
    )
  }

  func testRejectsMalformedHeaders() {
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(png(width: 10, height: 10).prefix(20)),
      .rejected(AttachmentLimits.errorMalformed)
    )
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(png(width: 0, height: 480)),
      .rejected(AttachmentLimits.errorMalformed)
    )
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(jpeg(width: 1024, height: 768).prefix(24)),
      .rejected(AttachmentLimits.errorMalformed)
    )
    let scanFirst = Data([0xFF, 0xD8, 0xFF, 0xDA, 0, 8, 1, 1])
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(scanFirst),
      .rejected(AttachmentLimits.errorMalformed)
    )
  }

  func testRejectsUnsupportedAndHeicFamiliesSeparately() {
    var gif = "GIF89a".data(using: .ascii)!
    gif.append(Data(repeating: 0, count: 16))
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(gif),
      .rejected(AttachmentLimits.errorUnsupported)
    )
    var heic = Data(repeating: 0, count: 16)
    heic[3] = 24
    heic.replaceSubrange(4..<12, with: "ftypheic".data(using: .ascii)!)
    XCTAssertEqual(
      AttachmentImageSniffer.sniff(heic),
      .rejected(AttachmentLimits.errorUnsupportedHeic)
    )
  }

  func testEnforcesByteAndPixelCeilings() {
    XCTAssertEqual(
      AttachmentDimensionPolicy.evaluate(
        header: AttachmentImageHeader(format: .jpeg, width: 800, height: 600),
        sourceBytes: AttachmentLimits.maxSourceBytes + 1
      ),
      .rejected(AttachmentLimits.errorInputTooLarge)
    )
    XCTAssertEqual(
      AttachmentDimensionPolicy.evaluate(
        header: AttachmentImageHeader(
          format: .png,
          width: AttachmentLimits.maxInputDimension + 1,
          height: 2
        ),
        sourceBytes: 1024
      ),
      .rejected(AttachmentLimits.errorDimensions)
    )
    XCTAssertEqual(
      AttachmentDimensionPolicy.evaluate(
        header: AttachmentImageHeader(format: .png, width: 16_384, height: 16_384),
        sourceBytes: 1024
      ),
      .rejected(AttachmentLimits.errorDimensions)
    )
    XCTAssertEqual(
      AttachmentDimensionPolicy.evaluate(
        header: AttachmentImageHeader(format: .jpeg, width: 4032, height: 3024),
        sourceBytes: 6_000_000
      ),
      .ok(needsScale: false)
    )
  }

  func testPinsAllEightExifOrientations() {
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(1), [])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(2), [.flipHorizontal])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(3), [.rotate180])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(4), [.rotate180, .flipHorizontal])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(5), [.rotate90, .flipHorizontal])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(6), [.rotate90])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(7), [.rotate270, .flipHorizontal])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(8), [.rotate270])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(0), [])
    XCTAssertEqual(AttachmentDimensionPolicy.orientationOps(9), [])
    XCTAssertFalse(AttachmentDimensionPolicy.swapsAxes(4))
    XCTAssertTrue(AttachmentDimensionPolicy.swapsAxes(5))
    XCTAssertTrue(AttachmentDimensionPolicy.swapsAxes(8))
  }

  func testValidatesAppOwnedFileNames() {
    XCTAssertTrue(AttachmentFileNames.isValid("att_0011223344556677.bin"))
    XCTAssertTrue(AttachmentFileNames.isStagingName("att_0011223344556677.bin"))
    XCTAssertTrue(AttachmentFileNames.isPreparedName("att_0011223344556677.png"))
    XCTAssertTrue(AttachmentFileNames.isPreparedName("att_0011223344556677.jpg"))
    XCTAssertFalse(AttachmentFileNames.isValid("../evil.png"))
    XCTAssertFalse(AttachmentFileNames.isValid("att_0011223344556677.gif"))
    XCTAssertFalse(AttachmentFileNames.isValid("att_0011223344556677.png/escape"))
    XCTAssertFalse(AttachmentFileNames.isStagingName("att_0011223344556677.png"))
  }

  func testSessionSnapshotCarriesOnlyDisplayMetadata() {
    let session = AttachmentSession(
      target: AttachmentTargetIdentity(
        terminalId: "poc-main",
        paneId: "%1",
        workspaceId: "@1"
      )
    )
    XCTAssertEqual(session.snapshot(previewUri: "")["status"] as? String, "idle")
    session.stagingFileName = "att_0011223344556677.bin"
    XCTAssertEqual(session.snapshot(previewUri: "")["status"] as? String, "staged")
    session.prepared = AttachmentPreparedImage(
      fileName: "att_8899aabbccddeeff.png",
      format: .png,
      width: 1080,
      height: 1920,
      byteCount: 123_456,
      sourceByteCount: 4_000_000
    )
    let snapshot = session.snapshot(
      previewUri: "file:///cache/attachments/att_8899aabbccddeeff.png"
    )
    XCTAssertEqual(snapshot["status"] as? String, "prepared")
    XCTAssertEqual(snapshot["fileId"] as? String, "att_8899aabbccddeeff.png")
    XCTAssertEqual(
      snapshot["previewUri"] as? String,
      "file:///cache/attachments/att_8899aabbccddeeff.png"
    )
    XCTAssertEqual(snapshot["width"] as? Int, 1080)
    XCTAssertEqual(
      (snapshot["target"] as? [String: Any])?["terminalId"] as? String,
      "poc-main"
    )
    // The core operation rides the session snapshot only while an op exists.
    XCTAssertTrue(snapshot["operation"] is NSNull)
    XCTAssertTrue(session.machine.recordBegin(
      attachmentId: 7,
      sizeBytes: 123_456,
      displayName: "att_8899aabbccddeeff.png"
    ))
    let withOp = session.snapshot(previewUri: "")["operation"] as? [String: Any]
    XCTAssertEqual(withOp?["phase"] as? String, "uploading")
    XCTAssertEqual(withOp?["attachmentId"] as? String, "7")
  }

  // Phase B: the adapter-side machine mirrors the contract's phase rules
  // while the Rust snapshot stays authoritative.

  private func operationRecord(
    attachmentId: UInt64 = 42,
    phase: Int32 = 1,
    flags: UInt32 = 0,
    bytesUploaded: UInt64 = 0,
    sizeBytes: UInt64 = 1_000,
    remotePath: String = "",
    displayName: String = "meeterm-x.png",
    errorCode: String = "",
    errorMessage: String = ""
  ) -> Data {
    var bytes = Data(count: AttachmentOperationCodec.recordSize)
    bytes.withUnsafeMutableBytes { raw in
      raw.storeBytes(of: phase.littleEndian, toByteOffset: 0, as: Int32.self)
      raw.storeBytes(of: flags.littleEndian, toByteOffset: 4, as: UInt32.self)
      raw.storeBytes(of: attachmentId.littleEndian, toByteOffset: 8, as: UInt64.self)
      raw.storeBytes(of: bytesUploaded.littleEndian, toByteOffset: 16, as: UInt64.self)
      raw.storeBytes(of: sizeBytes.littleEndian, toByteOffset: 24, as: UInt64.self)
      func field(_ value: String, _ lengthOffset: Int, _ dataOffset: Int, _ capacity: Int) {
        let utf8 = Array(value.utf8.prefix(capacity))
        raw.storeBytes(of: UInt16(utf8.count).littleEndian, toByteOffset: lengthOffset, as: UInt16.self)
        utf8.withUnsafeBytes { src in
          UnsafeMutableRawBufferPointer(rebasing: raw[dataOffset ..< dataOffset + src.count])
            .copyMemory(from: src)
        }
      }
      field(remotePath, 32, 34, 512)
      field(displayName, 546, 548, 128)
      field(errorCode, 676, 678, 64)
      field(errorMessage, 742, 744, 256)
    }
    return bytes
  }

  private func operation(
    _ id: UInt64,
    _ phase: AttachmentOpPhase,
    flags: UInt32 = 0
  ) -> AttachmentOperation {
    AttachmentOperation(
      attachmentId: id,
      phase: phase,
      flags: flags,
      bytesUploaded: 0,
      sizeBytes: 100,
      remotePath: "",
      displayName: "a.png",
      errorCode: "",
      errorMessage: ""
    )
  }

  func testDecodesCompleteSnapshotRecord() {
    let decoded = AttachmentOperationCodec.decode(
      operationRecord(
        attachmentId: 42,
        phase: 2,
        bytesUploaded: 1_000,
        remotePath: "/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png"
      )
    )
    XCTAssertNotNil(decoded)
    XCTAssertEqual(decoded?.attachmentId, 42)
    XCTAssertEqual(decoded?.phase, .uploaded)
    XCTAssertEqual(decoded?.phase.wireName, "uploaded")
    XCTAssertEqual(decoded?.bytesUploaded, 1_000)
    XCTAssertEqual(
      decoded?.remotePath,
      "/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png"
    )
    XCTAssertEqual(decoded?.insertUnconfirmed, false)
  }

  func testDecodesFlagsAndErrorFields() {
    let decoded = AttachmentOperationCodec.decode(
      operationRecord(phase: 3, flags: 3, errorCode: "input_unconfirmed", errorMessage: "unconfirmed")
    )
    XCTAssertEqual(decoded?.phase, .inserted)
    XCTAssertEqual(decoded?.insertUnconfirmed, true)
    XCTAssertEqual(decoded?.remoteRemoved, true)
    XCTAssertEqual(decoded?.errorCode, "input_unconfirmed")
    XCTAssertEqual(decoded?.errorMessage, "unconfirmed")
  }

  func testRejectsShortRecordsAndUnknownPhases() {
    XCTAssertNil(AttachmentOperationCodec.decode(operationRecord().prefix(512)))
    XCTAssertNil(AttachmentOperationCodec.decode(operationRecord(phase: 99)))
    XCTAssertNil(AttachmentOperationCodec.decode(operationRecord(phase: 6)))
  }

  func testRefusesSecondUploadWhileOperationIsLive() {
    let machine = AttachmentOpMachine()
    XCTAssertTrue(machine.canBeginUpload())
    XCTAssertTrue(machine.recordBegin(attachmentId: 7, sizeBytes: 100, displayName: "a.png"))
    XCTAssertFalse(machine.canBeginUpload())
    XCTAssertFalse(machine.recordBegin(attachmentId: 8, sizeBytes: 100, displayName: "b.png"))
    XCTAssertEqual(machine.operation?.attachmentId, 7)
  }

  func testAppliesSnapshotsOnlyForTheSameAttachmentId() {
    let machine = AttachmentOpMachine()
    machine.recordBegin(attachmentId: 7, sizeBytes: 100, displayName: "a.png")
    XCTAssertFalse(machine.applySnapshot(operation(9, .uploaded)))
    XCTAssertEqual(machine.operation?.phase, .uploading)
    XCTAssertTrue(machine.applySnapshot(operation(7, .uploaded)))
    XCTAssertEqual(machine.operation?.phase, .uploaded)
  }

  func testCancelledOperationIgnoresLateUploadSnapshots() {
    let machine = AttachmentOpMachine()
    machine.recordBegin(attachmentId: 7, sizeBytes: 100, displayName: "a.png")
    machine.markCancelled()
    // Late progress after a local cancel never resurrects the upload.
    XCTAssertTrue(machine.applySnapshot(operation(7, .uploading)))
    XCTAssertEqual(machine.operation?.phase, .cancelled)
    // A genuinely finished upload still surfaces its terminal state.
    XCTAssertTrue(machine.applySnapshot(operation(7, .uploaded)))
    XCTAssertEqual(machine.operation?.phase, .uploaded)
  }

  func testCapabilityGatesFollowTheContractPhases() {
    XCTAssertTrue(operation(1, .pending).canRetryUpload)
    XCTAssertTrue(operation(1, .failed).canRetryUpload)
    XCTAssertFalse(operation(1, .uploaded).canRetryUpload)
    XCTAssertTrue(operation(1, .uploaded).canInsert)
    XCTAssertFalse(operation(1, .inserted).canInsert)
    XCTAssertFalse(operation(1, .uploading).canInsert)
    XCTAssertTrue(operation(1, .pending).canCancel)
    XCTAssertTrue(operation(1, .uploading).canCancel)
    XCTAssertFalse(operation(1, .uploaded).canCancel)
    for phase in [AttachmentOpPhase.uploaded, .inserted, .failed, .cancelled] {
      XCTAssertTrue(operation(1, phase).canDeleteRemote, "delete allowed in \(phase)")
    }
    XCTAssertFalse(operation(1, .uploading).canDeleteRemote)
    XCTAssertFalse(operation(1, .pending).canDeleteRemote)
  }

  func testRemoteRemovalKeepsPhaseAndChangesCapabilities() {
    // `flags & 0x2` marks the verified remote deletion; the phase itself is
    // kept (an inserted line is not revoked, a failed op stays failed).
    let removedUploaded = operation(1, .uploaded, flags: 0x2)
    XCTAssertTrue(removedUploaded.remoteRemoved)
    XCTAssertEqual(removedUploaded.phase.wireName, "uploaded")
    XCTAssertFalse(removedUploaded.canInsert)
    XCTAssertFalse(removedUploaded.canDeleteRemote)
    // The core re-uploads the same operation for uploaded+removed.
    XCTAssertTrue(removedUploaded.canRetryUpload)

    let removedInserted = operation(1, .inserted, flags: 0x2)
    XCTAssertTrue(removedInserted.remoteRemoved)
    XCTAssertFalse(removedInserted.canInsert)
    XCTAssertFalse(removedInserted.canDeleteRemote)
    XCTAssertFalse(removedInserted.canRetryUpload)

    let removedFailed = operation(1, .failed, flags: 0x2)
    XCTAssertTrue(removedFailed.canRetryUpload)
    XCTAssertFalse(removedFailed.canDeleteRemote)
  }

  func testInFlightJobBlocksEveryAction() {
    // `flags & 0x4` (JOB_IN_FLIGHT) means a request is already running — no
    // second job may start on the operation until the flag drops.
    let busyUploaded = operation(1, .uploaded, flags: 0x4)
    XCTAssertTrue(busyUploaded.jobInFlight)
    XCTAssertFalse(busyUploaded.canInsert)
    XCTAssertFalse(busyUploaded.canDeleteRemote)
    XCTAssertFalse(busyUploaded.canRetryUpload)

    let busyFailed = operation(1, .failed, flags: 0x4)
    XCTAssertFalse(busyFailed.canRetryUpload)
    XCTAssertFalse(busyFailed.canDeleteRemote)

    // A stale reason from an earlier attempt never re-enables the gates.
    let busyWithError = operation(1, .pending, flags: 0x4)
    XCTAssertFalse(busyWithError.canRetryUpload)
  }

  func testClearDropsOperationForDiscardAndNewPick() {
    let machine = AttachmentOpMachine()
    machine.recordBegin(attachmentId: 7, sizeBytes: 100, displayName: "a.png")
    machine.clear()
    XCTAssertNil(machine.operation)
    XCTAssertTrue(machine.canBeginUpload())
  }

  func testInsertionPolicyHoldsWhileComposing() {
    XCTAssertEqual(
      AttachmentInsertionPolicy.insert(
        composing: true,
        hasActiveSession: true,
        hasPreparedImage: true,
        hasUploadedPath: true,
        sessionTerminalId: "poc-main",
        requestTerminalId: "poc-main"
      ),
      .heldComposing
    )
    // Composition protection precedes even missing-session handling.
    XCTAssertEqual(
      AttachmentInsertionPolicy.insert(
        composing: true,
        hasActiveSession: false,
        hasPreparedImage: false,
        hasUploadedPath: false,
        sessionTerminalId: nil,
        requestTerminalId: "poc-main"
      ),
      .heldComposing
    )
    XCTAssertEqual(
      AttachmentInsertionPolicy.insert(
        composing: false,
        hasActiveSession: false,
        hasPreparedImage: false,
        hasUploadedPath: false,
        sessionTerminalId: nil,
        requestTerminalId: "poc-main"
      ),
      .heldNoAttachment
    )
    XCTAssertEqual(
      AttachmentInsertionPolicy.insert(
        composing: false,
        hasActiveSession: true,
        hasPreparedImage: false,
        hasUploadedPath: false,
        sessionTerminalId: "other-terminal",
        requestTerminalId: "poc-main"
      ),
      .rejected(
        AttachmentLimits.errorDestinationChanged,
        "The attachment belongs to a different terminal."
      )
    )
    XCTAssertEqual(
      AttachmentInsertionPolicy.insert(
        composing: false,
        hasActiveSession: true,
        hasPreparedImage: true,
        hasUploadedPath: false,
        sessionTerminalId: "poc-main",
        requestTerminalId: "poc-main"
      ),
      .readyToInsert
    )
  }

  /// The marked-text signal that guards insertion must come from the real
  /// input view — not a policy stub — so the hold applies to actual IME state.
  @MainActor func testMarkedTextHoldsInsertionThroughRealInputView() {
    let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 64))
    let controller = UIViewController()
    window.rootViewController = controller
    window.makeKeyAndVisible()

    let inputView = TerminalInputView(frame: .zero, textContainer: nil)
    inputView.frame = CGRect(x: 0, y: 0, width: 320, height: 44)
    controller.view.addSubview(inputView)
    controller.view.layoutIfNeeded()
    defer {
      inputView.cancelCompositionForBinding()
      inputView.removeFromSuperview()
      window.isHidden = true
    }

    let guardRegistry = AttachmentCompositionGuard()
    guardRegistry.register(terminalId: "poc-main") {
      inputView.markedTextRange != nil
    }

    XCTAssertFalse(guardRegistry.isComposing(terminalId: "poc-main"))
    XCTAssertTrue(inputView.becomeFirstResponder())
    inputView.setMarkedText("あい", selectedRange: NSRange(location: 0, length: 0))
    XCTAssertNotNil(inputView.markedTextRange)
    XCTAssertTrue(guardRegistry.isComposing(terminalId: "poc-main"))
    XCTAssertEqual(
      AttachmentInsertionPolicy.insert(
        composing: guardRegistry.isComposing(terminalId: "poc-main"),
        hasActiveSession: true,
        hasPreparedImage: true,
        hasUploadedPath: false,
        sessionTerminalId: "poc-main",
        requestTerminalId: "poc-main"
      ),
      .heldComposing
    )
    // The composition itself must still be owned by the IME — the attachment
    // path holds rather than committing or clearing it.
    XCTAssertEqual(inputView.text(in: inputView.markedTextRange!), "あい")
  }

  /// FP-014: a cold-start reclaim must create the store/directory first —
  /// files left behind by a previous process are app-owned and swept.
  func testStartupReclaimSweepsPreviousProcessFiles() throws {
    let store = try AttachmentStore()
    let orphanStaging = store.directoryURL.appendingPathComponent("att_orphanstaging.bin")
    let orphanPrepared = store.directoryURL.appendingPathComponent("att_orphanprepared.png")
    let foreign = store.directoryURL.deletingLastPathComponent()
      .appendingPathComponent("att_foreign.bin")
    try Data("orphan".utf8).write(to: orphanStaging)
    try Data("orphan".utf8).write(to: orphanPrepared)
    try Data("not in the attachment dir".utf8).write(to: foreign)
    defer { try? FileManager.default.removeItem(at: foreign) }
    // A fresh store models the post-process-restart state — the controller's
    // startup reclaim (AttachmentController.reclaimStaleFiles) (re)creates it
    // and runs exactly this sweep with no live session keeping names.
    let freshStore = try AttachmentStore()
    freshStore.reclaimStale(keeping: [])
    XCTAssertFalse(FileManager.default.fileExists(atPath: orphanStaging.path))
    XCTAssertFalse(FileManager.default.fileExists(atPath: orphanPrepared.path))
    XCTAssertTrue(FileManager.default.fileExists(atPath: foreign.path),
      "reclaim must stay inside the app-owned attachments directory")
  }

  override func tearDown() {
    super.tearDown()
    if !recordedIssue {
      appendValidation("result=passed")
    }
  }
}

private extension UInt32 {
  var bigEndianData: Data {
    var value = self.bigEndian
    return Data(bytes: &value, count: 4)
  }
}
