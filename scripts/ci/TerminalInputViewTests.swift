import Foundation
import UIKit
import UniformTypeIdentifiers
import XCTest

/// Focused native input coverage for the paste path used by the real iOS UI
/// smoke. These tests run in the disposable UI-test target, alongside the
/// production TerminalInputView source, so the async provider path is tested
/// without a production-only test hook.
final class TerminalInputViewTests: XCTestCase {
  private var hostWindow: UIWindow!
  private var hostViewController: UIViewController!
  private var inputView: TerminalInputView!
  private var recordedIssue = false

  override func record(_ issue: XCTIssue) {
    recordedIssue = true
    appendValidation("result=failed source_line=\(issue.sourceCodeContext.location?.lineNumber ?? 0)")
    super.record(issue)
  }

  private func appendValidation(_ line: String) {
    guard let directory = ProcessInfo.processInfo.environment["MEETERM_IOS_ARTIFACT_DIR"] else { return }
    let path = URL(fileURLWithPath: directory).appendingPathComponent("ios-native-input-validation.txt")
    let data = Data((line + "\n").utf8)
    if let handle = try? FileHandle(forWritingTo: path) {
      handle.seekToEndOfFile()
      handle.write(data)
      try? handle.close()
    } else {
      try? data.write(to: path, options: .atomic)
    }
  }

  override func setUp() async throws {
    try await super.setUp()
    await MainActor.run {
      configureUI()
    }
    await drainMainRunLoop()
  }

  @MainActor private func configureUI() {
    hostWindow = makeHostWindow()
    hostViewController = UIViewController()
    hostWindow.rootViewController = hostViewController
    hostWindow.makeKeyAndVisible()

    inputView = TerminalInputView(frame: .zero, textContainer: nil)
    inputView.translatesAutoresizingMaskIntoConstraints = false
    hostViewController.view.addSubview(inputView)
    NSLayoutConstraint.activate([
      inputView.leadingAnchor.constraint(equalTo: hostViewController.view.leadingAnchor),
      inputView.trailingAnchor.constraint(equalTo: hostViewController.view.trailingAnchor),
      inputView.topAnchor.constraint(equalTo: hostViewController.view.topAnchor),
      inputView.heightAnchor.constraint(equalToConstant: 44)
    ])
    hostViewController.view.layoutIfNeeded()
    XCTAssertTrue(inputView.becomeFirstResponder(), "The native input view did not become first responder.")
  }

  override func tearDown() async throws {
    await MainActor.run {
      tearDownUI()
    }
    try await super.tearDown()
  }

  @MainActor private func tearDownUI() {
    inputView?.cancelCompositionForBinding()
    inputView?.removeFromSuperview()
    hostWindow?.resignKey()
    hostWindow?.isHidden = true
    inputView = nil
    hostViewController = nil
    hostWindow = nil
  }

  @MainActor func testMultilinePasteCallsOnPasteOnceWithoutCommit() async {
    let expected = "printf 'line one\nline two\n'"
    var pastedValues: [String] = []
    var commitCount = 0
    let pasted = expectation(description: "the multiline paste is delivered")

    inputView.onCommit = { _ in commitCount += 1 }
    inputView.onPaste = { value in
      pastedValues.append(value)
      pasted.fulfill()
    }

    let provider = plainTextProvider(expected)
    XCTAssertTrue(provider.canLoadObject(ofClass: String.self))
    inputView.paste(itemProviders: [provider])
    await fulfillment(of: [pasted], timeout: 2)

    XCTAssertEqual(pastedValues, [expected])
    XCTAssertEqual(commitCount, 0)
    if !recordedIssue { appendValidation("case=multiline result=passed") }
  }

