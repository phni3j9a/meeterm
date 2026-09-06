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

enum MeetermConnectionPhase: UInt32 {
  case disconnected = 0
  case connecting = 1
  case hostKeyPending = 2
  case authenticating = 3
  case openingPty = 4
  case ready = 5
  case closing = 6
  case failed = 7
  case attachingTmux = 8
  case synchronizing = 9
  case reconnecting = 10

  var jsValue: String {
    switch self {
    case .disconnected: return "Disconnected"
    case .connecting: return "Connecting"
    case .hostKeyPending: return "HostKeyPending"
    case .authenticating: return "Authenticating"
    case .openingPty: return "OpeningPty"
    case .ready: return "Ready"
    case .closing: return "Closing"
    case .failed: return "Failed"
    case .attachingTmux: return "AttachingTmux"
    case .synchronizing: return "Synchronizing"
    case .reconnecting: return "Reconnecting"
    }
  }
}

struct MeetermConnectionSnapshot {
  let state: MeetermConnectionPhase
  let host: String
  let port: Int
  let fingerprint: String
  let algorithm: String
  let knownFingerprint: String
  let errorCode: String
  let errorMessage: String

  static let disconnected = MeetermConnectionSnapshot(
    state: .disconnected,
    host: "",
    port: 0,
    fingerprint: "",
    algorithm: "",
    knownFingerprint: "",
    errorCode: "",
    errorMessage: ""
  )
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

  /// Submit a native SSH request. The key and passphrase are copied only for
  /// this call; this adapter never writes either value to disk or logs it.
  static func connect(
    terminalId: UInt64,
    host: String,
    port: Int,
    username: String,
    privateKey: String,
    passphrase: String,
    knownHostsPath: String
  ) -> Int32 {
    guard let port = UInt16(exactly: port) else {
      return -1
    }
    return withUTF8(host) { hostPointer, hostLength in
      withUTF8(username) { usernamePointer, usernameLength in
        withUTF8(privateKey) { keyPointer, keyLength in
          withUTF8(passphrase) { passphrasePointer, passphraseLength in
            withUTF8(knownHostsPath) { pathPointer, pathLength in
              meeterm_connect(
                terminalId,
                hostPointer,
                hostLength,
                port,
                usernamePointer,
                usernameLength,
                keyPointer,
                keyLength,
                passphrasePointer,
                passphraseLength,
                pathPointer,
                pathLength
              )
            }
          }
        }
      }
    }
  }

  static func disconnect(terminalId: UInt64) -> Int32 {
    meeterm_disconnect(terminalId)
  }

  static func reconnect(terminalId: UInt64) -> Int32 {
    meeterm_reconnect(terminalId)
  }

  static func selectPane(terminalId: UInt64, paneId: UInt64) -> Int32 {
    meeterm_select_pane(terminalId, paneId)
  }

  static func terminalExists(terminalId: UInt64) -> Bool {
    meeterm_terminal_exists(terminalId) == 1
  }

  static func sessionPanes(terminalId: UInt64) -> [[String: Any]]? {
    guard meeterm_pane_record_size() == MemoryLayout<meeterm_tmux_pane_t>.stride else { return nil }
    var capacity = meeterm_session_panes(terminalId, nil, 0)
    for _ in 0..<3 {
      guard capacity >= 0, capacity <= 4096 else { return nil }
      if capacity == 0 { return [] }
      var panes = Array(repeating: meeterm_tmux_pane_t(), count: capacity)
      let copied = panes.withUnsafeMutableBufferPointer { buffer in
        meeterm_session_panes(terminalId, buffer.baseAddress, buffer.count)
      }
      guard copied >= 0, copied <= 4096 else { return nil }
      if copied > capacity { capacity = copied; continue }
      return panes.prefix(copied).map { pane in
        [
          "windowId": "@\(pane.window_id)",
          "paneId": "%\(pane.pane_id)",
          "terminalId": "native:\(pane.terminal_id)",
          "windowName": sanitize(decode(pane.window_name, length: pane.window_name_len), maxLength: 256),
          "selected": pane.selected == 1
        ]
      }
    }
    return nil
  }

