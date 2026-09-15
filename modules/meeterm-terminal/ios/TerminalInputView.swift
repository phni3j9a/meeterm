import Foundation
import UIKit

/// Shared native-side validation for recovery values that arrive as strings
/// from JavaScript. The decimal epoch is parsed only after an ASCII digit
/// check, so it never travels through NSNumber/Double.
enum RecoveryBridgeValidation {
  static func parseOperationEpoch(_ value: String) -> UInt64? {
    guard !value.isEmpty,
          value.utf8.count <= 20,
          value.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 }) else {
      return nil
    }
    return UInt64(value)
  }

  static func validRecoveryToken(_ value: String) -> Bool {
    !value.isEmpty && value.utf8.count <= 128 &&
      !value.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) &&
      !value.utf8.contains(0)
  }
}

/// Native UITextInput implementation supplied by UITextView. Marked/preedit
/// text remains in this view and is sent only to the native renderer. Rust is
/// called exactly once when UIKit commits the text.
final class TerminalInputView: UITextView {
  var onCommit: ((String) -> Void)?
  var onCommitAtEpoch: ((String, UInt64) -> Void)?
  var onPaste: ((String) -> Void)?
  var onPasteAtEpoch: ((String, UInt64) -> Void)?
  var onPreeditChanged: ((String) -> Void)?
  var onSpecialKey: ((TerminalSpecialKey) -> Void)?
  var onSpecialKeyAtEpoch: ((TerminalSpecialKey, UInt64) -> Void)?
  var onModifiedCommit: ((String, UInt32) -> Void)?
  var onModifiedCommitAtEpoch: ((String, UInt32, UInt64) -> Void)?
  var onModifiedSpecialKey: ((TerminalSpecialKey, UInt32) -> Void)?
  var onModifiedSpecialKeyAtEpoch: ((TerminalSpecialKey, UInt32, UInt64) -> Void)?
  var onCopySelection: (() -> Void)?
  var hasTerminalSelection: (() -> Bool)?
  var operationEpochProvider: (() -> UInt64?)?

  // One-shot accessory modifiers live next to UIKit composition, never in JS.
  private var modifiers: UInt32 = 0
  private weak var controlButton: UIButton?
  private weak var altButton: UIButton?

  private var isReplacingMarkedText = false
  private var pasteGeneration: UInt64 = 0
  private var pendingPasteGeneration: UInt64?
  private var pendingPasteEpoch: UInt64?
  private var pendingPasteProgress: Progress?
  private var inputSessionEpoch: UInt64?
  private var isCachedReadOnly = false
  private var remoteInputControls: [UIView] = []
  private lazy var terminalAccessoryView: UIView = makeAccessoryView()
  private lazy var terminalPasteControl: UIPasteControl = makePasteControl()
  private let observesInputLifecycle = ProcessInfo.processInfo.arguments.contains("-meeterm-ui-observation")

  private enum PasteDropReason {
    case generation
    case cancel
    case focus
    case window
    case provider
    case epoch
  }

  override init(frame: CGRect, textContainer: NSTextContainer?) {
    super.init(frame: frame, textContainer: textContainer)
    configure()
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) {
    fatalError("init(coder:) has not been implemented")
  }

  // Opt-in public-screen diagnostics record only lifecycle booleans, never
  // text, marked ranges, key values, terminal IDs, or clipboard contents.
  override func becomeFirstResponder() -> Bool {
    guard !isCachedReadOnly else { return false }
    let result = super.becomeFirstResponder()
    if result {
      // A UIKit responder session gets one immutable operation epoch. Delayed
      // callbacks from this responder can then be rejected after recovery.
      inputSessionEpoch = operationEpochProvider?()
    }
    if observesInputLifecycle {
      NSLog("MEETERM_SMOKE_INPUT_FOCUS result=%d window=%d", result ? 1 : 0, window != nil ? 1 : 0)
    }
    return result
  }

  override func resignFirstResponder() -> Bool {
    let wasFocused = isFirstResponder
    let result = super.resignFirstResponder()
    if result || wasFocused {
      inputSessionEpoch = nil
    }
    if observesInputLifecycle {
      NSLog("MEETERM_SMOKE_INPUT_RESIGN focused=%d result=%d", wasFocused ? 1 : 0, result ? 1 : 0)
    }
    return result
  }