  @MainActor func testPendingPasteIsDroppedAfterCancelComposition() async {
    var pastedCount = 0
    var commitCount = 0
    let provider = DelayedTextProvider(testCase: self)

    inputView.onCommit = { _ in commitCount += 1 }
    inputView.onPaste = { _ in pastedCount += 1 }
    inputView.paste(itemProviders: [provider])
    await fulfillment(of: [provider.loadStarted], timeout: 2)

    inputView.cancelCompositionForBinding()
    // Rebind the same native input view before the old provider completes.
    // The view is live and first responder again, so this assertion exercises
    // the generation guard rather than only the first-responder guard.
    XCTAssertTrue(inputView.becomeFirstResponder(), "The input view could not be rebound.")
    provider.finish(with: "dropped after cancel\n")
    await drainMainRunLoop()

    XCTAssertEqual(pastedCount, 0)
    XCTAssertEqual(commitCount, 0)
    if !recordedIssue { appendValidation("case=rebind result=passed") }
  }

  @MainActor func testPendingPasteIsDroppedWhenInputLeavesWindow() async {
    var pastedCount = 0
    var commitCount = 0
    let provider = DelayedTextProvider(testCase: self)

    inputView.onCommit = { _ in commitCount += 1 }
    inputView.onPaste = { _ in pastedCount += 1 }
    inputView.paste(itemProviders: [provider])
    await fulfillment(of: [provider.loadStarted], timeout: 2)

    inputView.removeFromSuperview()
    provider.finish(with: "dropped after unmount\n")
    await drainMainRunLoop()

    XCTAssertEqual(pastedCount, 0)
    XCTAssertEqual(commitCount, 0)
    if !recordedIssue { appendValidation("case=unmount result=passed") }
  }

  @MainActor func testControlModifierAppliesToOneCommitAndIsCancelledOnRebind() {
    var modified: [(String, UInt32)] = []
    var committed: [String] = []
    inputView.onModifiedCommit = { modified.append(($0, $1)) }
    inputView.onCommit = { committed.append($0) }
    guard let control = findButton(title: "Ctrl", in: inputView.inputAccessoryView) else {
      XCTFail("The native Control key is missing."); return
    }
    control.sendActions(for: .touchUpInside)
    inputView.insertText("r")
    inputView.insertText("a")
    XCTAssertEqual(modified.count, 1)
    XCTAssertEqual(modified.first?.0, "r")
    XCTAssertEqual(modified.first?.1, 1)
    XCTAssertEqual(committed, ["a"])
    control.sendActions(for: .touchUpInside)
    inputView.cancelCompositionForBinding()
    inputView.insertText("b")
    XCTAssertEqual(committed, ["a", "b"])
    XCTAssertEqual(modified.count, 1)
    if !recordedIssue { appendValidation("case=control_one_shot result=passed") }
  }

  @MainActor func testHardwareControlUsesModifiedInputWithoutDoubleCommit() {
    var modified: [(String, UInt32)] = []
    var committed: [String] = []
    inputView.onModifiedCommit = { modified.append(($0, $1)) }
    inputView.onCommit = { committed.append($0) }
    guard let command = inputView.keyCommands?.first(where: { $0.input == "d" && $0.modifierFlags == .control }) else {
      XCTFail("The hardware Control-D command is missing."); return
    }
    _ = inputView.perform(command.action, with: command)
    XCTAssertEqual(modified.count, 1)
    XCTAssertEqual(modified.first?.0, "d")
    XCTAssertEqual(modified.first?.1, 1)
    XCTAssertTrue(committed.isEmpty)
    if !recordedIssue { appendValidation("case=hardware_control result=passed") }
  }

  @MainActor func testHardwareShiftCombinationsAreRegisteredAndCommitOnce() {
    var modified: [(String, UInt32)] = []
    var committed: [String] = []
    inputView.onModifiedCommit = { modified.append(($0, $1)) }
    inputView.onCommit = { committed.append($0) }
    let combinations: [(UIKeyModifierFlags, UInt32)] = [
      ([.control, .shift], 5), ([.alternate, .shift], 6),
      ([.control, .alternate, .shift], 7)
    ]
    for (flags, expected) in combinations {
      guard let command = inputView.keyCommands?.first(where: {
        $0.input == "C" && $0.modifierFlags == flags
      }) else { XCTFail("A hardware Shift combination is missing."); return }
      let previous = modified.count
      _ = inputView.perform(command.action, with: command)
      XCTAssertEqual(modified.count, previous + 1)
      XCTAssertEqual(modified.last?.0, "C")
      XCTAssertEqual(modified.last?.1, expected)
    }
    XCTAssertTrue(committed.isEmpty)
    if !recordedIssue { appendValidation("case=hardware_shift_combinations result=passed") }
  }

