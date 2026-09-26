import Foundation
import MeetermCoreFFI

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
  case discoveringRuntimes = 11
  case awaitingRuntimeSelection = 12
  case attachingRuntime = 13
  case creatingRuntime = 14

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
    case .discoveringRuntimes: return "DiscoveringRuntimes"
    case .awaitingRuntimeSelection: return "AwaitingRuntimeSelection"
    case .attachingRuntime: return "AttachingRuntime"
    case .creatingRuntime: return "CreatingRuntime"
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
  private static let maximumWorkspaceStateBytes = 4 * 1024 * 1024

  static func create(columns: Int, rows: Int) -> UInt64 {
    guard let columns = UInt16(exactly: columns),
          let rows = UInt16(exactly: rows) else {
      return 0
    }
    return meeterm_create_terminal(columns, rows)
  }

  /// Submit the legacy host-only SSH request. Credential strings are copied
  /// only for this call; this adapter never writes them to disk or logs them.
  /// The runtime picker performs the later backend/runtime selection.
  static func connect(
    terminalId: UInt64,
    host: String,
    port: Int,
    username: String,
    privateKey: String,
    passphrase: String,
    knownHostsPath: String,
    authMethod: String,
    password: String
  ) -> Int32 {
    guard let port = UInt16(exactly: port) else {
      return -1
    }
    return withUTF8(host) { hostPointer, hostLength in
      withUTF8(username) { usernamePointer, usernameLength in
        withUTF8(privateKey) { keyPointer, keyLength in
          withUTF8(passphrase) { passphrasePointer, passphraseLength in
            withUTF8(knownHostsPath) { pathPointer, pathLength in
              withUTF8(authMethod) { authMethodPointer, authMethodLength in
                withUTF8(password) { passwordPointer, passwordLength in
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
                    pathLength,
                    authMethodPointer,
                    authMethodLength,
                    passwordPointer,
                    passwordLength
                  )
                }
              }
            }
          }
        }
      }
    }
  }

  /// Authenticate and discover the host without binding a backend/runtime.
  /// The selected runtime is attached only by an explicit later operation.
  static func connectHost(
    terminalId: UInt64,
    host: String,
    port: Int,
    username: String,
    privateKey: String,
    passphrase: String,
    knownHostsPath: String,
    authMethod: String,
    password: String
  ) -> Int32 {
    guard let port = UInt16(exactly: port) else {
      return -1
    }
    return withUTF8(host) { hostPointer, hostLength in
      withUTF8(username) { usernamePointer, usernameLength in
        withUTF8(privateKey) { keyPointer, keyLength in
          withUTF8(passphrase) { passphrasePointer, passphraseLength in
            withUTF8(knownHostsPath) { pathPointer, pathLength in
              withUTF8(authMethod) { authMethodPointer, authMethodLength in
                withUTF8(password) { passwordPointer, passwordLength in
                  meeterm_connect_host(
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
                    pathLength,
                    authMethodPointer,
                    authMethodLength,
                    passwordPointer,
                    passwordLength
                  )
                }
              }
            }
          }
        }
      }
    }
  }

  static func disconnect(terminalId: UInt64) -> Int32 {
    meeterm_disconnect(terminalId)
  }

  static func disconnectForSwitcher(terminalId: UInt64) -> Int32 {
    meeterm_disconnect_for_switch(terminalId)
  }

  static func reconnect(terminalId: UInt64) -> Int32 {
    meeterm_reconnect(terminalId)
  }

  /// Retry retained recovery for an already validated native operation epoch.
  /// The Expo layer parses the decimal string before reaching this UInt64 API.
  static func retryRecovery(terminalId: UInt64, expectedEpoch: UInt64) -> Int32 {
    meeterm_retry_recovery(terminalId, expectedEpoch)
  }

  static func networkChanged() {
    meeterm_network_changed()
  }

  static func changeRuntime(terminalId: UInt64, expectedEpoch: UInt64) -> Int32 {
    meeterm_change_runtime(terminalId, expectedEpoch)
  }

  static func tmuxCommand(terminalId: UInt64, operation: UInt32, target: UInt64 = 0, name: String = "") -> Int32 {
    withUTF8(name) { pointer, length in meeterm_tmux_command(terminalId, operation, target, pointer, length) }
  }

  static func setForeground(terminalId: UInt64, foreground: Bool) -> Int32 {
    meeterm_set_foreground(terminalId, foreground ? 1 : 0)
  }

  static func setTerminalVisible(terminalId: UInt64, visible: Bool) -> Int32 {
    meeterm_set_terminal_visible(terminalId, visible ? 1 : 0)
  }

  static func setAutomaticReconnect(terminalId: UInt64, enabled: Bool) -> Int32 {
    meeterm_set_automatic_reconnect(terminalId, enabled ? 1 : 0)
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
          "paneName": sanitize(decode(pane.pane_name, length: pane.pane_name_len), maxLength: 256),
          "selected": pane.selected == 1,
          "active": pane.active == 1
        ]
      }
    }
    return nil
  }

  /// Read bounded backend-independent workspace metadata. Rust returns the
  /// required length when topology changes between the size and copy calls;
  /// retrying keeps this native bridge coherent without exposing terminal
  /// bytes to JavaScript.
  static func workspaceStateJSON(terminalId: UInt64) -> String? {
    guard terminalId != 0 else { return nil }
    var capacity = Int(meeterm_workspace_state_size(terminalId))
    for _ in 0..<4 {
      guard capacity > 0, capacity <= maximumWorkspaceStateBytes else { return nil }
      var data = Data(count: capacity)
      let copied = data.withUnsafeMutableBytes { (buffer: UnsafeMutableRawBufferPointer) -> Int in
        guard let address = buffer.bindMemory(to: UInt8.self).baseAddress else { return 0 }
        return Int(meeterm_workspace_state(terminalId, address, buffer.count))
      }
      if copied > capacity {
        capacity = copied
        continue
      }
      guard copied > 0, copied <= maximumWorkspaceStateBytes else { return nil }
      if copied < data.count { data.removeSubrange(copied..<data.count) }
      return String(data: data, encoding: .utf8)
    }
    return nil
  }

  /// Read bounded, sanitized runtime metadata. No executable, socket, session
  /// directory, stderr, or terminal data crosses this adapter.
  static func runtimeDiscoveryJSON(terminalId: UInt64) -> String? {
    guard terminalId != 0 else { return nil }
    var capacity = Int(meeterm_runtime_discovery_size(terminalId))
    for _ in 0..<4 {
      guard capacity > 0, capacity <= 1 * 1024 * 1024 else { return nil }
      var data = Data(count: capacity)
      let copied = data.withUnsafeMutableBytes { (buffer: UnsafeMutableRawBufferPointer) -> Int in
        guard let address = buffer.bindMemory(to: UInt8.self).baseAddress else { return 0 }
        return Int(meeterm_runtime_discovery(terminalId, address, buffer.count))
      }
      if copied > capacity { capacity = copied; continue }
      guard copied > 0, copied <= 1 * 1024 * 1024 else { return nil }
      if copied < data.count { data.removeSubrange(copied..<data.count) }
      return String(data: data, encoding: .utf8)
    }
    return nil
  }

  static func refreshRuntimes(terminalId: UInt64) -> Int32 {
    meeterm_refresh_runtimes(terminalId)
  }

  static func selectRuntime(terminalId: UInt64, candidateId: String) -> Int32 {
    withUTF8(candidateId) { pointer, length in
      meeterm_select_runtime(terminalId, pointer, length)
    }
  }

  static func createTmuxSession(terminalId: UInt64, name: String) -> Int32 {
    withUTF8(name) { pointer, length in
      meeterm_create_tmux_session(terminalId, pointer, length)
    }
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

  /// Return the current per-terminal operation epoch. Zero is reserved for an
  /// invalid/unknown native handle and is never used as an input-session epoch.
  static func operationEpoch(terminalId: UInt64) -> UInt64? {
    let epoch = meeterm_operation_epoch(terminalId)
    return epoch == 0 ? nil : epoch
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

  static func resizeAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    columns: Int,
    rows: Int
  ) -> Bool {
    guard terminalId != 0,
          let columns = UInt16(exactly: columns),
          let rows = UInt16(exactly: rows) else {
      return false
    }
    return meeterm_resize_terminal_at_epoch(terminalId, expectedEpoch, columns, rows) == 0
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
  static func commitAtEpoch(terminalId: UInt64, expectedEpoch: UInt64, text: String) -> UInt64 {
    guard terminalId != 0, !text.isEmpty, let data = text.data(using: .utf8) else {
      return 0
    }
    return data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
      guard let baseAddress = buffer.bindMemory(to: UInt8.self).baseAddress else {
        return 0
      }
      return meeterm_commit_utf8_at_epoch(
        terminalId,
        expectedEpoch,
        baseAddress,
        buffer.count
      )
    }
  }

  @discardableResult
  static func send(terminalId: UInt64, key: TerminalSpecialKey) -> Bool {
    guard terminalId != 0 else {
      return false
    }
    return meeterm_send_key(terminalId, key.rawValue, 0) >= 0
  }

  static func sendSpecialAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    key: TerminalSpecialKey
  ) -> Bool {
    guard terminalId != 0 else { return false }
    return meeterm_send_special_key_at_epoch(terminalId, expectedEpoch, key.rawValue) > 0
  }

  /// Compatibility spelling for native callers that use the shorter send API.
  static func sendAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    key: TerminalSpecialKey
  ) -> Bool {
    sendSpecialAtEpoch(terminalId: terminalId, expectedEpoch: expectedEpoch, key: key)
  }

  static func sendKey(terminalId: UInt64, key: TerminalSpecialKey, modifiers: UInt32) -> Bool {
    meeterm_send_key(terminalId, key.rawValue, modifiers) >= 0
  }

  static func sendKeyAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    key: TerminalSpecialKey,
    modifiers: UInt32
  ) -> Bool {
    guard terminalId != 0 else { return false }
    return meeterm_send_key_at_epoch(terminalId, expectedEpoch, key.rawValue, modifiers) > 0
  }

  static func commitModified(terminalId: UInt64, text: String, modifiers: UInt32) -> Bool {
    let result = withUTF8(text) { pointer, length in
      meeterm_commit_modified_utf8(terminalId, pointer, length, modifiers)
    }
    return result >= 0
  }

  static func commitModifiedAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    text: String,
    modifiers: UInt32
  ) -> Bool {
    let result = withUTF8(text) { pointer, length in
      meeterm_commit_modified_utf8_at_epoch(
        terminalId,
        expectedEpoch,
        pointer,
        length,
        modifiers
      )
    }
    return result > 0
  }

  static func selectStart(terminalId: UInt64, row: Int, column: Int) -> Bool {
    guard let row = UInt32(exactly: row), let column = UInt32(exactly: column) else { return false }
    return meeterm_select_start(terminalId, row, column) == 0
  }

  static func selectUpdate(terminalId: UInt64, row: Int, column: Int) -> Bool {
    guard let row = UInt32(exactly: row), let column = UInt32(exactly: column) else { return false }
    return meeterm_select_update(terminalId, row, column) == 0
  }

  @discardableResult static func clearSelection(terminalId: UInt64) -> Bool {
    meeterm_clear_selection(terminalId) == 0
  }

  static func selectionText(terminalId: UInt64) -> String? {
    var capacity = meeterm_selection_text(terminalId, nil, 0)
    for _ in 0..<3 {
      guard capacity > 0, capacity <= 4 * 1024 * 1024 else { return nil }
      var bytes = [UInt8](repeating: 0, count: capacity)
      let copied = bytes.withUnsafeMutableBufferPointer { meeterm_selection_text(terminalId, $0.baseAddress, $0.count) }
      // The pane can disappear between the size query and the copy. C size_t
      // imports as Int here, so Rust's SIZE_MAX error sentinel is negative.
      guard copied >= 0, copied <= 4 * 1024 * 1024 else { return nil }
      if copied > capacity { capacity = copied; continue }
      return String(bytes: bytes.prefix(copied), encoding: .utf8)
    }
    return nil
  }

  @discardableResult static func setTheme(terminalId: UInt64, light: Bool) -> Bool {
    meeterm_set_theme(terminalId, light ? 1 : 0) == 0
  }

  @discardableResult static func setScrollbackLimit(_ lines: Int) -> Bool {
    guard let lines = UInt32(exactly: lines) else { return false }
    return meeterm_set_scrollback_limit(lines) == 0
  }

  static func paste(terminalId: UInt64, text: String) -> Bool {
    let bytes = Array(text.utf8)
    return bytes.withUnsafeBufferPointer { buffer in
      meeterm_paste_utf8(terminalId, buffer.baseAddress, buffer.count) >= 0
    }
  }

  static func pasteAtEpoch(terminalId: UInt64, expectedEpoch: UInt64, text: String) -> Bool {
    let bytes = Array(text.utf8)
    return bytes.withUnsafeBufferPointer { buffer in
      meeterm_paste_utf8_at_epoch(
        terminalId,
        expectedEpoch,
        buffer.baseAddress,
        buffer.count
      ) > 0
    }
  }

  static func scroll(terminalId: UInt64, lines: Int32) -> Bool {
    meeterm_scroll_lines(terminalId, lines) == 0
  }

  static func scrollAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    lines: Int32
  ) -> Bool {
    meeterm_scroll_lines_at_epoch(terminalId, expectedEpoch, lines) == 0
  }

  static func sendBytesAtEpoch(
    terminalId: UInt64,
    expectedEpoch: UInt64,
    bytes: [UInt8]
  ) -> Bool {
    bytes.withUnsafeBufferPointer { buffer in
      meeterm_send_bytes_at_epoch(
        terminalId,
        expectedEpoch,
        buffer.baseAddress,
        buffer.count
      ) >= 0
    }
  }

  @discardableResult
  static func destroy(terminalId: UInt64) -> Bool {
    terminalId != 0 && meeterm_destroy_terminal(terminalId) == 1
  }

  // Issue #28 attachment contract (attachment-ffi.md + Main amendments).
  // The core owns the SFTP operation, remote path, destination fence, and the
  // single-line insert; this adapter only passes the local file and polls the
  // fixed-size snapshot. An empty remoteDirectory selects the core default
  // `~/.local/share/meeterm/attachments`.

  /// `meeterm_attachment_begin`: attachment id (>0), or 0 on synchronous
  /// rejection (poll-free errors such as no connection or unreadable file).
  static func attachmentBegin(
    terminalId: UInt64,
    localPath: String,
    displayName: String,
    remoteDirectory: String,
    sizeBytes: UInt64
  ) -> UInt64 {
    Data(localPath.utf8).withUnsafeBytes { path in
      Data(displayName.utf8).withUnsafeBytes { name in
        Data(remoteDirectory.utf8).withUnsafeBytes { directory in
          meeterm_attachment_begin(
            terminalId,
            path.bindMemory(to: UInt8.self).baseAddress,
            path.count,
            name.bindMemory(to: UInt8.self).baseAddress,
            name.count,
            directory.bindMemory(to: UInt8.self).baseAddress,
            directory.count,
            sizeBytes
          )
        }
      }
    }
  }

  /// `meeterm_attachment_retry_upload`: 0 accepted, negative = error code.
  static func attachmentRetryUpload(terminalId: UInt64, attachmentId: UInt64) -> Int32 {
    meeterm_attachment_retry_upload(terminalId, attachmentId)
  }

  /// `meeterm_attachment_insert`: 0 accepted, negative = error code.
  static func attachmentInsert(terminalId: UInt64, attachmentId: UInt64) -> Int32 {
    meeterm_attachment_insert(terminalId, attachmentId)
  }

  /// `meeterm_attachment_cancel`: 0 accepted, negative = error code.
  static func attachmentCancel(attachmentId: UInt64) -> Int32 {
    meeterm_attachment_cancel(attachmentId)
  }

  /// `meeterm_attachment_dispose`: 0 accepted, negative = error code.
  static func attachmentDispose(attachmentId: UInt64) -> Int32 {
    meeterm_attachment_dispose(attachmentId)
  }

  /// `meeterm_attachment_delete_remote`: 0 accepted, negative = error code.
  static func attachmentDeleteRemote(terminalId: UInt64, attachmentId: UInt64) -> Int32 {
    meeterm_attachment_delete_remote(terminalId, attachmentId)
  }

  /// `meeterm_attachment_snapshot_size`: the ABI record size.
  static func attachmentSnapshotSize() -> Int {
    meeterm_attachment_snapshot_size()
  }

  /// `meeterm_attachment_snapshot`: raw record bytes for the pure codec.
  /// The record size stays consistent with `meetterm_attachment_snapshot_size`.
  static func attachmentSnapshot(attachmentId: UInt64) -> Data? {
    var native = meeterm_attachment_snapshot_t()
    guard meeterm_attachment_snapshot(attachmentId, &native) == 0 else {
      return nil
    }
    return withUnsafeBytes(of: &native) { Data($0) }
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
