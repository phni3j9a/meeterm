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
  private lazy var terminalAccessoryView: UIView = makeAccessoryView()

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
    guard let pasted = UIPasteboard.general.string, !pasted.isEmpty else { return }
    super.unmarkText()
    resetBackingStore()
    onPreeditChanged?("")
    onPaste?(pasted)
  }

  private func configure() {
    backgroundColor = .clear
    textColor = .clear
    tintColor = .clear
    font = .systemFont(ofSize: 1)
    isScrollEnabled = false
    isAccessibilityElement = false
    accessibilityElementsHidden = true
    autocorrectionType = .no
    spellCheckingType = .no
    smartDashesType = .no
    smartQuotesType = .no
    smartInsertDeleteType = .no
    keyboardType = .default
    keyboardAppearance = .dark
    returnKeyType = .default
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
    super.unmarkText()
    resetBackingStore()
    onPreeditChanged?("")
    // End the old UIKit input session before its callbacks can target a new
    // pane. The newly selected terminal can be focused with a native tap.
    resignFirstResponder()
  }

  private func resetBackingStore() {
    text = ""
    selectedRange = NSRange(location: 0, length: 0)
  }

  private func makeAccessoryView() -> UIView {
    let toolbar = UIToolbar()
    toolbar.barStyle = .black
    toolbar.isTranslucent = false
    toolbar.barTintColor = UIColor(red: 33.0 / 255, green: 31.0 / 255, blue: 27.0 / 255, alpha: 1)
    toolbar.tintColor = UIColor(red: 219.0 / 255, green: 179.0 / 255, blue: 120.0 / 255, alpha: 1)
    toolbar.items = [
      item(title: "Esc", action: #selector(sendEscape)),
      flexibleSpace(),
      item(title: "Tab", action: #selector(sendTab)),
      flexibleSpace(),
      item(title: "^C", action: #selector(sendInterrupt)),
      flexibleSpace(),
      item(title: "Paste", action: #selector(paste(_:))),
      flexibleSpace(),
      item(title: "←", action: #selector(sendLeft)),
      flexibleSpace(),
      item(title: "↑", action: #selector(sendUp)),
      flexibleSpace(),
      item(title: "↓", action: #selector(sendDown)),
      flexibleSpace(),
      item(title: "→", action: #selector(sendRight)),
      flexibleSpace(),
      item(title: "⌄", action: #selector(hideKeyboard))
    ]
    toolbar.sizeToFit()
    return toolbar
  }

  private func item(title: String, action: Selector) -> UIBarButtonItem {
    let button = UIBarButtonItem(title: title, style: .plain, target: self, action: action)
    button.accessibilityLabel = title == "^C" ? "Ctrl-C" : title == "⌄" ? "Hide keyboard" : title
    return button
  }

  private func flexibleSpace() -> UIBarButtonItem {
    UIBarButtonItem(systemItem: .flexibleSpace)
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
