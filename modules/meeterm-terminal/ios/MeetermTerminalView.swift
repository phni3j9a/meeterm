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
  private var interactionMode = "live"
  private var keyboardOcclusion: CGFloat = 0
  private var lastColumns = 0
  private var lastRows = 0
  private var lastOperationEpoch: UInt64?
  private var lastTerminalRevision: UInt64 = .max
  private var revisionTimer: Timer?
  private var fontSize: CGFloat = 15
  private var lightTheme = false
  private var selectionStart: (row: Int, column: Int)?
  private var selectionEnd: (row: Int, column: Int)?
  private let selectionBar = UIStackView()
  private let startHandle = UIButton(type: .system)
  private let endHandle = UIButton(type: .system)
  private var scrollGestureDelegate: TerminalScrollGestureDelegate?
  private let observesInputLifecycle = ProcessInfo.processInfo.arguments.contains("-meeterm-ui-observation")

  private var cellSize: CGSize {
    TerminalRenderer.cellSize(fontSize: fontSize, scale: max(1, window?.screen.scale ?? contentScaleFactor))
  }

  private var isCachedReadOnly: Bool { interactionMode == "cachedReadOnly" }

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
          red: Double(36) / 255,
          green: Double(33) / 255,
          blue: Double(29) / 255,
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
      red: CGFloat(36) / 255,
      green: CGFloat(33) / 255,
      blue: CGFloat(29) / 255,
      alpha: 1
    )
    clipsToBounds = true
    isAccessibilityElement = false
    renderingView.isAccessibilityElement = true
    renderingView.accessibilityLabel = "Terminal"
    renderingView.accessibilityIdentifier = "native-terminal-surface"
    updateSmokeNativeHandleObservation()

    renderingView.backgroundColor = backgroundColor
    addSubview(renderingView)

    terminalInputView.onPreeditChanged = { [weak self] value in
      self?.renderer.setPreedit(value)
    }
    terminalInputView.operationEpochProvider = { [weak self] in
      self?.currentOperationEpoch()
    }
    terminalInputView.onCommitAtEpoch = { [weak self] text, epoch in
      self?.commit(text, epoch: epoch)
    }
    terminalInputView.onPasteAtEpoch = { [weak self] text, epoch in
      guard let self, !self.isCachedReadOnly, self.terminalHandle != 0 else { return }
      let accepted = MeetermCore.pasteAtEpoch(
        terminalId: self.terminalHandle,
        expectedEpoch: epoch,
        text: text
      )
      if self.observesInputLifecycle {
        NSLog("MEETERM_SMOKE_PASTE_RESULT accepted=%d", accepted ? 1 : 0)
      }
      if accepted {
        // Selection is local state and is cleared only after Rust accepted the
        // epoch-checked remote operation.
        self.clearSelection()
        self.renderer.requestFrame()
      }
    }
    terminalInputView.onSpecialKeyAtEpoch = { [weak self] key, epoch in
      self?.send(key, epoch: epoch)
    }
    terminalInputView.onCopySelection = { [weak self] in self?.copySelection() }
    terminalInputView.hasTerminalSelection = { [weak self] in self?.selectionStart != nil }
    terminalInputView.onModifiedCommitAtEpoch = { [weak self] text, modifiers, epoch in
      self?.commitModified(text, modifiers: modifiers, epoch: epoch)
    }
    terminalInputView.onModifiedSpecialKeyAtEpoch = { [weak self] key, modifiers, epoch in
      self?.sendModified(key, modifiers: modifiers, epoch: epoch)
    }
    addSubview(terminalInputView)
    configureSelectionControls()

    let focusGesture = UITapGestureRecognizer(target: self, action: #selector(focusTerminal))
    focusGesture.cancelsTouchesInView = false
    renderingView.addGestureRecognizer(focusGesture)
    let scrollGesture = UIPanGestureRecognizer(target: self, action: #selector(scrollTerminal(_:)))
    scrollGesture.maximumNumberOfTouches = 1
    let scrollDelegate = TerminalScrollGestureDelegate { [weak self] in self?.selectionStart != nil }
    scrollGestureDelegate = scrollDelegate
    scrollGesture.delegate = scrollDelegate
    renderingView.addGestureRecognizer(scrollGesture)
    focusGesture.require(toFail: scrollGesture)
    let selectionGesture = UILongPressGestureRecognizer(target: self, action: #selector(selectTerminal(_:)))
    selectionGesture.minimumPressDuration = 0.45
    renderingView.addGestureRecognizer(selectionGesture)
    scrollGesture.require(toFail: selectionGesture)
    focusGesture.require(toFail: selectionGesture)

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
    AttachmentCompositionGuard.shared.unregister(terminalId: terminalId)
    NotificationCenter.default.removeObserver(self)
  }

  func bindTerminal(_ requestedId: String?) {
    let normalized = requestedId?.trimmingCharacters(in: .whitespacesAndNewlines)
    let nextId = normalized.flatMap { $0.isEmpty ? nil : $0 } ?? Self.defaultTerminalId
    if nextId == terminalId, terminalHandle != 0 {
      return
    }

    clearSelection()
    AttachmentCompositionGuard.shared.unregister(terminalId: terminalId)
    terminalInputView.cancelCompositionForBinding()
    renderer.attachTerminal(0)
    terminalId = nextId
    // Attachment insertion asks this provider whether the IME still owns an
    // uncommitted marked-text composition for this terminal.
    AttachmentCompositionGuard.shared.register(terminalId: nextId) { [weak self] in
      guard let view = self else { return false }
      return view.terminalInputView.markedTextRange != nil
    }
    lastColumns = 0
    lastRows = 0
    terminalHandle = TerminalRegistry.acquire(
      terminalId: nextId,
      columns: Self.defaultColumns,
      rows: Self.defaultRows
    )
    updateSmokeNativeHandleObservation()
    guard terminalHandle != 0 else {
      renderer.requestFrame()
      return
    }

    renderer.attachTerminal(terminalHandle)
    applyAppearance()
    lastTerminalRevision = MeetermCore.terminalRevision(terminalId: terminalHandle)
    lastOperationEpoch = MeetermCore.operationEpoch(terminalId: terminalHandle)
    terminalInputView.operationEpochDidChange(lastOperationEpoch)
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
      // Leaving a screen is not a text commit. Cancel preedit while retaining
      // the Rust-owned pane so UIKit cannot submit it during responder teardown.
      terminalInputView.cancelCompositionForBinding()
      clearSelection()
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
    if synchronizeOperationEpoch() {
      // A rejected in-flight resize is intentionally not queued. Re-entering
      // layout with a fresh epoch sends only the current measured dimensions.
      renderer.requestFrame()
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

  private func updateSmokeNativeHandleObservation() {
    // This is an opaque, test-only accessibility value. It is enabled only
    // by the smoke launch argument and contains no terminal bytes, cells, or
    // remote identifiers. The UI test compares it across the retained
    // cached/live transition while the normal app exposes no handle.
    if observesInputLifecycle, terminalHandle != 0 {
      renderingView.accessibilityValue = "native-handle-\(terminalHandle)"
    } else {
      renderingView.accessibilityValue = nil
    }
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
    selectionBar.frame = CGRect(x: terminalFrame.midX - 92, y: max(terminalFrame.minY, terminalFrame.maxY - 52), width: 184, height: 44)
    positionSelectionHandles()
  }

  func setFontSize(_ size: Double) {
    guard size.isFinite, size >= 10, size <= 24, fontSize != CGFloat(size) else { return }
    clearSelection()
    fontSize = CGFloat(size)
    applyAppearance()
    setNeedsLayout()
  }

  func setTheme(_ theme: String) {
    let next = theme == "light"
    guard next != lightTheme else { return }
    lightTheme = next
    applyAppearance()
  }

  /// Keep the same native terminal handle/surface while changing whether
  /// remote-affecting input is allowed. Returning to live only prepares the
  /// next native input session; it does not focus the keyboard or grant Rust
  /// authority.
  func setInteractionMode(_ mode: String) {
    let nextMode = mode == "cachedReadOnly" ? "cachedReadOnly" : "live"
    guard nextMode != interactionMode else {
      if nextMode == "live" { synchronizeOperationEpoch() }
      return
    }
    interactionMode = nextMode
    terminalInputView.setInteractionMode(nextMode)
    if nextMode == "live" { synchronizeOperationEpoch() }
    lastColumns = 0
    lastRows = 0
    if isCachedReadOnly {
      renderingView.accessibilityLabel = "Terminal, cached output, read only"
      renderingView.accessibilityHint = "Input is paused until recovery finishes."
    } else {
      renderingView.accessibilityLabel = "Terminal"
      renderingView.accessibilityHint = nil
    }
    setNeedsLayout()
    renderer.requestFrame()
  }

  func setScrollbackLines(_ lines: Int) {
    guard (1000...50000).contains(lines) else { return }
    MeetermCore.setScrollbackLimit(lines)
  }

  private func applyAppearance() {
    let color = lightTheme ? UIColor(red: 251/255, green: 247/255, blue: 239/255, alpha: 1)
      : UIColor(red: 36/255, green: 33/255, blue: 29/255, alpha: 1)
    backgroundColor = color
    renderingView.backgroundColor = color
    terminalInputView.keyboardAppearance = lightTheme ? .light : .dark
    renderer.setAppearance(fontSize: fontSize, light: lightTheme)
    if terminalHandle != 0 { MeetermCore.setTheme(terminalId: terminalHandle, light: lightTheme) }
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
    guard !isCachedReadOnly else { return false }
    return terminalInputView.becomeFirstResponder()
  }

  @discardableResult
  override func resignFirstResponder() -> Bool {
    terminalInputView.resignFirstResponder()
  }

  @objc private func focusTerminal() {
    guard !isCachedReadOnly else { return }
    if selectionStart != nil { clearSelection(); return }
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
    // Invalidate a retained responder synchronously. The later poll still
    // catches the fresh Ready epoch if Rust foreground recovery completes on
    // a following main-queue turn.
    synchronizeOperationEpoch()
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

  @discardableResult
  private func synchronizeOperationEpoch() -> Bool {
    guard terminalHandle != 0 else { return false }
    let operationEpoch = MeetermCore.operationEpoch(terminalId: terminalHandle)
    guard operationEpoch != lastOperationEpoch else { return false }
    lastOperationEpoch = operationEpoch
    terminalInputView.operationEpochDidChange(operationEpoch)
    lastColumns = 0
    lastRows = 0
    setNeedsLayout()
    return true
  }

  private func reconcileResize(for size: CGSize) {
    guard terminalHandle != 0, size.width > 0, size.height > 0 else {
      return
    }

    let columns = max(
      2,
      Int((size.width / cellSize.width).rounded(.down))
    )
    let rows = max(
      1,
      Int((size.height / cellSize.height).rounded(.down))
    )
    guard columns != lastColumns || rows != lastRows else {
      return
    }
    guard !isCachedReadOnly,
          let epoch = currentOperationEpoch(),
          MeetermCore.resizeAtEpoch(
            terminalId: terminalHandle,
            expectedEpoch: epoch,
            columns: columns,
            rows: rows
          ) else {
      return
    }

    clearSelection()
    lastColumns = columns
    lastRows = rows
    let scale = window?.screen.scale ?? contentScaleFactor
    onMetrics([
      "terminalId": terminalId,
      "columns": columns,
      "rows": rows,
      "cellWidthPx": Int((cellSize.width * scale).rounded()),
      "cellHeightPx": Int((cellSize.height * scale).rounded())
    ])
    renderer.requestFrame()
  }

  @objc private func scrollTerminal(_ gesture: UIPanGestureRecognizer) {
    guard terminalHandle != 0 else { return }
    if selectionStart != nil {
      updateSelection(at: gesture.location(in: renderingView))
      return
    }
    let translation = gesture.translation(in: self)
    let lines = Int32((translation.y / cellSize.height).rounded(.towardZero))
    if lines != 0 {
      if let epoch = currentOperationEpoch(),
         MeetermCore.scrollAtEpoch(
           terminalId: terminalHandle,
           expectedEpoch: epoch,
           lines: lines
         ) {
        renderer.requestFrame()
      }
      gesture.setTranslation(CGPoint(x: 0, y: translation.y - CGFloat(lines) * cellSize.height), in: self)
    }
  }

  private func currentOperationEpoch() -> UInt64? {
    guard terminalHandle != 0 else { return nil }
    return MeetermCore.operationEpoch(terminalId: terminalHandle)
  }

  private func commit(_ text: String, epoch: UInt64) {
    guard !isCachedReadOnly, terminalHandle != 0 else {
      return
    }
    if MeetermCore.commitAtEpoch(
      terminalId: terminalHandle,
      expectedEpoch: epoch,
      text: text
    ) > 0 {
      clearSelection()
      renderer.requestFrame()
    }
  }

  private func commitModified(_ text: String, modifiers: UInt32, epoch: UInt64) {
    guard !isCachedReadOnly, terminalHandle != 0 else { return }
    if MeetermCore.commitModifiedAtEpoch(
      terminalId: terminalHandle,
      expectedEpoch: epoch,
      text: text,
      modifiers: modifiers
    ) {
      clearSelection()
      renderer.requestFrame()
    }
  }

  private func send(_ key: TerminalSpecialKey, epoch: UInt64) {
    guard !isCachedReadOnly, terminalHandle != 0 else {
      return
    }
    if MeetermCore.sendSpecialAtEpoch(
      terminalId: terminalHandle,
      expectedEpoch: epoch,
      key: key
    ) {
      clearSelection()
      renderer.requestFrame()
    }
  }

  private func sendModified(_ key: TerminalSpecialKey, modifiers: UInt32, epoch: UInt64) {
    guard !isCachedReadOnly, terminalHandle != 0 else { return }
    if MeetermCore.sendKeyAtEpoch(
      terminalId: terminalHandle,
      expectedEpoch: epoch,
      key: key,
      modifiers: modifiers
    ) {
      clearSelection()
      renderer.requestFrame()
    }
  }

  private func configureSelectionControls() {
    selectionBar.axis = .horizontal
    selectionBar.spacing = 8
    selectionBar.distribution = .fillEqually
    selectionBar.isHidden = true
    for (title, action) in [("コピー", #selector(copySelection)), ("解除", #selector(cancelSelection))] {
      var configuration = UIButton.Configuration.filled()
      configuration.title = title
      configuration.baseBackgroundColor = UIColor(red: 0.57, green: 0.38, blue: 0.13, alpha: 1)
      let button = UIButton(configuration: configuration)
      button.accessibilityLabel = title == "コピー" ? "Copy selection" : "Cancel selection"
      button.addTarget(self, action: action, for: .touchUpInside)
      selectionBar.addArrangedSubview(button)
    }
    addSubview(selectionBar)
    for (handle, name) in [(startHandle, "Selection start"), (endHandle, "Selection end")] {
      handle.setTitle("●", for: .normal)
      handle.titleLabel?.font = .systemFont(ofSize: 24)
      handle.tintColor = UIColor(red: 0.57, green: 0.38, blue: 0.13, alpha: 1)
      handle.accessibilityLabel = name
      handle.isHidden = true
      handle.addGestureRecognizer(UIPanGestureRecognizer(target: self, action: #selector(dragSelectionHandle(_:))))
      addSubview(handle)
    }
  }

  private func cell(at point: CGPoint) -> (row: Int, column: Int) {
    (max(0, min(lastRows - 1, Int(floor(point.y / cellSize.height)))),
     max(0, min(lastColumns - 1, Int(floor(point.x / cellSize.width)))))
  }

  @objc private func selectTerminal(_ gesture: UILongPressGestureRecognizer) {
    guard terminalHandle != 0, lastColumns > 0, lastRows > 0 else { return }
    let location = gesture.location(in: renderingView)
    if gesture.state == .began {
      let point = cell(at: location)
      if MeetermCore.selectStart(terminalId: terminalHandle, row: point.row, column: point.column) {
        selectionStart = point
        selectionEnd = point
        selectionBar.isHidden = false
        startHandle.isHidden = false
        endHandle.isHidden = false
        UISelectionFeedbackGenerator().selectionChanged()
        positionSelectionHandles()
      }
    } else if gesture.state == .changed { updateSelection(at: location) }
    renderer.requestFrame()
  }

  private func updateSelection(at location: CGPoint) {
    guard selectionStart != nil else { return }
    let point = cell(at: location)
    if MeetermCore.selectUpdate(terminalId: terminalHandle, row: point.row, column: point.column) {
      selectionEnd = point
      positionSelectionHandles()
      renderer.requestFrame()
    }
  }

  @objc private func dragSelectionHandle(_ gesture: UIPanGestureRecognizer) {
    let point = cell(at: gesture.location(in: renderingView))
    if gesture.view === startHandle, let end = selectionEnd {
      if MeetermCore.selectStart(terminalId: terminalHandle, row: point.row, column: point.column) {
        selectionStart = point
        _ = MeetermCore.selectUpdate(terminalId: terminalHandle, row: end.row, column: end.column)
      }
    } else { updateSelection(at: gesture.location(in: renderingView)) }
    positionSelectionHandles()
    renderer.requestFrame()
  }

  private func positionSelectionHandles() {
    for (handle, point, offset) in [(startHandle, selectionStart, CGFloat(0)), (endHandle, selectionEnd, CGFloat(1))] {
      guard let point else { continue }
      let x = renderingView.frame.minX + (CGFloat(point.column) + offset) * cellSize.width
      let y = renderingView.frame.minY + (CGFloat(point.row) + 1) * cellSize.height
      handle.frame = CGRect(x: min(max(0, x - 22), max(0, bounds.width - 44)),
        y: min(max(0, y - 12), max(0, renderingView.frame.maxY - 44)), width: 44, height: 44)
    }
  }

  @objc private func copySelection() {
    guard let text = MeetermCore.selectionText(terminalId: terminalHandle), !text.isEmpty else { return }
    UIPasteboard.general.string = text
    clearSelection()
    UIAccessibility.post(notification: .announcement, argument: "コピーしました")
  }

  @objc private func cancelSelection() { clearSelection() }

  private func clearSelection() {
    guard selectionStart != nil else { return }
    if terminalHandle != 0 { MeetermCore.clearSelection(terminalId: terminalHandle) }
    selectionStart = nil
    selectionEnd = nil
    selectionBar.isHidden = true
    startHandle.isHidden = true
    endHandle.isHidden = true
    renderer.requestFrame()
  }
}
