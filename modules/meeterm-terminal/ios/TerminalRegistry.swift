import Foundation

/// Process-local stable identity mapping. Views borrow a handle; unmounting a
/// view never destroys its Rust terminal state.
final class TerminalRegistry {
  private static let lock = NSLock()
  private static var handles: [String: UInt64] = [:]

  static func acquire(terminalId: String, columns: Int, rows: Int) -> UInt64 {
    lock.lock()
    defer { lock.unlock() }

    return ensureLocked(terminalId: terminalId, columns: columns, rows: rows)
  }

  /// Resolve a stable terminal handle for control-plane calls that can arrive
  /// before a TerminalView has attached. This does not make view lifecycle the
  /// owner of the Rust terminal and therefore does not disconnect on unmount.
  static func ensure(terminalId: String, columns: Int, rows: Int) -> UInt64 {
    lock.lock()
    defer { lock.unlock() }
    return ensureLocked(terminalId: terminalId, columns: columns, rows: rows)
  }

  static func handle(for terminalId: String) -> UInt64 {
    lock.lock()
    defer { lock.unlock() }
    if terminalId.hasPrefix("native:") { return nativeHandle(terminalId) }
    return handles[terminalId] ?? 0
  }

  private static func ensureLocked(terminalId: String, columns: Int, rows: Int) -> UInt64 {
    // Rust owns pane handles and their lifetime; a view only borrows them.
    if terminalId.hasPrefix("native:") { return nativeHandle(terminalId) }
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

  private static func nativeHandle(_ terminalId: String) -> UInt64 {
    guard let handle = UInt64(terminalId.dropFirst("native:".count)),
          handle != 0, MeetermCore.terminalExists(terminalId: handle) else { return 0 }
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
