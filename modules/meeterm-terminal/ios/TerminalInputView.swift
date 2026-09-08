import Foundation
import UIKit

/// Native UITextInput implementation supplied by UITextView. Marked/preedit
/// text remains in this view and is sent only to the native renderer. Rust is
/// called exactly once when UIKit commits the text.
final class TerminalInputView: UITextView {
  var onCommit: ((String) -> Void)?
  var onPaste: ((String) -> Void)?
  var onPreeditChanged: ((String) -> Void)?
  var onSpecialKey: ((TerminalSpecialKey) -> Void)?

  private var isReplacingMarkedText = false
  private var pasteGeneration: UInt64 = 0
  private var pendingPasteGeneration: UInt64?
  private var pendingPasteProgress: Progress?
  private lazy var terminalAccessoryView: UIView = makeAccessoryView()
  private lazy var terminalPasteControl: UIPasteControl = makePasteControl()

  override init(frame: CGRect, textContainer: NSTextContainer?) {
    super.init(frame: frame, textContainer: textContainer)
    configure()
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) {
    fatalError("init(coder:) has not been implemented")
  }

  override var keyCommands: [UIKeyCommand]? {
    let commands = [
      UIKeyCommand(input: UIKeyCommand.inputEscape, modifierFlags: [], action: #selector(sendEscape)),
      UIKeyCommand(input: "\t", modifierFlags: [], action: #selector(sendTab)),
      UIKeyCommand(input: "c", modifierFlags: [.control], action: #selector(sendInterrupt)),
      UIKeyCommand(input: UIKeyCommand.inputUpArrow, modifierFlags: [], action: #selector(sendUp)),
      UIKeyCommand(input: UIKeyCommand.inputDownArrow, modifierFlags: [], action: #selector(sendDown)),
      UIKeyCommand(input: UIKeyCommand.inputLeftArrow, modifierFlags: [], action: #selector(sendLeft)),
      UIKeyCommand(input: UIKeyCommand.inputRightArrow, modifierFlags: [], action: #selector(sendRight))
    ]
    for command in commands {
      command.wantsPriorityOverSystemBehavior = true
    }
    return commands
  }

  override func setMarkedText(_ markedText: String?, selectedRange: NSRange) {
    super.setMarkedText(markedText, selectedRange: selectedRange)
    onPreeditChanged?(currentMarkedText())
  }

  override func unmarkText() {
    let committed = currentMarkedText()
    super.unmarkText()
    onPreeditChanged?("")
    if !isReplacingMarkedText, !committed.isEmpty {
      onCommit?(committed)
      resetBackingStore()
    }
  }

  override func insertText(_ text: String) {
    isReplacingMarkedText = true
    super.insertText(text)
    isReplacingMarkedText = false
    onPreeditChanged?("")

    switch text {
    case "\n", "\r":
      onSpecialKey?(.enter)
    case "\t":
      onSpecialKey?(.tab)
    default:
      if !text.isEmpty {
        onCommit?(text)
      }
    }
    resetBackingStore()
  }

  override func deleteBackward() {
    if markedTextRange != nil {
      super.deleteBackward()
      onPreeditChanged?(currentMarkedText())
    } else {
      onSpecialKey?(.backspace)
    }
  }

  override func paste(_ sender: Any?) {
    // Read the clipboard only in response to the user's explicit paste action.
    invalidatePendingPaste()
    guard let pasted = UIPasteboard.general.string, !pasted.isEmpty else { return }
    deliverPaste(pasted)
  }

  override func canPaste(_ itemProviders: [NSItemProvider]) -> Bool {
    itemProviders.contains { $0.canLoadObject(ofClass: String.self) }
  }

  override func paste(itemProviders: [NSItemProvider]) {
    invalidatePendingPaste()
    guard window != nil, isFirstResponder,
          let provider = itemProviders.first(where: { $0.canLoadObject(ofClass: String.self) }) else {
      return
    }

    let generation = pasteGeneration
    pendingPasteGeneration = generation
    terminalPasteControl.accessibilityValue = "Pasting"
    pendingPasteProgress = provider.loadObject(ofClass: String.self) { [weak self] pasted, _ in
      DispatchQueue.main.async { [weak self] in
        guard let self,
              self.pasteGeneration == generation,
              self.pendingPasteGeneration == generation else {
          return
        }
        self.pendingPasteGeneration = nil
        self.pendingPasteProgress = nil
        self.terminalPasteControl.accessibilityValue = "Ready"
        guard self.window != nil, self.isFirstResponder,
              let pasted, !pasted.isEmpty else {
          return
        }
        self.deliverPaste(pasted)
      }
    }
  }

  override func didMoveToWindow() {
    super.didMoveToWindow()
    if window == nil {
      invalidatePendingPaste()
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
    invalidatePendingPaste()
    super.unmarkText()
    resetBackingStore()
    onPreeditChanged?("")
    // End the old UIKit input session before its callbacks can target a new
    // pane. The newly selected terminal can be focused with a native tap.
    resignFirstResponder()
  }

  private func invalidatePendingPaste() {
    pasteGeneration &+= 1
    pendingPasteGeneration = nil
    pendingPasteProgress?.cancel()
    pendingPasteProgress = nil
    terminalPasteControl.accessibilityValue = "Ready"
  }

  private func deliverPaste(_ pasted: String) {
    guard !pasted.isEmpty else { return }
    super.unmarkText()
    resetBackingStore()
    onPreeditChanged?("")
    onPaste?(pasted)
  }

  private func resetBackingStore() {
    text = ""
    selectedRange = NSRange(location: 0, length: 0)
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
    let keys = UIStackView(arrangedSubviews: [
      accessoryButton(title: "Esc", action: #selector(sendEscape)),
      accessoryButton(title: "Tab", action: #selector(sendTab)),
      accessoryButton(title: "^C", action: #selector(sendInterrupt)),
      terminalPasteControl,
      accessoryButton(title: "←", action: #selector(sendLeft)),
      accessoryButton(title: "↑", action: #selector(sendUp)),
      accessoryButton(title: "↓", action: #selector(sendDown)),
      accessoryButton(title: "→", action: #selector(sendRight))
    ])
    keys.axis = .horizontal
    keys.spacing = 4
    keys.translatesAutoresizingMaskIntoConstraints = false
    scroll.addSubview(keys)
    accessory.addSubview(scroll)

    let hide = accessoryButton(title: "⌄", action: #selector(hideKeyboard))
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

  private func accessoryButton(title: String, action: Selector) -> UIButton {
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
    NSLayoutConstraint.activate([
      button.widthAnchor.constraint(greaterThanOrEqualToConstant: 44),
      button.heightAnchor.constraint(equalToConstant: 44)
    ])
    return button
  }

  @objc private func sendEscape() {
    emitSpecial(.escape)
  }

  @objc private func sendTab() {
    emitSpecial(.tab)
  }

  @objc private func sendInterrupt() {
    emitSpecial(.interrupt)
  }

  @objc private func hideKeyboard() {
    cancelCompositionForBinding()
  }

  @objc private func sendUp() {
    emitSpecial(.up)
  }

  @objc private func sendDown() {
    emitSpecial(.down)
  }

  @objc private func sendLeft() {
    emitSpecial(.left)
  }

  @objc private func sendRight() {
    emitSpecial(.right)
  }

  private func emitSpecial(_ key: TerminalSpecialKey) {
    if markedTextRange != nil {
      super.setMarkedText(nil, selectedRange: NSRange(location: 0, length: 0))
      resetBackingStore()
      onPreeditChanged?("")
    }
    onSpecialKey?(key)
  }
}
