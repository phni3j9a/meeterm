import Foundation

/// Process-local stable identity mapping. Views borrow a handle; unmounting a
/// view never destroys its Rust terminal state.
final class TerminalRegistry {
  private static let lock = NSLock()
  private static var handles: [String: UInt64] = [:]

  static func acquire(terminalId: String, columns: Int, rows: Int) -> UInt64 {
    lock.lock()
    defer { lock.unlock() }

    if let existing = handles[terminalId] {
      return existing
    }

    let handle = MeetermCore.create(columns: columns, rows: rows)
    guard handle != 0 else {
      return 0
    }
    handles[terminalId] = handle
    return handle
  }

  /// Destruction is explicit and reserved for tests/process ownership. Normal
  /// Expo view lifecycle changes intentionally do not call this function.
  static func resetForTests() {
    lock.lock()
    defer { lock.unlock() }

    for handle in handles.values {
      MeetermCore.destroy(terminalId: handle)
    }
    handles.removeAll(keepingCapacity: false)
  }
}