  override var keyCommands: [UIKeyCommand]? {
    guard !isCachedReadOnly else { return [] }
    var commands: [UIKeyCommand] = []
    let special = [UIKeyCommand.inputEscape, "\t", UIKeyCommand.inputUpArrow,
      UIKeyCommand.inputDownArrow, UIKeyCommand.inputLeftArrow, UIKeyCommand.inputRightArrow,
      UIKeyCommand.inputHome, UIKeyCommand.inputEnd, UIKeyCommand.inputPageUp,
      UIKeyCommand.inputPageDown, UIKeyCommand.inputDelete]
    let combinations: [UIKeyModifierFlags] = [[], .shift, .control, .alternate,
      [.control, .shift], [.alternate, .shift], [.control, .alternate], [.control, .alternate, .shift]]
    for flags in combinations {
      for input in special { commands.append(UIKeyCommand(input: input, modifierFlags: flags, action: #selector(hardwareKey(_:)))) }
    }
    let textCombinations: [UIKeyModifierFlags] = [.control, .alternate, [.control, .alternate],
      [.control, .shift], [.alternate, .shift], [.control, .alternate, .shift]]
    for flags in textCombinations {
      for scalar in 32...126 {
        let input = String(UnicodeScalar(scalar)!)
        commands.append(UIKeyCommand(input: input, modifierFlags: flags, action: #selector(hardwareKey(_:))))
      }
    }
    for command in commands {
      command.wantsPriorityOverSystemBehavior = true
    }
    return commands
  }

  override func setMarkedText(_ markedText: String?, selectedRange: NSRange) {
    guard !isCachedReadOnly else { return }
    super.setMarkedText(markedText, selectedRange: selectedRange)
    onPreeditChanged?(currentMarkedText())
  }

  override func unmarkText() {
    if isCachedReadOnly {
      super.unmarkText()
      resetBackingStore()
      onPreeditChanged?("")
      return
    }
    let committed = currentMarkedText()
    super.unmarkText()
    onPreeditChanged?("")
    if !isReplacingMarkedText, !committed.isEmpty {
      emitCommit(committed)
      resetBackingStore()
    }
  }

  override func insertText(_ text: String) {
    guard !isCachedReadOnly else { return }
    isReplacingMarkedText = true
    super.insertText(text)
    isReplacingMarkedText = false
    onPreeditChanged?("")

    switch text {
    case "\n", "\r":
      emitSpecial(.enter)
    case "\t":
      emitSpecial(.tab)
    default:
      if !text.isEmpty {
        emitCommit(text)
      }
    }
    resetBackingStore()
  }

  override func deleteBackward() {
    guard !isCachedReadOnly else { return }
    if markedTextRange != nil {
      super.deleteBackward()
      onPreeditChanged?(currentMarkedText())
    } else {
      emitSpecial(.backspace)
    }
  }

  override func paste(_ sender: Any?) {
    guard !isCachedReadOnly else { return }
    // Read the clipboard only in response to the user's explicit paste action.
    recordPasteRequest()
    invalidatePendingPaste(dropReason: .generation)
    guard let pasted = UIPasteboard.general.string, !pasted.isEmpty else { return }
    let epoch = activeOperationEpoch
    guard onPasteAtEpoch == nil || epoch != nil else {
      recordPasteDrop(.epoch)
      return
    }
    guard onPasteAtEpoch == nil || operationEpochProvider?() == epoch else {
      recordPasteDrop(.epoch)
      return
    }
    deliverPaste(pasted, epoch: epoch)
  }

  override func copy(_ sender: Any?) { onCopySelection?() }

  override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
    if action == #selector(copy(_:)) { return hasTerminalSelection?() == true }
    return super.canPerformAction(action, withSender: sender)
  }

  override func canPaste(_ itemProviders: [NSItemProvider]) -> Bool {
    !isCachedReadOnly && itemProviders.contains { $0.canLoadObject(ofClass: String.self) }
  }

  override func paste(itemProviders: [NSItemProvider]) {
    guard !isCachedReadOnly else { return }
    recordPasteRequest()
    invalidatePendingPaste(dropReason: .generation)
    guard window != nil else {
      recordPasteDrop(.window)
      return
    }
    guard isFirstResponder else {
      recordPasteDrop(.focus)
      return
    }
    guard let provider = itemProviders.first(where: { $0.canLoadObject(ofClass: String.self) }) else {
      recordPasteDrop(.provider)
      return
    }

    let generation = pasteGeneration
    let operationEpoch = activeOperationEpoch
    guard onPasteAtEpoch == nil || operationEpoch != nil else {
      recordPasteDrop(.epoch)
      return
    }
    let observesProviderCompletion = observesInputLifecycle
    pendingPasteGeneration = generation
    pendingPasteEpoch = operationEpoch
    terminalPasteControl.accessibilityValue = "Pasting"
    pendingPasteProgress = provider.loadObject(ofClass: String.self) { [weak self] pasted, _ in
      if observesProviderCompletion {
        NSLog("MEETERM_SMOKE_PASTE_PROVIDER_COMPLETION")
      }
        DispatchQueue.main.async { [weak self] in
          guard let self,
                self.pasteGeneration == generation,
                self.pendingPasteGeneration == generation,
                self.pendingPasteEpoch == operationEpoch else {
            return
          }
          self.pendingPasteGeneration = nil
          self.pendingPasteEpoch = nil
          self.pendingPasteProgress = nil
        self.terminalPasteControl.accessibilityValue = "Ready"
        let windowAttached = self.window != nil
        let focused = self.isFirstResponder
        guard windowAttached, focused else {
          if !windowAttached { self.recordPasteDrop(.window) }
          if !focused { self.recordPasteDrop(.focus) }
          return
        }
        guard self.onPasteAtEpoch == nil || self.operationEpochProvider?() == operationEpoch else {
          self.invalidatePendingPaste(dropReason: .epoch)
          return
        }
        guard let pasted, !pasted.isEmpty else {
          self.recordPasteDrop(.provider)
          return
        }
        self.deliverPaste(pasted, epoch: operationEpoch)
      }
    }
  }

  override func didMoveToWindow() {
    super.didMoveToWindow()
    if observesInputLifecycle {
      NSLog("MEETERM_SMOKE_INPUT_WINDOW attached=%d", window != nil ? 1 : 0)
    }
    if window == nil {
      invalidatePendingPaste(dropReason: .window)
    }
  }

  private func configure() {
    backgroundColor = .clear
    textColor = .clear
    tintColor = .clear
    font = .systemFont(ofSize: 1)
    isScrollEnabled = false
    isAccessibilityElement = false
    accessibilityElementsHidden = true
    autocapitalizationType = .none
    autocorrectionType = .no
    spellCheckingType = .no
    smartDashesType = .no
    smartQuotesType = .no
    smartInsertDeleteType = .no
    keyboardType = .default
    keyboardAppearance = .dark
    returnKeyType = .default
    pasteConfiguration = UIPasteConfiguration(forAccepting: String.self)
    inputAccessoryView = terminalAccessoryView
    inputAssistantItem.leadingBarButtonGroups = []
    inputAssistantItem.trailingBarButtonGroups = []
  }

  private func currentMarkedText() -> String {
    guard let range = markedTextRange else {
      return ""
    }
    return text(in: range) ?? ""
  }

  /// Cancel local preedit before borrowing a different native terminal.
  func cancelCompositionForBinding() {
    if observesInputLifecycle { NSLog("MEETERM_SMOKE_INPUT_BINDING_CANCEL") }
    clearModifiers()
    invalidatePendingPaste(dropReason: .cancel)
    super.unmarkText()
    resetBackingStore()
    onPreeditChanged?("")
    inputSessionEpoch = nil
    // End the old UIKit input session before its callbacks can target a new
    // pane. The newly selected terminal can be focused with a native tap.
    resignFirstResponder()
  }

  private func invalidatePendingPaste(dropReason: PasteDropReason? = nil) {
    let hasPendingPaste = pendingPasteGeneration != nil
    if hasPendingPaste, let dropReason {
      recordPasteDrop(dropReason)
    }
    pasteGeneration &+= 1
    pendingPasteGeneration = nil
    pendingPasteEpoch = nil
    pendingPasteProgress?.cancel()
    pendingPasteProgress = nil
    terminalPasteControl.accessibilityValue = "Ready"
  }

  /// Switch the remote interaction policy without replacing the native
  /// terminal binding or renderer. Cached read-only mode keeps local display,
  /// scrolling, selection, and copying available while invalidating input.
  func setInteractionMode(_ value: String) {
    let nextReadOnly = value != "live"
    guard nextReadOnly != isCachedReadOnly else {
      updateInteractionAccessibility()
      return
    }
    isCachedReadOnly = nextReadOnly
    if nextReadOnly {
      cancelCompositionForBinding()
    }
    updateInteractionAccessibility()
  }

  /// A native Rust operation-epoch boundary invalidates UIKit composition and
  /// every pending provider callback. The next responder acquisition captures
  /// the new epoch; this method never auto-focuses the keyboard.
  func operationEpochDidChange(_ epoch: UInt64?) {
    guard inputSessionEpoch != nil || pendingPasteGeneration != nil else { return }
    guard inputSessionEpoch != epoch || pendingPasteEpoch != epoch else { return }
    cancelCompositionForBinding()
  }

  private var activeOperationEpoch: UInt64? {
    inputSessionEpoch ?? operationEpochProvider?()
  }

  private func deliverPaste(_ pasted: String, epoch: UInt64?) {
    guard !pasted.isEmpty else { return }
    recordPasteDeliveryAttempt()
    super.unmarkText()
    resetBackingStore()
    onPreeditChanged?("")
    clearModifiers()
    if let epoch, let onPasteAtEpoch {
      onPasteAtEpoch(pasted, epoch)
    } else if onPasteAtEpoch == nil {
      onPaste?(pasted)
    }
  }

  private func recordPasteRequest() {
    if observesInputLifecycle { NSLog("MEETERM_SMOKE_PASTE_REQUEST") }
  }

  private func recordPasteDeliveryAttempt() {
    if observesInputLifecycle { NSLog("MEETERM_SMOKE_PASTE_DELIVERY_ATTEMPT") }
  }

  private func recordPasteDrop(_ reason: PasteDropReason) {
    guard observesInputLifecycle else { return }
    switch reason {
    case .generation:
      NSLog("MEETERM_SMOKE_PASTE_DROP_GENERATION")
    case .cancel:
      NSLog("MEETERM_SMOKE_PASTE_DROP_CANCEL")
    case .focus:
      NSLog("MEETERM_SMOKE_PASTE_DROP_FOCUS")
    case .window:
      NSLog("MEETERM_SMOKE_PASTE_DROP_WINDOW")
    case .provider:
      NSLog("MEETERM_SMOKE_PASTE_DROP_PROVIDER")
    case .epoch:
      NSLog("MEETERM_SMOKE_PASTE_DROP_EPOCH")
    }
  }

  private func resetBackingStore() {
    text = ""
    selectedRange = NSRange(location: 0, length: 0)
  }

  private func updateInteractionAccessibility() {
    for control in remoteInputControls {
      control.isUserInteractionEnabled = !isCachedReadOnly
      control.isAccessibilityElement = !isCachedReadOnly
      control.accessibilityElementsHidden = isCachedReadOnly
      if let control = control as? UIControl {
        control.isEnabled = !isCachedReadOnly
      }
    }
  }

  private func makeAccessoryView() -> UIView {
    let accessory = UIView(frame: CGRect(x: 0, y: 0, width: 0, height: 52))
    accessory.autoresizingMask = [.flexibleWidth]
    accessory.backgroundColor = UIColor(red: 33.0 / 255, green: 31.0 / 255, blue: 27.0 / 255, alpha: 1)

    // A compact phone cannot fit every terminal key at its native touch size.
    // Keep keyboard dismissal visible and let the remaining keys scroll.
    let scroll = UIScrollView()
    scroll.translatesAutoresizingMaskIntoConstraints = false
    scroll.showsHorizontalScrollIndicator = false
    scroll.alwaysBounceHorizontal = false
    scroll.contentInsetAdjustmentBehavior = .never
    let control = accessoryButton(title: "Ctrl", action: #selector(toggleControl))
    let alt = accessoryButton(title: "Alt", action: #selector(toggleAlt))
    remoteInputControls.append(terminalPasteControl)
    controlButton = control
    altButton = alt
    let keys = UIStackView(arrangedSubviews: [
      accessoryButton(title: "Esc", action: #selector(sendEscape)),
      accessoryButton(title: "Tab", action: #selector(sendTab)),
      accessoryButton(title: "^C", action: #selector(sendInterrupt)),
      terminalPasteControl,
      control,
      alt,
      accessoryButton(title: "←", action: #selector(sendLeft)),
      accessoryButton(title: "↑", action: #selector(sendUp)),
      accessoryButton(title: "↓", action: #selector(sendDown)),
      accessoryButton(title: "→", action: #selector(sendRight)),
      accessoryButton(title: "Home", action: #selector(sendHome)),
      accessoryButton(title: "End", action: #selector(sendEnd)),
      accessoryButton(title: "PgUp", action: #selector(sendPageUp)),
      accessoryButton(title: "PgDn", action: #selector(sendPageDown)),
      accessoryButton(title: "Del", action: #selector(sendDelete))
    ])
    keys.axis = .horizontal
    keys.spacing = 4
    keys.translatesAutoresizingMaskIntoConstraints = false
    scroll.addSubview(keys)
    accessory.addSubview(scroll)

    let hide = accessoryButton(title: "⌄", action: #selector(hideKeyboard), remote: false)
    accessory.addSubview(hide)
    NSLayoutConstraint.activate([
      scroll.leadingAnchor.constraint(equalTo: accessory.safeAreaLayoutGuide.leadingAnchor, constant: 8),
      scroll.topAnchor.constraint(equalTo: accessory.topAnchor),
      scroll.bottomAnchor.constraint(equalTo: accessory.bottomAnchor),
      scroll.trailingAnchor.constraint(equalTo: hide.leadingAnchor, constant: -4),
      hide.trailingAnchor.constraint(equalTo: accessory.safeAreaLayoutGuide.trailingAnchor, constant: -8),
      hide.centerYAnchor.constraint(equalTo: accessory.centerYAnchor),
      hide.widthAnchor.constraint(equalToConstant: 44),
      keys.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
      keys.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
      keys.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 4),
      keys.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -4),
      keys.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -8)
    ])
    return accessory
  }

  private func makePasteControl() -> UIPasteControl {
    var configuration = UIPasteControl.Configuration()
    configuration.baseForegroundColor = UIColor(
      red: 219.0 / 255,
      green: 179.0 / 255,
      blue: 120.0 / 255,
      alpha: 1
    )
    configuration.baseBackgroundColor = UIColor(white: 1, alpha: 0.05)
    configuration.cornerStyle = .capsule
    configuration.displayMode = .labelOnly
    let control = UIPasteControl(configuration: configuration)
    control.target = self
    control.accessibilityLabel = "Paste"
    control.accessibilityIdentifier = "terminal-paste"
    control.accessibilityValue = "Ready"
    control.translatesAutoresizingMaskIntoConstraints = false
    NSLayoutConstraint.activate([
      control.widthAnchor.constraint(greaterThanOrEqualToConstant: 64),
      control.heightAnchor.constraint(equalToConstant: 44)
    ])
    return control
  }

  private func accessoryButton(title: String, action: Selector, remote: Bool = true) -> UIButton {
    var configuration = UIButton.Configuration.plain()
    configuration.title = title
    configuration.baseForegroundColor = UIColor(red: 219.0 / 255, green: 179.0 / 255, blue: 120.0 / 255, alpha: 1)
    configuration.background.backgroundColor = UIColor(white: 1, alpha: 0.05)
    configuration.background.cornerRadius = 8
    configuration.contentInsets = NSDirectionalEdgeInsets(top: 8, leading: 10, bottom: 8, trailing: 10)
    let button = UIButton(configuration: configuration)
    button.translatesAutoresizingMaskIntoConstraints = false
    button.accessibilityLabel = title == "^C" ? "Ctrl-C" : title == "⌄" ? "Hide keyboard" : title
    button.addTarget(self, action: action, for: .touchUpInside)
    if remote {
      remoteInputControls.append(button)
    }
    NSLayoutConstraint.activate([
      button.widthAnchor.constraint(greaterThanOrEqualToConstant: 44),
      button.heightAnchor.constraint(equalToConstant: 44)
    ])
    return button
  }

  @objc private func sendEscape() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.escape)
  }

  @objc private func sendTab() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.tab)
  }

  @objc private func sendInterrupt() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.interrupt)
  }

  @objc private func hideKeyboard() {
    cancelCompositionForBinding()
  }

  @objc private func sendUp() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.up)
  }

  @objc private func sendDown() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.down)
  }

  @objc private func sendLeft() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.left)
  }

  @objc private func sendRight() {
    guard !isCachedReadOnly else { return }
    emitSpecial(.right)
  }

  @objc private func sendHome() { guard !isCachedReadOnly else { return }; emitSpecial(.home) }
  @objc private func sendEnd() { guard !isCachedReadOnly else { return }; emitSpecial(.end) }
  @objc private func sendPageUp() { guard !isCachedReadOnly else { return }; emitSpecial(.pageUp) }
  @objc private func sendPageDown() { guard !isCachedReadOnly else { return }; emitSpecial(.pageDown) }
  @objc private func sendDelete() { guard !isCachedReadOnly else { return }; emitSpecial(.delete) }
  @objc private func toggleControl() {
    guard !isCachedReadOnly else { return }
    modifiers ^= 1
    updateModifierButtons()
  }
  @objc private func toggleAlt() {
    guard !isCachedReadOnly else { return }
    modifiers ^= 2
    updateModifierButtons()
  }

  private func updateModifierButtons() {
    for (button, bit) in [(controlButton, UInt32(1)), (altButton, UInt32(2))] {
      let selected = modifiers & bit != 0
      button?.isSelected = selected
      button?.accessibilityValue = selected ? "On" : "Off"
      button?.configuration?.background.backgroundColor = selected
        ? UIColor(red: 0.57, green: 0.38, blue: 0.13, alpha: 0.65) : UIColor(white: 1, alpha: 0.05)
    }
  }

  private func clearModifiers() { modifiers = 0; updateModifierButtons() }

  private func emitCommit(_ text: String, flags: UInt32? = nil) {
    guard !isCachedReadOnly else { return }
    let selected = flags ?? modifiers
    clearModifiers()
    guard let epoch = activeOperationEpoch else {
      if selected == 0 {
        if onCommitAtEpoch == nil { onCommit?(text) }
      } else if onModifiedCommitAtEpoch == nil {
        onModifiedCommit?(text, selected)
      }
      return
    }
    if selected == 0 {
      if let onCommitAtEpoch { onCommitAtEpoch(text, epoch) } else { onCommit?(text) }
    } else if let onModifiedCommitAtEpoch {
      onModifiedCommitAtEpoch(text, selected, epoch)
    } else {
      onModifiedCommit?(text, selected)
    }
  }

  @objc private func hardwareKey(_ command: UIKeyCommand) {
    guard !isCachedReadOnly else { return }
    guard let input = command.input else { return }
    var flags: UInt32 = 0
    if command.modifierFlags.contains(.shift) { flags |= 4 }
    if command.modifierFlags.contains(.alternate) { flags |= 2 }
    if command.modifierFlags.contains(.control) { flags |= 1 }
    let key: TerminalSpecialKey?
    switch input {
    case UIKeyCommand.inputEscape: key = .escape
    case "\t": key = .tab
    case UIKeyCommand.inputUpArrow: key = .up
    case UIKeyCommand.inputDownArrow: key = .down
    case UIKeyCommand.inputLeftArrow: key = .left
    case UIKeyCommand.inputRightArrow: key = .right
    case UIKeyCommand.inputHome: key = .home
    case UIKeyCommand.inputEnd: key = .end
    case UIKeyCommand.inputPageUp: key = .pageUp
    case UIKeyCommand.inputPageDown: key = .pageDown
    case UIKeyCommand.inputDelete: key = .delete
    default: key = nil
    }
    if let key { emitSpecial(key, flags: flags) }
    else {
      // Hardware shortcuts cancel preedit; they must never commit it as text.
      if markedTextRange != nil {
        super.setMarkedText(nil, selectedRange: NSRange(location: 0, length: 0))
        resetBackingStore()
        onPreeditChanged?("")
      }
      emitCommit(input, flags: flags)
    }
  }

  private func emitSpecial(_ key: TerminalSpecialKey, flags: UInt32? = nil) {
    guard !isCachedReadOnly else { return }
    if markedTextRange != nil {
      super.setMarkedText(nil, selectedRange: NSRange(location: 0, length: 0))
      resetBackingStore()
      onPreeditChanged?("")
    }
    let selected = flags ?? modifiers
    clearModifiers()
    guard let epoch = activeOperationEpoch else {
      if selected == 0 {
        if onSpecialKeyAtEpoch == nil { onSpecialKey?(key) }
      } else if onModifiedSpecialKeyAtEpoch == nil {
        onModifiedSpecialKey?(key, selected)
      }
      return
    }
    if selected == 0 {
      if let onSpecialKeyAtEpoch { onSpecialKeyAtEpoch(key, epoch) } else { onSpecialKey?(key) }
    } else if let onModifiedSpecialKeyAtEpoch {
      onModifiedSpecialKeyAtEpoch(key, selected, epoch)
    } else {
      onModifiedSpecialKey?(key, selected)
    }
  }
}
