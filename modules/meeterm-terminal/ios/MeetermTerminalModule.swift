import ExpoModulesCore

public final class MeetermTerminalModule: Module {
  public func definition() -> ModuleDefinition {
    Name("MeetermTerminal")

    AsyncFunction("getProfiles") { () throws -> [[String: Any]] in try ClientStore.profiles() }
    AsyncFunction("saveProfile") { (profile: [String: Any], credential: [String: Any]?, keepCredential: Bool) throws -> [String: Any] in
      try ClientStore.saveProfile(profile, credential: credential, keepCredential: keepCredential)
    }
    AsyncFunction("deleteProfile") { (profileId: String) throws in try ClientStore.deleteProfile(profileId) }
    AsyncFunction("connectProfile") { (terminalId: String, profileId: String) throws in
      try Self.connectOptions(terminalId, options: ClientStore.connectionOptions(profileId))
    }
    AsyncFunction("getPreferences") { () throws -> [String: Any] in
      let preferences = try ClientStore.preferences()
      guard MeetermCore.setScrollbackLimit(preferences["scrollbackLines"] as! Int) else { throw Self.error("The history preference could not be applied.") }
      return preferences
    }
    AsyncFunction("setPreferences") { (preferences: [String: Any]) throws in
      let validated = try ClientStore.validatePreferences(preferences)
      try ClientStore.setPreferences(validated)
      guard MeetermCore.setScrollbackLimit(validated["scrollbackLines"] as! Int) else { throw Self.error("The history preference could not be applied.") }
    }

    AsyncFunction("setForeground") { (terminalId: String, foreground: Bool) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.setForeground(terminalId: handle, foreground: foreground) == 0 else {
        throw Self.error("The app lifecycle could not be updated.")
      }
    }
    AsyncFunction("setTerminalVisible") { (terminalId: String, visible: Bool) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.setTerminalVisible(terminalId: handle, visible: visible) == 0 else {
        throw Self.error("The terminal visibility could not be updated.")
      }
    }
    AsyncFunction("setAutomaticReconnect") { (terminalId: String, enabled: Bool) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.setAutomaticReconnect(terminalId: handle, enabled: enabled) == 0 else {
        throw Self.error("The reconnect preference could not be updated.")
      }
    }
    AsyncFunction("createWorkspace") { (terminalId: String, name: String) throws in try Self.tmuxCommand(terminalId, operation: 0, name: name) }
    AsyncFunction("renameWorkspace") { (terminalId: String, windowId: String, name: String) throws in
      try Self.tmuxCommand(terminalId, operation: 1, target: Self.targetId(windowId, prefix: "@"), name: name)
    }
    AsyncFunction("closeWorkspace") { (terminalId: String, windowId: String) throws in
      try Self.tmuxCommand(terminalId, operation: 2, target: Self.targetId(windowId, prefix: "@"))
    }
    AsyncFunction("createPane") { (terminalId: String, windowId: String) throws in
      try Self.tmuxCommand(terminalId, operation: 3, target: Self.targetId(windowId, prefix: "@"))
    }
    AsyncFunction("renamePane") { (terminalId: String, paneId: String, name: String) throws in
      try Self.tmuxCommand(terminalId, operation: 4, target: Self.targetId(paneId, prefix: "%"), name: name)
    }
    AsyncFunction("closePane") { (terminalId: String, paneId: String) throws in
      try Self.tmuxCommand(terminalId, operation: 5, target: Self.targetId(paneId, prefix: "%"))
    }
    AsyncFunction("refreshTerminal") { (terminalId: String) throws in try Self.tmuxCommand(terminalId, operation: 6) }
    AsyncFunction("createGroup") { (terminalId: String, workspaceId: String, name: String) throws in
      try Self.tmuxCommand(terminalId, operation: 7, target: Self.numericId(workspaceId), name: name)
    }
    AsyncFunction("renameGroup") { (terminalId: String, groupId: String, name: String) throws in
      try Self.tmuxCommand(terminalId, operation: 8, target: Self.numericId(groupId), name: name)
    }
    AsyncFunction("closeGroup") { (terminalId: String, groupId: String) throws in
      try Self.tmuxCommand(terminalId, operation: 9, target: Self.numericId(groupId))
    }
    AsyncFunction("selectGroup") { (terminalId: String, groupId: String) throws in
      try Self.tmuxCommand(terminalId, operation: 10, target: Self.numericId(groupId))
    }
    AsyncFunction("getWorkspaceState") { (terminalId: String) throws -> [String: Any] in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard let json = MeetermCore.workspaceStateJSON(terminalId: handle),
            let data = json.data(using: .utf8),
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw Self.error("Native workspace state is unavailable.")
      }
      return value
    }