  static func connectionSnapshot(terminalId: UInt64) -> MeetermConnectionSnapshot? {
    guard terminalId != 0 else {
      return nil
    }

    guard meeterm_connection_snapshot_size() == MemoryLayout<meeterm_ssh_connection_state_t>.size else {
      return nil
    }

    var native = meeterm_ssh_connection_state_t()
    let result = withUnsafeMutablePointer(to: &native) { pointer in
      meeterm_connection_snapshot(terminalId, pointer)
    }
    guard result == 0 else {
      return nil
    }

    guard let phase = MeetermConnectionPhase(rawValue: native.state),
          native.host_len <= 256,
          native.fingerprint_len <= 128,
          native.algorithm_len <= 64,
          native.known_fingerprint_len <= 128,
          native.error_code_len <= 64,
          native.error_message_len <= 256 else {
      return nil
    }
    let host = decode(native.host, length: native.host_len)
    let fingerprint = decode(native.fingerprint, length: native.fingerprint_len)
    let algorithm = decode(native.algorithm, length: native.algorithm_len)
    let knownFingerprint = decode(
      native.known_fingerprint,
      length: native.known_fingerprint_len
    )
    let errorCode = decode(native.error_code, length: native.error_code_len)
    let errorMessage = decode(
      native.error_message,
      length: native.error_message_len
    )

    return MeetermConnectionSnapshot(
      state: phase,
      host: sanitize(host, maxLength: 256),
      port: Int(native.port),
      fingerprint: sanitize(fingerprint, maxLength: 128),
      algorithm: sanitize(algorithm, maxLength: 64),
      knownFingerprint: sanitize(knownFingerprint, maxLength: 128),
      errorCode: sanitizeErrorCode(errorCode),
      errorMessage: sanitize(errorMessage, maxLength: 256)
    )
  }

  static func respondToHostKey(
    terminalId: UInt64,
    fingerprint: String,
    accept: Bool
  ) -> Int32 {
    withUTF8(fingerprint) { pointer, length in
      meeterm_respond_host_key(
        terminalId,
        pointer,
        length,
        accept ? UInt8(1) : UInt8(0)
      )
    }
  }

  static func forgetHostKey(host: String, port: Int, knownHostsPath: String) -> Int32 {
    guard let port = UInt16(exactly: port) else {
      return -1
    }
    return withUTF8(host) { hostPointer, hostLength in
      withUTF8(knownHostsPath) { pathPointer, pathLength in
        meeterm_forget_host_key(
          hostPointer,
          hostLength,
          port,
          pathPointer,
          pathLength
        )
      }
    }
  }

  static func terminalRevision(terminalId: UInt64) -> UInt64 {
    meeterm_terminal_revision(terminalId)
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

  private static func withUTF8(
    _ value: String,
    _ body: (UnsafePointer<UInt8>?, Int) -> Int32
  ) -> Int32 {
    let data = Data(value.utf8)
    return data.withUnsafeBytes { buffer in
      let bytes = buffer.bindMemory(to: UInt8.self)
      return body(bytes.baseAddress, bytes.count)
    }
  }

  private static func decode<T>(_ value: T, length: UInt16) -> String {
    withUnsafeBytes(of: value) { buffer in
      let count = min(Int(length), buffer.count)
      return String(decoding: buffer.prefix(count), as: UTF8.self)
    }
  }

  private static func sanitize(_ value: String, maxLength: Int) -> String {
    value
      .unicodeScalars
      .filter { !CharacterSet.controlCharacters.contains($0) }
      .prefix(maxLength)
      .reduce(into: "") { result, scalar in result.unicodeScalars.append(scalar) }
  }

  private static func sanitizeErrorCode(_ value: String) -> String {
    let code = sanitize(value, maxLength: 64)
    guard !code.isEmpty,
          code.unicodeScalars.allSatisfy({ scalar in
            scalar.value >= 0x61 && scalar.value <= 0x7A ||
            scalar.value >= 0x30 && scalar.value <= 0x39 ||
            scalar.value == 0x5F
          }) else {
      return code.isEmpty ? "" : "native_error"
    }
    return code
  }
}
