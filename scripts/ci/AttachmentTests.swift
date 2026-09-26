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
        workspaceId: "@1",
        backend: "tmux",
        runtime: "meeterm",
        host: "host",
        port: 22
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
    session.remotePath = "/tmp/meeterm-attach/att.png"
    XCTAssertEqual(session.snapshot(previewUri: "")["status"] as? String, "uploaded")
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
        AttachmentLimits.errorTarget,
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
    XCTAssertEqual(inputView.markedText(in: inputView.markedTextRange!), "あい")
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
