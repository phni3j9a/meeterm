import Foundation

/**
 * Process-wide lookup of "does terminal X have an active marked text?".
 *
 * `MeetermTerminalView` registers a provider that reads its own
 * `TerminalInputView.markedTextRange` on the main thread, so module async
 * functions can ask without touching input internals. Providers are removed
 * when the view rebinds or leaves the window.
 */
final class AttachmentCompositionGuard {
  static let shared = AttachmentCompositionGuard()

  private var providers: [String: () -> Bool] = [:]
  private var overrideProvider: ((String) -> Bool)?
  private let lock = NSLock()

  func register(terminalId: String, provider: @escaping () -> Bool) {
    lock.lock()
    providers[terminalId] = provider
    lock.unlock()
  }

  func unregister(terminalId: String) {
    lock.lock()
    providers.removeValue(forKey: terminalId)
    lock.unlock()
  }

  func isComposing(terminalId: String) -> Bool {
    lock.lock()
    let override = overrideProvider
    let provider = providers[terminalId]
    lock.unlock()
    if let override = override { return override(terminalId) }
    return provider?() ?? false
  }

  /// Test hook: when set, it replaces all registered providers.
  func setOverrideForTesting(_ provider: ((String) -> Bool)?) {
    lock.lock()
    overrideProvider = provider
    lock.unlock()
  }
}
