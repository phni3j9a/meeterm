import ExpoModulesCore

public final class MeetermTerminalModule: Module {
  public func definition() -> ModuleDefinition {
    Name("MeetermTerminal")

    AsyncFunction("connect") { (terminalId: String, options: [String: Any]) throws in
      let normalizedId = try Self.normalizeTerminalId(terminalId)
      let connection = try Self.decodeOptions(options)
      let handle = try Self.ensureHandle(normalizedId)
      let knownHostsPath: String
      do {
        knownHostsPath = try KnownHostsStore.path()
      } catch {
        throw Self.error("SSH trust storage is unavailable.")
      }

      let result = MeetermCore.connect(
        terminalId: handle,
        host: connection.host,
        port: connection.port,
        username: connection.username,
        privateKey: connection.privateKey,
        passphrase: connection.passphrase,
        knownHostsPath: knownHostsPath,
        authMethod: connection.authMethod,
        password: connection.password
      )
      guard result == 0 else {
        throw Self.error("The SSH connection could not be started.")
      }
    }

    AsyncFunction("disconnect") { (terminalId: String) throws in
      let normalizedId = try Self.normalizeTerminalId(terminalId)
      let handle = try Self.ensureHandle(normalizedId)
      guard MeetermCore.disconnect(terminalId: handle) == 0 else {
        throw Self.error("The SSH disconnect request could not be sent.")
      }
    }

    AsyncFunction("getConnectionState") { (terminalId: String) throws -> [String: Any] in
      let normalizedId = try Self.normalizeTerminalId(terminalId)
      let handle = try Self.ensureHandle(normalizedId)
      guard let snapshot = MeetermCore.connectionSnapshot(terminalId: handle) else {
        throw Self.error("Native connection state is unavailable.")
      }
      return Self.expoRecord(snapshot)
    }

    AsyncFunction("reconnect") { (terminalId: String) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.reconnect(terminalId: handle) == 0 else {
        throw Self.error("The reconnect request could not be started.")
      }
    }

    AsyncFunction("selectPane") { (terminalId: String, paneId: String) throws in
      guard paneId.first == "%", !paneId.dropFirst().isEmpty,
            paneId.dropFirst().allSatisfy({ $0.isASCII && $0.isNumber }),
            let pane = UInt64(paneId.dropFirst()) else {
        throw Self.error("The pane ID is invalid.")
      }
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.selectPane(terminalId: handle, paneId: pane) == 0 else {
        throw Self.error("The terminal could not be selected.")
      }
    }

    AsyncFunction("getSessionState") { (terminalId: String) throws -> [String: Any] in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard let panes = MeetermCore.sessionPanes(terminalId: handle) else {
        throw Self.error("Native session state is unavailable.")
      }
      return ["panes": panes]
    }

    AsyncFunction("respondToHostKey") {
      (terminalId: String, fingerprint: String, accept: Bool) throws in
      let normalizedId = try Self.normalizeTerminalId(terminalId)
      guard !fingerprint.isEmpty, !Self.containsControl(fingerprint) else {
        throw Self.error("The host-key response is invalid.")
      }
      let handle = try Self.ensureHandle(normalizedId)
      guard MeetermCore.respondToHostKey(
        terminalId: handle,
        fingerprint: fingerprint,
        accept: accept
      ) == 0 else {
        throw Self.error("The host-key response could not be sent.")
      }
    }

    AsyncFunction("forgetHostKey") { (host: String, port: Int) throws in
      let normalizedHost = host.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !normalizedHost.isEmpty, !Self.containsControl(normalizedHost),
            (1...65535).contains(port) else {
        throw Self.error("The host-key endpoint is invalid.")
      }
      let knownHostsPath: String
      do {
        knownHostsPath = try KnownHostsStore.path()
      } catch {
        throw Self.error("SSH trust storage is unavailable.")
      }
      guard MeetermCore.forgetHostKey(
        host: normalizedHost,
        port: port,
        knownHostsPath: knownHostsPath
      ) == 0 else {
        throw Self.error("The trusted host key could not be removed.")
      }
    }

    View(MeetermTerminalView.self) {
      Prop("terminalId", "poc-main") { (view: MeetermTerminalView, terminalId: String) in
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")
    }
  }

  private struct SshOptions {
    let host: String
    let port: Int
    let username: String
    let authMethod: String
    let privateKey: String
    let passphrase: String
    let password: String
  }

  private static func decodeOptions(_ values: [String: Any]) throws -> SshOptions {
    guard let host = (values["host"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
          let username = (values["username"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
          let port = integer(values["port"]),
          !host.isEmpty,
          !username.isEmpty,
          !containsControl(host),
          !containsControl(username),
          (1...65535).contains(port) else {
      throw error("The SSH connection options are invalid.")
    }

    let authMethod: String
    if values.keys.contains("authMethod") {
      guard let value = values["authMethod"] as? String,
            value == "publicKey" || value == "password" else {
        throw error("The SSH connection options are invalid.")
      }
      authMethod = value
    } else {
      authMethod = "publicKey"
    }

    switch authMethod {
    case "publicKey":
      guard let privateKey = values["privateKey"] as? String,
            let passphrase = values["passphrase"] as? String,
            !privateKey.isEmpty,
            !privateKey.utf8.contains(0),
            !passphrase.utf8.contains(0) else {
        throw error("The SSH connection options are invalid.")
      }
      return SshOptions(
        host: host,
        port: port,
        username: username,
        authMethod: authMethod,
        privateKey: privateKey,
        passphrase: passphrase,
        password: ""
      )
    case "password":
      guard let password = values["password"] as? String,
            !password.isEmpty,
            !password.utf8.contains(0) else {
        throw error("The SSH connection options are invalid.")
      }
      return SshOptions(
        host: host,
        port: port,
        username: username,
        authMethod: authMethod,
        privateKey: "",
        passphrase: "",
        password: password
      )
    default:
      throw error("The SSH connection options are invalid.")
    }
  }

  private static func integer(_ value: Any?) -> Int? {
    if let value = value as? Int {
      return value
    }
    if value is Bool {
      return nil
    }
    if let value = value as? NSNumber {
      let double = value.doubleValue
      guard double.isFinite, double.rounded(.towardZero) == double else {
        return nil
      }
      let integer = value.intValue
      return Double(integer) == double ? integer : nil
    }
    return nil
  }

  private static func ensureHandle(_ terminalId: String) throws -> UInt64 {
    let existing = TerminalRegistry.handle(for: terminalId)
    if existing != 0 {
      return existing
    }
    let created = TerminalRegistry.ensure(
      terminalId: terminalId,
      columns: 80,
      rows: 24
    )
    guard created != 0 else {
      throw error("The native terminal could not be created.")
    }
    return created
  }

  private static func normalizeTerminalId(_ value: String) throws -> String {
    let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !normalized.isEmpty, !containsControl(normalized) else {
      throw error("The terminal ID is invalid.")
    }
    return normalized
  }

  private static func expoRecord(_ snapshot: MeetermConnectionSnapshot) -> [String: Any] {
    [
      "state": snapshot.state.jsValue,
      "host": snapshot.host,
      "port": snapshot.port,
      "fingerprint": snapshot.fingerprint,
      "algorithm": snapshot.algorithm,
      "knownFingerprint": snapshot.knownFingerprint,
      "errorCode": snapshot.errorCode,
      "errorMessage": snapshot.errorMessage
    ]
  }

  private static func containsControl(_ value: String) -> Bool {
    value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
  }

  private static func error(_ message: String) -> NSError {
    NSError(
      domain: "dev.meeterm.terminal",
      code: 1,
      userInfo: [NSLocalizedDescriptionKey: message]
    )
  }
}
