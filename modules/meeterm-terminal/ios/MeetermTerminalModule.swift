import ExpoModulesCore

public final class MeetermTerminalModule: Module {
  public func definition() -> ModuleDefinition {
    Name("MeetermTerminal")

    // This is a smoke-only, fixed-enum sink. It is intentionally a no-op for
    // normal launches and for values outside the allowlist.
    Function("recordStartupPhase") { (phase: String) in
      Self.recordStartupPhase(phase)
    }

    AsyncFunction("getProfiles") { () throws -> [[String: Any]] in try ClientStore.profiles() }
    AsyncFunction("saveProfile") { (profile: [String: Any], credential: [String: Any]?, keepCredential: Bool) throws -> [String: Any] in
      try ClientStore.saveProfile(profile, credential: credential, keepCredential: keepCredential)
    }
    AsyncFunction("deleteProfile") { (profileId: String) throws in try ClientStore.deleteProfile(profileId) }
    // Every fresh connection path authenticates the SSH host first. Persisted
    // backend/runtime values are only last-used hints and never select a
    // runtime on behalf of the caller.
    AsyncFunction("connectHost") { (terminalId: String, options: [String: Any]) throws in
      try Self.connectHostOptions(terminalId, options: options)
    }
    AsyncFunction("connectProfileHost") { (terminalId: String, profileId: String) throws in
      var options = try ClientStore.connectionOptions(profileId)
      // Persisted backend/runtime values are display/sort hints only. They are
      // deliberately removed before host authentication.
      options.removeValue(forKey: "backend")
      options.removeValue(forKey: "runtime")
      try Self.connectHostOptions(terminalId, options: options)
    }
    AsyncFunction("connectProfile") { (terminalId: String, profileId: String) throws in
      try Self.connectHostOptions(terminalId, options: ClientStore.connectionOptions(profileId))
    }
    AsyncFunction("setLastUsedRuntime") { (profileId: String, backend: String, runtime: String) throws -> [String: Any] in
      try ClientStore.setLastUsedRuntime(profileId, backend: backend, runtime: runtime)
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
      try Self.connectHostOptions(terminalId, options: options)
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

    AsyncFunction("getRuntimeDiscovery") { (terminalId: String) throws -> [String: Any] in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard let json = MeetermCore.runtimeDiscoveryJSON(terminalId: handle),
            let data = json.data(using: .utf8),
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw Self.error("Native runtime discovery is unavailable.")
      }
      return try Self.runtimeDiscoveryRecord(value)
    }

    AsyncFunction("refreshRuntimes") { (terminalId: String) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.refreshRuntimes(terminalId: handle) == 0 else {
        throw Self.error("Runtime discovery could not be refreshed.")
      }
    }

    AsyncFunction("selectRuntime") { (terminalId: String, candidateId: String) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard !candidateId.isEmpty, !Self.containsControl(candidateId), candidateId.utf8.count <= 256,
            MeetermCore.selectRuntime(terminalId: handle, candidateId: candidateId) == 0 else {
        throw Self.error("The selected runtime could not be opened.")
      }
    }

    AsyncFunction("createTmuxSession") { (terminalId: String, name: String) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard !name.isEmpty, name.utf8.count <= 64, !Self.containsControl(name),
            MeetermCore.createTmuxSession(terminalId: handle, name: name) == 0 else {
        throw Self.error("The tmux session could not be created.")
      }
    }

    AsyncFunction("runtimeBrowseStartCurrent") { (terminalId: String) throws -> [String: Any] in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard let json = MeetermCore.runtimeBrowseStartCurrent(terminalId: handle),
            let data = json.data(using: .utf8),
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw Self.error("The runtime browse could not be started.")
      }
      return try Self.runtimeBrowseRecord(value)
    }
    AsyncFunction("runtimeBrowseStartProfile") { (terminalId: String, profileId: String) throws -> [String: Any] in
      let profile = try ClientStore.connectionOptions(profileId)
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      let options = try Self.decodeOptions(profile)
      guard let json = MeetermCore.runtimeBrowseStartProfile(
        terminalId: handle, host: options.host, port: options.port,
        username: options.username, privateKey: options.privateKey, passphrase: options.passphrase,
        knownHostsPath: try KnownHostsStore.path(), authMethod: options.authMethod, password: options.password
      ), let data = json.data(using: .utf8),
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw Self.error("The runtime browse could not be started.")
      }
      return try Self.runtimeBrowseRecord(value)
    }
    AsyncFunction("runtimeBrowseStartCredential") { (terminalId: String, values: [String: Any]) throws -> [String: Any] in
      let options = try Self.decodeOptions(values)
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard let json = MeetermCore.runtimeBrowseStartCredential(
        terminalId: handle, host: options.host, port: options.port,
        username: options.username, privateKey: options.privateKey, passphrase: options.passphrase,
        knownHostsPath: try KnownHostsStore.path(), authMethod: options.authMethod, password: options.password
      ), let data = json.data(using: .utf8),
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw Self.error("The runtime browse could not be started.")
      }
      return try Self.runtimeBrowseRecord(value)
    }
    // `unchanged` is a confirmed no-op outcome; preserve the existing Ready owner.
    AsyncFunction("runtimeBrowseState") { (token: String) throws -> [String: Any] in
      guard Self.validBrowseToken(token), let json = MeetermCore.runtimeBrowseState(token: token),
            let data = json.data(using: .utf8),
            let value = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw Self.error("The runtime browse state is unavailable.")
      }
      return try Self.runtimeBrowseRecord(value)
    }
    AsyncFunction("runtimeBrowseRefresh") { (token: String) throws in
      guard Self.validBrowseToken(token), MeetermCore.runtimeBrowseRefresh(token: token) == 0 else {
        throw Self.error("Runtime browse could not be refreshed.")
      }
    }
    AsyncFunction("runtimeBrowseCancel") { (token: String) throws in
      guard Self.validBrowseToken(token), MeetermCore.runtimeBrowseCancel(token: token) == 0 else {
        throw Self.error("Runtime browse could not be cancelled.")
      }
    }
    AsyncFunction("runtimeBrowseRespondToHostKey") { (token: String, fingerprint: String, accept: Bool) throws in
      guard Self.validBrowseToken(token), !fingerprint.isEmpty,
            fingerprint.utf8.count <= 128, !Self.containsControl(fingerprint),
            MeetermCore.runtimeBrowseRespondToHostKey(token: token, fingerprint: fingerprint, accept: accept) == 0 else {
        throw Self.error("The runtime browse host key response could not be sent.")
      }
    }
    AsyncFunction("runtimeBrowseCommit") {
      (token: String, browseGeneration: String, discoveryRevision: Int, target: [String: Any]) throws in
      guard Self.validBrowseToken(token), let generation = UInt64(browseGeneration),
            discoveryRevision >= 0 else {
        throw Self.error("The runtime browse identity is invalid.")
      }
      let kind = target["kind"] as? String
      let candidate: String
      let createName: String
      switch kind {
      case "candidate":
        candidate = target["candidateId"] as? String ?? ""
        createName = ""
        guard !candidate.isEmpty, candidate.utf8.count <= 256, !Self.containsControl(candidate) else {
          throw Self.error("The runtime candidate is invalid.")
        }
      case "createTmux":
        candidate = ""
        createName = target["name"] as? String ?? ""
        guard !createName.isEmpty, createName.utf8.count <= 64, !Self.containsControl(createName) else {
          throw Self.error("The tmux session name is invalid.")
        }
      default:
        throw Self.error("The runtime browse target is invalid.")
      }
      guard MeetermCore.runtimeBrowseCommit(
        token: token, browseGeneration: generation, discoveryRevision: UInt64(discoveryRevision),
        candidateId: candidate, createName: createName
      ) == 0 else {
        throw Self.error("The runtime browse commit could not be started.")
      }
    }

    AsyncFunction("reconnect") { (terminalId: String) throws in
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.reconnect(terminalId: handle) == 0 else {
        throw Self.error("The reconnect request could not be started.")
      }
    }

    AsyncFunction("retryRecovery") { (terminalId: String, operationEpoch: String) throws in
      let epoch = try Self.parseOperationEpoch(operationEpoch)
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.retryRecovery(terminalId: handle, expectedEpoch: epoch) == 0 else {
        throw Self.error("The recovery retry could not be started.")
      }
    }

    AsyncFunction("confirmRecovery") { (terminalId: String, confirmationToken: String) throws in
      guard Self.validRecoveryToken(confirmationToken) else {
        throw Self.error("The recovery confirmation is invalid or unavailable.")
      }
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.confirmRecovery(terminalId: handle, token: confirmationToken) == 0 else {
        throw Self.error("The recovery confirmation is invalid or unavailable.")
      }
    }

    AsyncFunction("changeRuntime") { (terminalId: String, operationEpoch: String) throws in
      let epoch = try Self.parseOperationEpoch(operationEpoch)
      let handle = try Self.ensureHandle(Self.normalizeTerminalId(terminalId))
      guard MeetermCore.changeRuntime(terminalId: handle, expectedEpoch: epoch) == 0 else {
        throw Self.error("The runtime could not be changed.")
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
      Prop("interactionMode", "live") { (view: MeetermTerminalView, value: String) in view.setInteractionMode(value) }
      Prop("terminalId", "poc-main") { (view: MeetermTerminalView, terminalId: String) in
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")
    }
  }

  private static func connectHostOptions(_ terminalId: String, options: [String: Any]) throws {
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
    let result = MeetermCore.connectHost(terminalId: handle, host: connection.host, port: connection.port,
      username: connection.username, privateKey: connection.privateKey, passphrase: connection.passphrase,
      knownHostsPath: knownHostsPath, authMethod: connection.authMethod, password: connection.password)
    guard result == 0 else { throw Self.error("The SSH host connection could not be started.") }
  }

  /// Keep the JavaScript contract limited to the fixed runtime summary. The
  /// native adapter rejects malformed/oversized fields instead of forwarding
  /// arbitrary CLI output, paths, sockets, or stderr.
  private static func runtimeDiscoveryRecord(_ value: [String: Any]) throws -> [String: Any] {
    guard let connectionGeneration = value["connectionGeneration"] as? String,
          !connectionGeneration.isEmpty, connectionGeneration.utf8.count <= 20,
          connectionGeneration.allSatisfy({ $0.isASCII && $0.isNumber }),
          UInt64(connectionGeneration) != nil,
          let revision = integer(value["revision"]), revision >= 0,
          let rawBackends = value["backends"] as? [[String: Any]], rawBackends.count <= 2 else {
      throw error("The native runtime discovery is invalid.")
    }
    var backends: [[String: Any]] = []
    for raw in rawBackends {
      guard let backend = raw["backend"] as? String,
            backend == "tmux" || backend == "herdr",
            let state = raw["state"] as? String,
            ["loading", "ready", "error"].contains(state),
            let canCreate = raw["canCreate"] as? Bool,
            canCreate == (backend == "tmux"),
            let rawCandidates = raw["candidates"] as? [[String: Any]], rawCandidates.count <= 256 else {
        throw error("The native runtime discovery is invalid.")
      }
      var candidates: [[String: Any]] = []
      for candidate in rawCandidates {
        guard let id = candidate["id"] as? String,
              let name = candidate["name"] as? String,
              let candidateBackend = candidate["backend"] as? String,
              candidateBackend == backend,
              let candidateState = candidate["state"] as? String,
              candidateState == "running" || candidateState == "stopped",
              let selectable = candidate["selectable"] as? Bool,
              let isDefault = candidate["isDefault"] as? Bool,
              let lastUsed = candidate["lastUsed"] as? Bool,
              let rawErrorCode = candidate["errorCode"] as? String,
              let rawErrorMessage = candidate["errorMessage"] as? String,
              !id.isEmpty, id.utf8.count <= 256,
              !name.isEmpty, name.utf8.count <= 256,
              rawErrorCode.utf8.count <= 64,
              rawErrorMessage.utf8.count <= 256 else {
          throw error("The native runtime discovery is invalid.")
        }
        candidates.append([
          "id": sanitize(id, maxLength: 256),
          "backend": backend,
          "name": sanitize(name, maxLength: 256),
          "state": candidateState,
          "selectable": selectable,
          "isDefault": isDefault,
          "lastUsed": lastUsed,
          "errorCode": sanitizeErrorCode(rawErrorCode),
          "errorMessage": sanitize(rawErrorMessage, maxLength: 256)
        ])
      }
      backends.append([
        "backend": backend,
        "state": state,
        "errorCode": sanitizeErrorCode(raw["errorCode"] as? String ?? ""),
        "errorMessage": sanitize(raw["errorMessage"] as? String ?? "", maxLength: 256),
        "candidates": candidates,
        "canCreate": canCreate
      ])
    }
    return ["connectionGeneration": connectionGeneration, "revision": revision, "backends": backends]
  }

  private static func runtimeBrowseRecord(_ value: [String: Any]) throws -> [String: Any] {
    guard let token = value["token"] as? String, validBrowseToken(token),
          let generation = value["browseGeneration"] as? String, UInt64(generation) != nil,
          let phase = value["phase"] as? String,
          ["starting", "discovering", "ready", "committing", "committed", "unchanged", "failed", "cancelled"].contains(phase),
          let revision = integer(value["discoveryRevision"]), revision >= 0,
          let rawDiscovery = value["discovery"] as? [String: Any] else {
      throw error("The native runtime browse is invalid.")
    }
    let discovery = try runtimeDiscoveryRecord(rawDiscovery)
    guard let rawHostKey = value["hostKey"] as? [String: Any],
          let hostKeyPending = rawHostKey["pending"] as? Bool,
          let hostKeyHost = rawHostKey["host"] as? String,
          let hostKeyPort = integer(rawHostKey["port"]),
          let hostKeyFingerprint = rawHostKey["fingerprint"] as? String,
          let hostKeyAlgorithm = rawHostKey["algorithm"] as? String,
          let hostKeyKnownFingerprint = rawHostKey["knownFingerprint"] as? String,
          hostKeyPort >= 0, hostKeyPort <= 65535,
          hostKeyHost.utf8.count <= 256, hostKeyFingerprint.utf8.count <= 128,
          hostKeyAlgorithm.utf8.count <= 64, hostKeyKnownFingerprint.utf8.count <= 128,
          !containsControl(hostKeyHost), !containsControl(hostKeyFingerprint),
          !containsControl(hostKeyAlgorithm), !containsControl(hostKeyKnownFingerprint) else {
      throw error("The native runtime browse is invalid.")
    }
    let active = value["activeTerminalId"] as? String
    if let active { guard UInt64(active) != nil else { throw error("The native runtime browse is invalid.") } }
    let warning: [String: Any]?
    if let raw = value["cleanupWarning"] as? [String: Any] {
      guard let id = raw["id"] as? String, UInt64(id) != nil,
            raw["code"] as? String == "layout_restore_unconfirmed",
            let message = raw["message"] as? String else {
        throw error("The native runtime browse is invalid.")
      }
      warning = ["id": id, "code": "layout_restore_unconfirmed", "message": sanitize(message, maxLength: 256)]
    } else {
      warning = nil
    }
    return [
      "token": token,
      "browseGeneration": generation,
      "discoveryRevision": revision,
      "phase": phase,
      "discovery": discovery,
      "hostKey": [
        "pending": hostKeyPending,
        "host": sanitize(hostKeyHost, maxLength: 256),
        "port": hostKeyPort,
        "fingerprint": sanitize(hostKeyFingerprint, maxLength: 128),
        "algorithm": sanitize(hostKeyAlgorithm, maxLength: 64),
        "knownFingerprint": sanitize(hostKeyKnownFingerprint, maxLength: 128)
      ],
      "errorCode": sanitizeErrorCode(value["errorCode"] as? String ?? ""),
      "errorMessage": sanitize(value["errorMessage"] as? String ?? "", maxLength: 256),
      "cleanupWarning": warning ?? NSNull(),
      "activeTerminalId": active ?? NSNull()
    ]
  }

  private static func validBrowseToken(_ value: String) -> Bool {
    !value.isEmpty && value.utf8.count <= 20 && value.allSatisfy { $0.isASCII && $0.isNumber } && UInt64(value).map { $0 != 0 } == true
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

  /// Parse the JS decimal string without converting through NSNumber/Double.
  /// UInt64(String) is lossless after the explicit ASCII-decimal check.
  private static func parseOperationEpoch(_ value: String) throws -> UInt64 {
    guard let epoch = RecoveryBridgeValidation.parseOperationEpoch(value) else {
      throw error("The operation epoch is invalid.")
    }
    return epoch
  }

  private static func validRecoveryToken(_ value: String) -> Bool {
    RecoveryBridgeValidation.validRecoveryToken(value)
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

  private static func containsControl(_ value: String) -> Bool {
    value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
  }

  private static let startupPhases: Set<String> = [
    "js_module_loaded",
    "root_effect",
    "initial_url_requested",
    "initial_url_null",
    "initial_url_allowed_fixture",
    "initial_url_other",
    "initial_url_rejected",
    "app_content_mounted",
    "profiles_requested",
    "profiles_succeeded",
    "profiles_failed",
  ]

  private static func recordStartupPhase(_ phase: String) {
    guard ProcessInfo.processInfo.arguments.contains("-meeterm-ui-observation"),
          startupPhases.contains(phase) else {
      return
    }
    NSLog("MEETERM_SMOKE_STARTUP phase=%@", phase)
  }

  private static func error(_ message: String) -> NSError {
    NSError(
      domain: "dev.meeterm.terminal",
      code: 1,
      userInfo: [NSLocalizedDescriptionKey: message]
    )
  }
}