    AsyncFunction("connect") { (terminalId: String, options: [String: Any]) throws in
      try Self.connectOptions(terminalId, options: options)
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
      let pane = try Self.targetId(paneId, prefix: "%")
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
      Prop("fontSize", 15.0) { (view: MeetermTerminalView, value: Double) in view.setFontSize(value) }
      Prop("theme", "dark") { (view: MeetermTerminalView, value: String) in view.setTheme(value) }
      Prop("scrollbackLines", 10000) { (view: MeetermTerminalView, value: Int) in view.setScrollbackLines(value) }
      Prop("terminalId", "poc-main") { (view: MeetermTerminalView, terminalId: String) in
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")
    }
  }

  private static func connectOptions(_ terminalId: String, options: [String: Any]) throws {
    let connection = try decodeOptions(options)
    let handle = try ensureHandle(normalizeTerminalId(terminalId))
    let preferences = try ClientStore.preferences()
    guard MeetermCore.setScrollbackLimit(preferences["scrollbackLines"] as! Int),
          MeetermCore.setAutomaticReconnect(terminalId: handle, enabled: preferences["automaticReconnect"] as! Bool) == 0 else {
      throw error("The connection preferences could not be applied.")
    }
    let knownHostsPath: String
    do { knownHostsPath = try KnownHostsStore.path() }
    catch { throw Self.error("SSH trust storage is unavailable.") }
    let result = MeetermCore.connectBackend(terminalId: handle, host: connection.host, port: connection.port,
      username: connection.username, privateKey: connection.privateKey, passphrase: connection.passphrase,
      knownHostsPath: knownHostsPath, authMethod: connection.authMethod, password: connection.password,
      backend: connection.backend, runtime: connection.runtime)
    guard result == 0 else { throw Self.error("The SSH connection could not be started.") }
  }

  private static func targetId(_ value: String, prefix: Character) throws -> UInt64 {
    let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines)
    let digits = normalized.first == prefix ? String(normalized.dropFirst()) : normalized
    guard !digits.isEmpty, digits.allSatisfy({ $0.isASCII && $0.isNumber }),
          let id = UInt64(digits) else { throw error("The tmux target is invalid.") }
    return id
  }

  private static func numericId(_ value: String) throws -> UInt64 {
    let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !normalized.isEmpty, normalized.allSatisfy({ $0.isASCII && $0.isNumber }),
          let id = UInt64(normalized) else { throw error("The workspace group ID is invalid.") }
    return id
  }

  private static func tmuxCommand(_ terminalId: String, operation: UInt32, target: UInt64 = 0, name: String = "") throws {
    let handle = try ensureHandle(normalizeTerminalId(terminalId))
    guard MeetermCore.tmuxCommand(terminalId: handle, operation: operation, target: target, name: name) == 0 else {
      throw error("The workspace operation could not be started. Check the connection and try again.")
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
    let backend: String
    let runtime: String
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

    guard (!values.keys.contains("backend") || values["backend"] is String),
          (!values.keys.contains("runtime") || values["runtime"] is String) else { throw error("The backend or runtime is invalid.") }
    let backend = values["backend"] as? String ?? "tmux"
    guard backend == "tmux" || backend == "herdr" else {
      throw error("The SSH connection options are invalid.")
    }
    let runtime = values["runtime"] as? String ?? ""
    try validateRuntime(backend: backend, runtime: runtime)

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
        password: "",
        backend: backend,
        runtime: runtime
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
        password: password,
        backend: backend,
        runtime: runtime
      )
    default:
      throw error("The SSH connection options are invalid.")
    }
  }

  private static func validateRuntime(backend: String, runtime: String) throws {
    let validCharacters = runtime.unicodeScalars.allSatisfy { scalar in
      (scalar.value >= 0x41 && scalar.value <= 0x5A) ||
      (scalar.value >= 0x61 && scalar.value <= 0x7A) ||
      (scalar.value >= 0x30 && scalar.value <= 0x39) ||
      scalar.value == 0x2E || scalar.value == 0x5F || scalar.value == 0x2D
    }
    guard runtime.utf8.count <= 64, runtime != ".", runtime != "..", validCharacters,
          backend == "herdr" || runtime.isEmpty else {
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
