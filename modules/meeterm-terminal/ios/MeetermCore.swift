import Foundation
import MeetermCoreFFI

enum TerminalSpecialKey: UInt32 {
  case escape = 0
  case tab = 1
  case enter = 2
  case backspace = 3
  case up = 4
  case down = 5
  case left = 6
  case right = 7
}

/// Thin, native-only access to the Rust C ABI. No snapshot or input bytes are
/// exposed through the Expo module/JavaScript boundary.
enum MeetermCore {
  private static let maximumSnapshotBytes = 64 * 1024 * 1024

  static func create(columns: Int, rows: Int) -> UInt64 {
    guard let columns = UInt16(exactly: columns),
          let rows = UInt16(exactly: rows) else {
      return 0
    }
    return meeterm_create_terminal(columns, rows)
  }

  static func snapshot(terminalId: UInt64) -> Data? {
    guard terminalId != 0 else {
      return nil
    }

    // A resize may occur between the size query and copy. Retry with the
    // required capacity reported by Rust instead of accepting a partial frame.
    var capacity = Int(meeterm_snapshot_size(terminalId))
    for _ in 0..<3 {
      guard capacity > 0, capacity <= maximumSnapshotBytes else {
        return nil
      }

      var data = Data(count: capacity)
      let copied = data.withUnsafeMutableBytes { (buffer: UnsafeMutableRawBufferPointer) -> Int in
        guard let baseAddress = buffer.bindMemory(to: UInt8.self).baseAddress else {
          return 0
        }
        return Int(meeterm_snapshot(terminalId, baseAddress, buffer.count))
      }

      if copied > capacity {
        capacity = copied
        continue
      }
      guard copied > 0 else {
        return nil
      }
      if copied < data.count {
        data.removeSubrange(copied..<data.count)
      }
      return data
    }
    return nil
  }

  static func resize(terminalId: UInt64, columns: Int, rows: Int) -> Bool {
    guard terminalId != 0,
          let columns = UInt16(exactly: columns),
          let rows = UInt16(exactly: rows) else {
      return false
    }
    return meeterm_resize_terminal(terminalId, columns, rows) == 0
  }

  @discardableResult
  static func commit(terminalId: UInt64, text: String) -> UInt64 {
    guard terminalId != 0, !text.isEmpty, let data = text.data(using: .utf8) else {
      return 0
    }
    return data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
      guard let baseAddress = buffer.bindMemory(to: UInt8.self).baseAddress else {
        return 0
      }
      return meeterm_commit_utf8(terminalId, baseAddress, buffer.count)
    }
  }

  @discardableResult
  static func send(terminalId: UInt64, key: TerminalSpecialKey) -> Bool {
    guard terminalId != 0 else {
      return false
    }
    return meeterm_send_special_key(terminalId, key.rawValue) >= 0
  }

  @discardableResult
  static func destroy(terminalId: UInt64) -> Bool {
    terminalId != 0 && meeterm_destroy_terminal(terminalId) == 1
  }
}