  @MainActor func testMarkedCompositionIsLocalAndCommitsOnce() {
    var committed: [String] = []
    var preedit: [String] = []
    inputView.onCommit = { committed.append($0) }
    inputView.onPreeditChanged = { preedit.append($0) }
    inputView.setMarkedText("きょう", selectedRange: NSRange(location: 3, length: 0))
    XCTAssertTrue(committed.isEmpty)
    XCTAssertEqual(preedit.last, "きょう")
    inputView.insertText("今日")
    inputView.unmarkText()
    XCTAssertEqual(committed, ["今日"])
    XCTAssertEqual(preedit.last, "")
    if !recordedIssue { appendValidation("case=marked_commit result=passed") }
  }

  @MainActor private func findButton(title: String, in view: UIView?) -> UIButton? {
    if let button = view as? UIButton, button.configuration?.title == title { return button }
    for child in view?.subviews ?? [] {
      if let button = findButton(title: title, in: child) { return button }
    }
    return nil
  }

  @MainActor private func makeHostWindow() -> UIWindow {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    if let scene = scenes.first(where: {
      $0.activationState == .foregroundActive || $0.activationState == .foregroundInactive
    }) {
      let window = UIWindow(windowScene: scene)
      window.frame = scene.coordinateSpace.bounds
      return window
    }
    return UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 844))
  }

  private func drainMainRunLoop() async {
    // An async XCTest waiter yields the main actor while the provider
    // completion dispatch is delivered. This avoids blocking the main queue
    // with wait(for:) and does not rely on an arbitrary sleep.
    let drained = ExpectationBox(expectation(description: "main queue drained"))
    DispatchQueue.main.async {
      drained.fulfill()
    }
    await fulfillment(of: [drained.expectation], timeout: 1)
  }

  private func plainTextProvider(_ value: String) -> NSItemProvider {
    let provider = NSItemProvider()
    provider.registerDataRepresentation(for: UTType.plainText, visibility: .all) { completion in
      completion(Data(value.utf8), nil)
      return nil
    }
    return provider
  }
}

private final class DelayedTextProvider: NSItemProvider {
  let loadStarted: XCTestExpectation

  private let completionBox = ReadingCompletionBox()

  init(testCase: XCTestCase) {
    loadStarted = testCase.expectation(description: "the delayed text provider started loading")
    super.init()
  }

  override func canLoadObject(ofClass _: NSItemProviderReading.Type) -> Bool {
    true
  }

  override func loadObject(
    ofClass _: NSItemProviderReading.Type,
    completionHandler: @escaping (NSItemProviderReading?, Error?) -> Void
  ) -> Progress {
    completionBox.store(completionHandler)
    loadStarted.fulfill()
    return Progress(totalUnitCount: 1)
  }

  func finish(with value: String) {
    completionBox.finish(value as NSString)
  }
}

private final class ExpectationBox: @unchecked Sendable {
  let expectation: XCTestExpectation

  init(_ expectation: XCTestExpectation) {
    self.expectation = expectation
  }

  func fulfill() {
    expectation.fulfill()
  }
}

private final class ReadingCompletionBox: @unchecked Sendable {
  private let lock = NSLock()
  private var completion: ((NSItemProviderReading?, Error?) -> Void)?

  func store(_ completion: @escaping (NSItemProviderReading?, Error?) -> Void) {
    lock.lock()
    defer { lock.unlock() }
    self.completion = completion
  }

  func finish(_ object: NSItemProviderReading?) {
    lock.lock()
    let completion = self.completion
    lock.unlock()
    completion?(object, nil)
  }
}
