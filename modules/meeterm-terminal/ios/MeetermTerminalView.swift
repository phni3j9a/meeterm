import ExpoModulesCore
import Metal
import MetalKit
import UIKit

final class MeetermTerminalView: ExpoView {
  private static let defaultTerminalId = "poc-main"
  private static let defaultColumns = 80
  private static let defaultRows = 24

  let onNativeReady = EventDispatcher()
  let onMetrics = EventDispatcher()

  private let renderingView: UIView
  private let renderer: any TerminalFrameRendering
  private let terminalInputView = TerminalInputView(frame: .zero, textContainer: nil)
  private var terminalId = defaultTerminalId
  private var terminalHandle: UInt64 = 0
  private var keyboardOcclusion: CGFloat = 0
  private var lastColumns = 0
  private var lastRows = 0
  private var lastTerminalRevision: UInt64 = .max
  private var revisionTimer: Timer?

  required init(appContext: AppContext? = nil) {
    let selectedView: UIView
    let selectedRenderer: any TerminalFrameRendering
    if let device = MTLCreateSystemDefaultDevice() {
      let metalView = MTKView(frame: .zero, device: device)
      metalView.colorPixelFormat = .bgra8Unorm
      if let metalRenderer = try? TerminalRenderer(
        device: device,
        colorPixelFormat: metalView.colorPixelFormat
      ) {
        metalView.clearColor = MTLClearColor(
          red: Double(9) / 255,
          green: Double(11) / 255,
          blue: Double(15) / 255,
          alpha: 1
        )
        metalView.framebufferOnly = true
        metalView.autoResizeDrawable = true
        metalView.isPaused = true
        metalView.enableSetNeedsDisplay = true
        metalView.delegate = metalRenderer
        metalRenderer.view = metalView
        selectedView = metalView
        selectedRenderer = metalRenderer
      } else {
        #if targetEnvironment(simulator)
        let softwareView = TerminalSoftwareView(frame: .zero)
        selectedView = softwareView
        selectedRenderer = softwareView
        #else
        fatalError("Unable to initialize the meeterm Metal renderer")
        #endif
      }
    } else {
      #if targetEnvironment(simulator)
      let softwareView = TerminalSoftwareView(frame: .zero)
      selectedView = softwareView
      selectedRenderer = softwareView
      #else
      fatalError("Metal is required by MeetermTerminalView on physical devices")
      #endif
    }
    self.renderingView = selectedView
    self.renderer = selectedRenderer
    super.init(appContext: appContext)

    backgroundColor = UIColor(
      red: CGFloat(9) / 255,
      green: CGFloat(11) / 255,
      blue: CGFloat(15) / 255,
      alpha: 1
    )
    clipsToBounds = true
    isAccessibilityElement = true
    accessibilityLabel = "Terminal"

    renderingView.backgroundColor = backgroundColor
    addSubview(renderingView)

    terminalInputView.onPreeditChanged = { [weak self] value in
      self?.renderer.setPreedit(value)
    }
    terminalInputView.onCommit = { [weak self] text in
      self?.commit(text)
    }
    terminalInputView.onSpecialKey = { [weak self] key in
      self?.send(key)
    }
    addSubview(terminalInputView)

    let focusGesture = UITapGestureRecognizer(target: self, action: #selector(focusTerminal))
    focusGesture.cancelsTouchesInView = false
    addGestureRecognizer(focusGesture)

    NotificationCenter.default.addObserver(
      self,
      selector: #selector(keyboardFrameChanged(_:)),
      name: UIResponder.keyboardWillChangeFrameNotification,
      object: nil
    )
    NotificationCenter.default.addObserver(
      self,
      selector: #selector(keyboardWillHide(_:)),
      name: UIResponder.keyboardWillHideNotification,
      object: nil
    )
    NotificationCenter.default.addObserver(
      self,
      selector: #selector(applicationDidBecomeActive),
      name: UIApplication.didBecomeActiveNotification,
      object: nil
    )
    NotificationCenter.default.addObserver(
      self,
      selector: #selector(applicationWillResignActive),
      name: UIApplication.willResignActiveNotification,
      object: nil
    )
  }

  deinit {
    stopRevisionPolling()
    NotificationCenter.default.removeObserver(self)
  }

  func bindTerminal(_ requestedId: String?) {
    let normalized = requestedId?.trimmingCharacters(in: .whitespacesAndNewlines)
    let nextId = normalized.flatMap { $0.isEmpty ? nil : $0 } ?? Self.defaultTerminalId
    if nextId == terminalId, terminalHandle != 0 {
      return
    }

    terminalInputView.cancelCompositionForBinding()
    renderer.attachTerminal(0)
    terminalId = nextId
    lastColumns = 0
    lastRows = 0
    terminalHandle = TerminalRegistry.acquire(
      terminalId: nextId,
      columns: Self.defaultColumns,
      rows: Self.defaultRows
    )
    guard terminalHandle != 0 else {
      renderer.requestFrame()
      return
    }

    renderer.attachTerminal(terminalHandle)
    lastTerminalRevision = MeetermCore.terminalRevision(terminalId: terminalHandle)
    NSLog("MEETERM_SMOKE_NATIVE_READY")
    onNativeReady([
      "terminalId": terminalId,
      "native": true
    ])
    setNeedsLayout()
    renderer.requestFrame()
  }

  override func didMoveToWindow() {
    super.didMoveToWindow()
    if window != nil {
      if terminalHandle == 0 {
        bindTerminal(terminalId)
      }
      setNeedsLayout()
      renderer.requestFrame()
      if isNativeViewVisible {
        startRevisionPolling()
      } else {
        stopRevisionPolling()
      }
    } else {
      stopRevisionPolling()
      terminalInputView.resignFirstResponder()
    }
  }

  private func startRevisionPolling() {
    stopRevisionPolling()
    guard isNativeViewVisible, terminalHandle != 0 else {
      return
    }
    revisionTimer = Timer(timeInterval: 0.033, repeats: true) { [weak self] _ in
      self?.pollTerminalRevision()
    }
    if let revisionTimer {
      RunLoop.main.add(revisionTimer, forMode: .common)
    }
  }

  private func stopRevisionPolling() {
    revisionTimer?.invalidate()
    revisionTimer = nil
  }

  private func pollTerminalRevision() {
    guard isNativeViewVisible, terminalHandle != 0 else {
      stopRevisionPolling()
      return
    }
    let revision = MeetermCore.terminalRevision(terminalId: terminalHandle)
    if revision != lastTerminalRevision {
      lastTerminalRevision = revision
      // The renderer pulls a snapshot only after this native revision check.
      // No terminal bytes or cells cross the JavaScript boundary.
      renderer.requestFrame()
    }
  }

  private var isNativeViewVisible: Bool {
    guard window != nil, !isHidden, alpha > 0, window?.isHidden == false else {
      return false
    }
    if let activationState = window?.windowScene?.activationState,
       activationState == .background || activationState == .unattached {
      return false
    }
    return true
  }

  override func layoutSubviews() {
    super.layoutSubviews()

    var terminalFrame = bounds.inset(by: safeAreaInsets)
    if keyboardOcclusion > 0 {
      let visibleBottom = min(terminalFrame.maxY, bounds.maxY - keyboardOcclusion)
      terminalFrame.size.height = max(0, visibleBottom - terminalFrame.minY)
    }
    renderingView.frame = terminalFrame
    terminalInputView.frame = CGRect(
      x: terminalFrame.minX,
      y: terminalFrame.minY,
      width: 1,
      height: 1
    )
    reconcileResize(for: terminalFrame.size)
  }

  override func safeAreaInsetsDidChange() {
    super.safeAreaInsetsDidChange()
    setNeedsLayout()
  }

  override var canBecomeFirstResponder: Bool {
    true
  }

  @discardableResult
  override func becomeFirstResponder() -> Bool {
    terminalInputView.becomeFirstResponder()
  }

  @discardableResult
  override func resignFirstResponder() -> Bool {
    terminalInputView.resignFirstResponder()
  }

  @objc private func focusTerminal() {
    terminalInputView.becomeFirstResponder()
  }

  @objc private func keyboardFrameChanged(_ notification: Notification) {
    guard window != nil,
          let screenFrame = notification.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect else {
      return
    }
    let localFrame = convert(screenFrame, from: nil)
    let intersection = bounds.intersection(localFrame)
    keyboardOcclusion = intersection.isNull ? 0 : max(0, intersection.height)
    setNeedsLayout()
  }

  @objc private func keyboardWillHide(_: Notification) {
    keyboardOcclusion = 0
    setNeedsLayout()
  }

  @objc private func applicationDidBecomeActive() {
    guard window != nil else {
      return
    }
    renderer.requestFrame()
    // Scene activation is updated alongside this notification. Starting on
    // the next main-queue turn avoids treating the transition as background.
    DispatchQueue.main.async { [weak self] in
      self?.startRevisionPolling()
    }
  }

  @objc private func applicationWillResignActive() {
    stopRevisionPolling()
  }

  private func reconcileResize(for size: CGSize) {
    guard terminalHandle != 0, size.width > 0, size.height > 0 else {
      return
    }

    let columns = max(
      2,
      Int((size.width / TerminalRenderer.cellWidthPoints).rounded(.down))
    )
    let rows = max(
      1,
      Int((size.height / TerminalRenderer.cellHeightPoints).rounded(.down))
    )
    guard columns != lastColumns || rows != lastRows else {
      return
    }
    guard MeetermCore.resize(terminalId: terminalHandle, columns: columns, rows: rows) else {
      return
    }

    lastColumns = columns
    lastRows = rows
    let scale = window?.screen.scale ?? contentScaleFactor
    onMetrics([
      "terminalId": terminalId,
      "columns": columns,
      "rows": rows,
      "cellWidthPx": Int((TerminalRenderer.cellWidthPoints * scale).rounded()),
      "cellHeightPx": Int((TerminalRenderer.cellHeightPoints * scale).rounded())
    ])
    renderer.requestFrame()
  }

  private func commit(_ text: String) {
    guard terminalHandle != 0 else {
      return
    }
    if MeetermCore.commit(terminalId: terminalHandle, text: text) > 0 {
      renderer.requestFrame()
    }
  }

  private func send(_ key: TerminalSpecialKey) {
    guard terminalHandle != 0 else {
      return
    }
    if MeetermCore.send(terminalId: terminalHandle, key: key) {
      renderer.requestFrame()
    }
  }
}
