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

  override func setUp() async throws {
    try await super.setUp()
    await MainActor.run {
      configureUI()
    }
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
    drainMainRunLoop()
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

  @MainActor func testMultilinePasteCallsOnPasteOnceWithoutCommit() {
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
    wait(for: [pasted], timeout: 2)

    XCTAssertEqual(pastedValues, [expected])
    XCTAssertEqual(commitCount, 0)
  }

  @MainActor func testPendingPasteIsDroppedAfterCancelComposition() {
    var pastedCount = 0
    var commitCount = 0
    let provider = DelayedTextProvider(testCase: self)

    inputView.onCommit = { _ in commitCount += 1 }
    inputView.onPaste = { _ in pastedCount += 1 }
    inputView.paste(itemProviders: [provider])
    wait(for: [provider.loadStarted], timeout: 2)

    inputView.cancelCompositionForBinding()
    // Rebind the same native input view before the old provider completes.
    // The view is live and first responder again, so this assertion exercises
    // the generation guard rather than only the first-responder guard.
    XCTAssertTrue(inputView.becomeFirstResponder(), "The input view could not be rebound.")
    provider.finish(with: "dropped after cancel\n")
    drainMainRunLoop()

    XCTAssertEqual(pastedCount, 0)
    XCTAssertEqual(commitCount, 0)
  }

  @MainActor func testPendingPasteIsDroppedWhenInputLeavesWindow() {
    var pastedCount = 0
    var commitCount = 0
    let provider = DelayedTextProvider(testCase: self)

    inputView.onCommit = { _ in commitCount += 1 }
    inputView.onPaste = { _ in pastedCount += 1 }
    inputView.paste(itemProviders: [provider])
    wait(for: [provider.loadStarted], timeout: 2)

    inputView.removeFromSuperview()
    provider.finish(with: "dropped after unmount\n")
    drainMainRunLoop()

    XCTAssertEqual(pastedCount, 0)
    XCTAssertEqual(commitCount, 0)
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

  @MainActor private func drainMainRunLoop() {
    // A main-queue barrier drains the provider completion dispatch without
    // relying on an arbitrary sleep. XCTest's wait pumps the main run loop.
    let drained = ExpectationBox(expectation(description: "main queue drained"))
    DispatchQueue.main.async {
      drained.fulfill()
    }
    wait(for: [drained.expectation], timeout: 1)
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
