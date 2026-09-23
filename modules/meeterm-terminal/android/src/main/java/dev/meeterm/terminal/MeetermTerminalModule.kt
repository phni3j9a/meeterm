package dev.meeterm.terminal

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import org.json.JSONArray
import org.json.JSONObject

class MeetermTerminalModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("MeetermTerminal")

    AsyncFunction("getProfiles") { ClientStore.profiles(storageContext()) }
    AsyncFunction("saveProfile") { profile: Map<String, Any?>, credential: Map<String, Any?>?, keepCredential: Boolean ->
      ClientStore.saveProfile(storageContext(), profile, credential, keepCredential)
    }
    AsyncFunction("deleteProfile") { profileId: String -> ClientStore.deleteProfile(storageContext(), profileId) }
    // Every fresh connection path authenticates the SSH host first. Persisted
    // backend/runtime values are only last-used hints and never select a
    // runtime on behalf of the caller.
    AsyncFunction("connectHost") { terminalId: String, options: Map<String, Any?> ->
      connectHostOptions(terminalId, options)
    }
    AsyncFunction("connectProfileHost") { terminalId: String, profileId: String ->
      val options = ClientStore.connectionOptions(storageContext(), profileId).toMutableMap().apply {
        remove("backend")
        remove("runtime")
      }
      connectHostOptions(terminalId, options)
    }
    AsyncFunction("connectProfile") { terminalId: String, profileId: String ->
      connectHostOptions(terminalId, ClientStore.connectionOptions(storageContext(), profileId))
    }
    AsyncFunction("setLastUsedRuntime") { profileId: String, backend: String, runtime: String ->
      ClientStore.setLastUsedRuntime(storageContext(), profileId, backend, runtime)
    }
    AsyncFunction("getPreferences") {
      ClientStore.preferences(storageContext()).also {
        check(MeetermNative.setScrollbackLimit((it["scrollbackLines"] as Number).toInt()) == 0) { "The history preference could not be applied." }
      }
    }
    AsyncFunction("setPreferences") { preferences: Map<String, Any?> ->
      val validated = ClientStore.validatePreferences(preferences)
      ClientStore.setPreferences(storageContext(), validated)
      check(MeetermNative.setScrollbackLimit((validated["scrollbackLines"] as Number).toInt()) == 0) { "The history preference could not be applied." }
    }

    AsyncFunction("setForeground") { terminalId: String, foreground: Boolean ->
      check(MeetermNative.setForeground(ensureHandle(normalizeTerminalId(terminalId)), foreground) == 0) { "The app lifecycle could not be updated." }
    }
    AsyncFunction("setTerminalVisible") { terminalId: String, visible: Boolean ->
      check(MeetermNative.setTerminalVisible(ensureHandle(normalizeTerminalId(terminalId)), visible) == 0) { "The terminal visibility could not be updated." }
    }
    AsyncFunction("setAutomaticReconnect") { terminalId: String, enabled: Boolean ->
      check(MeetermNative.setAutomaticReconnect(ensureHandle(normalizeTerminalId(terminalId)), enabled) == 0) { "The reconnect preference could not be updated." }
    }
    AsyncFunction("createWorkspace") { terminalId: String, name: String -> tmuxCommand(terminalId, 0, 0, name) }
    AsyncFunction("renameWorkspace") { terminalId: String, windowId: String, name: String -> tmuxCommand(terminalId, 1, targetId(windowId, '@'), name) }
    AsyncFunction("closeWorkspace") { terminalId: String, windowId: String -> tmuxCommand(terminalId, 2, targetId(windowId, '@')) }
    AsyncFunction("createPane") { terminalId: String, windowId: String -> tmuxCommand(terminalId, 3, targetId(windowId, '@')) }
    AsyncFunction("renamePane") { terminalId: String, paneId: String, name: String -> tmuxCommand(terminalId, 4, targetId(paneId, '%'), name) }
    AsyncFunction("closePane") { terminalId: String, paneId: String -> tmuxCommand(terminalId, 5, targetId(paneId, '%')) }
    AsyncFunction("refreshTerminal") { terminalId: String -> tmuxCommand(terminalId, 6, 0) }
    AsyncFunction("createGroup") { terminalId: String, workspaceId: String, name: String ->
      tmuxCommand(terminalId, 7, numericId(workspaceId), name)
    }
    AsyncFunction("renameGroup") { terminalId: String, groupId: String, name: String ->
      tmuxCommand(terminalId, 8, numericId(groupId), name)
    }
    AsyncFunction("closeGroup") { terminalId: String, groupId: String ->
      tmuxCommand(terminalId, 9, numericId(groupId))
    }
    AsyncFunction("selectGroup") { terminalId: String, groupId: String ->
      tmuxCommand(terminalId, 10, numericId(groupId))
    }
    AsyncFunction("getWorkspaceState") { terminalId: String ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      val json = MeetermNative.workspaceState(handle)
        ?: throw IllegalStateException("Native workspace state is unavailable.")
      jsonObjectValue(JSONObject(json))
    }

    AsyncFunction("connect") { terminalId: String, options: Map<String, Any?> ->
      connectHostOptions(terminalId, options)
    }

    AsyncFunction("disconnect") { terminalId: String ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      if (MeetermNative.sshDisconnect(handle) != 0) {
        throw IllegalStateException("The SSH disconnect request could not be sent.")
      }
    }

    AsyncFunction("getConnectionState") { terminalId: String ->
      val normalizedId = normalizeTerminalId(terminalId)
      val handle = ensureHandle(normalizedId)
      connectionState(handle)
    }

    AsyncFunction("getRuntimeDiscovery") { terminalId: String ->
      runtimeDiscovery(ensureHandle(normalizeTerminalId(terminalId)))
    }

    AsyncFunction("refreshRuntimes") { terminalId: String ->
      check(MeetermNative.refreshRuntimes(ensureHandle(normalizeTerminalId(terminalId))) == 0) {
        "Runtime discovery could not be refreshed."
      }
    }

    AsyncFunction("selectRuntime") { terminalId: String, candidateId: String ->
      require(candidateId.isNotEmpty() && candidateId.toByteArray(Charsets.UTF_8).size <= RUNTIME_ID_MAX_BYTES &&
        candidateId.none(Char::isISOControl)) { "The selected runtime is invalid." }
      check(MeetermNative.selectRuntime(ensureHandle(normalizeTerminalId(terminalId)), candidateId) == 0) {
        "The selected runtime could not be opened."
      }
    }

    AsyncFunction("createTmuxSession") { terminalId: String, name: String ->
      require(name.isNotEmpty() && name.toByteArray(Charsets.UTF_8).size <= TMUX_CREATE_NAME_MAX_BYTES &&
        name.none(Char::isISOControl)) { "The tmux session name is invalid." }
      check(MeetermNative.createTmuxSession(ensureHandle(normalizeTerminalId(terminalId)), name) == 0) {
        "The tmux session could not be created."
      }
    }

    AsyncFunction("runtimeBrowseStartCurrent") { terminalId: String ->
      runtimeBrowseStateValue(MeetermNative.runtimeBrowseStartCurrent(ensureHandle(normalizeTerminalId(terminalId))))
    }
    AsyncFunction("runtimeBrowseStartProfile") { terminalId: String, profileId: String ->
      val options = ClientStore.connectionOptions(storageContext(), profileId).toMutableMap().apply {
        remove("backend")
        remove("runtime")
      }
      val nativeOptions = SshOptions.from(options)
      val raw = MeetermNative.runtimeBrowseStartProfile(
        ensureHandle(normalizeTerminalId(terminalId)), nativeOptions.host, nativeOptions.port,
        nativeOptions.username, nativeOptions.privateKey, nativeOptions.passphrase,
        KnownHostsStore.path(storageContext()), nativeOptions.authMethod, nativeOptions.password,
      )
      runtimeBrowseStateValue(raw)
    }
    AsyncFunction("runtimeBrowseStartCredential") { terminalId: String, options: Map<String, Any?> ->
      val nativeOptions = SshOptions.from(options)
      runtimeBrowseStateValue(MeetermNative.runtimeBrowseStartCredential(
        ensureHandle(normalizeTerminalId(terminalId)), nativeOptions.host, nativeOptions.port,
        nativeOptions.username, nativeOptions.privateKey, nativeOptions.passphrase,
        KnownHostsStore.path(storageContext()), nativeOptions.authMethod, nativeOptions.password,
      ))
    }
    AsyncFunction("runtimeBrowseState") { token: String ->
      runtimeBrowseStateValue(MeetermNative.runtimeBrowseState(token))
    }
    AsyncFunction("runtimeBrowseRefresh") { token: String ->
      requireValidBrowseToken(token)
      check(MeetermNative.runtimeBrowseRefresh(token) == 0) { "Runtime browse could not be refreshed." }
    }
    AsyncFunction("runtimeBrowseCancel") { token: String ->
      requireValidBrowseToken(token)
      check(MeetermNative.runtimeBrowseCancel(token) == 0) { "Runtime browse could not be cancelled." }
    }
    AsyncFunction("runtimeBrowseRespondToHostKey") { token: String, fingerprint: String, accept: Boolean ->
      requireValidBrowseToken(token)
      require(fingerprint.isNotEmpty() && fingerprint.toByteArray(Charsets.UTF_8).size <= FINGERPRINT_MAX_BYTES &&
        fingerprint.none(Char::isISOControl)) {
        "The runtime browse host key is invalid."
      }
      check(MeetermNative.runtimeBrowseRespondToHostKey(token, fingerprint, accept) == 0) {
        "The runtime browse host key response could not be sent."
      }
    }
    AsyncFunction("runtimeBrowseCommit") {
        token: String,
        browseGeneration: String,
        discoveryRevision: Int,
        target: Map<String, Any?>,
      ->
      requireValidBrowseToken(token)
      requireDecimalBrowseValue(browseGeneration)
      require(discoveryRevision >= 0) { "The runtime browse revision is invalid." }
      val kind = target["kind"] as? String
      val candidate = if (kind == "candidate") {
        val id = target["candidateId"] as? String
        require(!id.isNullOrEmpty() && id.toByteArray(Charsets.UTF_8).size <= RUNTIME_ID_MAX_BYTES &&
          id.none(Char::isISOControl)) { "The runtime candidate is invalid." }
        id
      } else ""
      val createName = if (kind == "createTmux") {
        val name = target["name"] as? String
        require(!name.isNullOrEmpty() && name.toByteArray(Charsets.UTF_8).size <= TMUX_CREATE_NAME_MAX_BYTES &&
          name.none(Char::isISOControl)) { "The tmux session name is invalid." }
        name
      } else ""
      require(kind == "candidate" || kind == "createTmux") { "The runtime browse target is invalid." }
      check(MeetermNative.runtimeBrowseCommit(
        token, browseGeneration, discoveryRevision.toString(), candidate, createName,
      ) == 0) { "The runtime browse commit could not be started." }
    }

    AsyncFunction("reconnect") { terminalId: String ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      check(MeetermNative.sshReconnect(handle) == 0) { "The reconnect request could not be started." }
    }

    AsyncFunction("retryRecovery") { terminalId: String, operationEpoch: String ->
      val epoch = parseOperationEpoch(operationEpoch)
      check(MeetermNative.retryRecovery(ensureHandle(normalizeTerminalId(terminalId)), epoch) == 0) {
        "The recovery retry could not be started."
      }
    }

    AsyncFunction("confirmRecovery") { terminalId: String, confirmationToken: String ->
      require(validRecoveryToken(confirmationToken)) {
        "The recovery confirmation is invalid."
      }
      check(MeetermNative.confirmRecovery(
        ensureHandle(normalizeTerminalId(terminalId)),
        confirmationToken,
      ) == 0) {
        "The recovery confirmation is unavailable."
      }
    }

    AsyncFunction("changeRuntime") { terminalId: String, operationEpoch: String ->
      val epoch = parseOperationEpoch(operationEpoch)
      check(MeetermNative.changeRuntime(ensureHandle(normalizeTerminalId(terminalId)), epoch) == 0) {
        "The runtime could not be changed."
      }
    }

    AsyncFunction("selectPane") { terminalId: String, paneId: String ->
      val pane = targetId(paneId, '%')
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      check(MeetermNative.tmuxSelectPane(handle, pane) == 0) { "The terminal could not be selected." }
    }

    AsyncFunction("getSessionState") { terminalId: String ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      val fields = MeetermNative.tmuxSessionState(handle)
        ?: throw IllegalStateException("Native session state is unavailable.")
      check(fields.size % 7 == 0) { "Native session state is unavailable." }
      mapOf("panes" to fields.toList().chunked(7).map { pane ->
        mapOf(
          "windowId" to "@${pane[0]}",
          "paneId" to "%${pane[1]}",
          "terminalId" to "native:${pane[2]}",
          "windowName" to sanitize(pane[3], 256),
          "selected" to (pane[4] == "1"),
          "active" to (pane[5] == "1"),
          "paneName" to sanitize(pane[6], 256),
        )
      })
    }

    AsyncFunction("respondToHostKey") {
        terminalId: String,
        fingerprint: String,
        accept: Boolean,
      ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      if (fingerprint.isEmpty() || fingerprint.any { it.isISOControl() }) {
        throw IllegalArgumentException("The host-key response is invalid.")
      }
      if (MeetermNative.sshRespondHostKey(handle, fingerprint, accept) != 0) {
        throw IllegalStateException("The host-key response could not be sent.")
      }
    }

    AsyncFunction("forgetHostKey") { host: String, port: Int ->
      val normalizedHost = host.trim()
      if (normalizedHost.isEmpty() || normalizedHost.any { it.isISOControl() }) {
        throw IllegalArgumentException("The host-key endpoint is invalid.")
      }
      requireValidPort(port)
      val context = requireNotNull(appContext.reactContext?.applicationContext) {
        "Native application context is unavailable."
      }
      if (MeetermNative.sshForgetHostKey(
          normalizedHost,
          port,
          KnownHostsStore.path(context),
        ) != 0
      ) {
        throw IllegalStateException("The trusted host key could not be removed.")
      }
    }

    View(MeetermTerminalView::class) {
      Prop("fontSize", 15.0) { view: MeetermTerminalView, value: Double -> view.setFontSize(value) }
      Prop("theme", "dark") { view: MeetermTerminalView, value: String -> view.setTheme(value) }
      Prop("scrollbackLines", 10000) { view: MeetermTerminalView, value: Int -> view.setScrollbackLines(value) }
      Prop("interactionMode", "live") { view: MeetermTerminalView, value: String -> view.setInteractionMode(value) }
      Prop("terminalId", "poc-main") { view: MeetermTerminalView, terminalId: String ->
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")

      OnViewDestroys { view: MeetermTerminalView ->
        view.releaseBindingForLifecycle()
      }
    }
  }

  private fun storageContext() = requireNotNull(appContext.reactContext?.applicationContext) {
    "Native application context is unavailable."
  }

  private fun targetId(value: String, prefix: Char): Long {
    val normalized = value.trim()
    val digits = if (normalized.firstOrNull() == prefix) normalized.drop(1) else normalized
    require(digits.isNotEmpty() && digits.all { it in '0'..'9' }) { "The tmux target is invalid." }
    return digits.toLongOrNull() ?: throw IllegalArgumentException("The tmux target is invalid.")
  }

  private fun parseOperationEpoch(value: String): String {
    return RecoveryBridgeValidation.parseOperationEpoch(value)
  }

  private fun validRecoveryToken(value: String): Boolean =
    RecoveryBridgeValidation.validRecoveryToken(value)

  private fun numericId(value: String): Long {
    val normalized = value.trim()
    require(normalized.isNotEmpty() && normalized.all { it in '0'..'9' }) { "The workspace group ID is invalid." }
    return normalized.toLongOrNull() ?: throw IllegalArgumentException("The workspace group ID is invalid.")
  }

  private fun tmuxCommand(terminalId: String, operation: Int, target: Long, name: String = "") {
    check(MeetermNative.tmuxCommand(ensureHandle(normalizeTerminalId(terminalId)), operation, target, name) == 0) {
      "The workspace operation could not be started. Check the connection and try again."
    }
  }

  private fun connectHostOptions(terminalId: String, options: Map<String, Any?>) {
    val nativeOptions = SshOptions.from(options)
    val handle = ensureHandle(normalizeTerminalId(terminalId))
    val preferences = ClientStore.preferences(storageContext())
    check(MeetermNative.setScrollbackLimit((preferences["scrollbackLines"] as Number).toInt()) == 0)
    check(MeetermNative.setAutomaticReconnect(handle, preferences["automaticReconnect"] as Boolean) == 0)
    check(MeetermNative.sshConnectHost(handle, nativeOptions.host, nativeOptions.port,
      nativeOptions.username, nativeOptions.privateKey, nativeOptions.passphrase,
      KnownHostsStore.path(storageContext()), nativeOptions.authMethod, nativeOptions.password) == 0) {
      "The SSH host connection could not be started."
    }
  }

  /**
   * Keep the JS boundary to a fixed, bounded runtime summary. Arbitrary CLI
   * output, executable paths, socket paths, and stderr are never forwarded.
   */
  private fun runtimeDiscovery(handle: Long): Map<String, Any?> {
    val raw = MeetermNative.runtimeDiscovery(handle)
      ?: throw IllegalStateException("Native runtime discovery is unavailable.")
    return runtimeDiscoveryValue(raw)
  }

  private fun runtimeDiscoveryValue(raw: String): Map<String, Any?> {
    require(raw.toByteArray(Charsets.UTF_8).size <= RUNTIME_DISCOVERY_MAX_BYTES) {
      "The native runtime discovery is invalid."
    }
    val root = JSONObject(raw)
    val connectionGeneration = root.getString("connectionGeneration")
    require(connectionGeneration.isNotEmpty() && connectionGeneration.length <= 20 &&
      connectionGeneration.all { it in '0'..'9' } && connectionGeneration.toULongOrNull() != null) {
      "The native runtime discovery is invalid."
    }
    val revision = root.getLong("revision")
    require(revision in 0L..Int.MAX_VALUE.toLong()) { "The native runtime discovery is invalid." }
    val rawBackends = root.getJSONArray("backends")
    require(rawBackends.length() <= 2) { "The native runtime discovery is invalid." }
    val backends = (0 until rawBackends.length()).map { backendIndex ->
      val source = rawBackends.getJSONObject(backendIndex)
      val backend = source.getString("backend")
      require(backend == TMUX_BACKEND || backend == HERDR_BACKEND) { "The native runtime discovery is invalid." }
      val state = source.getString("state")
      require(state in listOf("loading", "ready", "error")) { "The native runtime discovery is invalid." }
      val canCreate = source.getBoolean("canCreate")
      require(canCreate == (backend == TMUX_BACKEND)) { "The native runtime discovery is invalid." }
      val sourceCandidates = source.getJSONArray("candidates")
      require(sourceCandidates.length() <= RUNTIME_CANDIDATE_LIMIT) { "The native runtime discovery is invalid." }
      val candidates = (0 until sourceCandidates.length()).map { candidateIndex ->
        val candidate = sourceCandidates.getJSONObject(candidateIndex)
        val id = candidate.getString("id")
        val name = candidate.getString("name")
        val candidateBackend = candidate.getString("backend")
        val candidateState = candidate.getString("state")
        val selectableValue = candidate.opt("selectable")
        val errorCodeValue = candidate.opt("errorCode")
        val errorMessageValue = candidate.opt("errorMessage")
        require(candidateBackend == backend && candidateState in listOf("running", "stopped") &&
          id.isNotEmpty() && name.isNotEmpty() && id.toByteArray(Charsets.UTF_8).size <= RUNTIME_ID_MAX_BYTES &&
          name.toByteArray(Charsets.UTF_8).size <= RUNTIME_NAME_MAX_BYTES &&
          selectableValue is Boolean && errorCodeValue is String && errorMessageValue is String &&
          errorCodeValue.toByteArray(Charsets.UTF_8).size <= RUNTIME_ERROR_CODE_MAX_BYTES &&
          errorMessageValue.toByteArray(Charsets.UTF_8).size <= RUNTIME_ERROR_MAX_BYTES) {
          "The native runtime discovery is invalid."
        }
        mapOf(
          "id" to sanitize(id, RUNTIME_ID_MAX_BYTES),
          "backend" to backend,
          "name" to sanitize(name, RUNTIME_NAME_MAX_BYTES),
          "state" to candidateState,
          "selectable" to selectableValue,
          "isDefault" to candidate.getBoolean("isDefault"),
          "lastUsed" to candidate.getBoolean("lastUsed"),
          "errorCode" to sanitizeErrorCode(errorCodeValue as String),
          "errorMessage" to sanitize(errorMessageValue as String, RUNTIME_ERROR_MAX_BYTES),
        )
      }
      mapOf(
        "backend" to backend,
        "state" to state,
        "errorCode" to sanitizeErrorCode(source.optString("errorCode", "")),
        "errorMessage" to sanitize(source.optString("errorMessage", ""), RUNTIME_ERROR_MAX_BYTES),
        "candidates" to candidates,
        "canCreate" to canCreate,
      )
    }
    return mapOf(
      "connectionGeneration" to connectionGeneration,
      "revision" to revision.toInt(),
      "backends" to backends,
    )
  }

  private fun runtimeBrowseStateValue(raw: String?): Map<String, Any?> {
    require(!raw.isNullOrEmpty() && raw.toByteArray(Charsets.UTF_8).size <= RUNTIME_BROWSE_MAX_BYTES) {
      "The native runtime browse is unavailable."
    }
    val root = JSONObject(raw)
    val token = root.getString("token")
    val browseGeneration = root.getString("browseGeneration")
    val phase = root.getString("phase")
    requireValidBrowseToken(token)
    requireDecimalBrowseValue(browseGeneration)
    require(phase in RUNTIME_BROWSE_PHASES) { "The native runtime browse is invalid." }
    val revision = root.getLong("discoveryRevision")
    require(revision in 0L..Int.MAX_VALUE.toLong()) { "The native runtime browse is invalid." }
    val discovery = root.getJSONObject("discovery")
    val normalizedDiscovery = runtimeDiscoveryValue(discovery.toString())
    val hostKey = root.getJSONObject("hostKey")
    val hostKeyPending = hostKey.getBoolean("pending")
    val hostKeyHost = hostKey.getString("host")
    val hostKeyPort = hostKey.getInt("port")
    val hostKeyFingerprint = hostKey.getString("fingerprint")
    val hostKeyAlgorithm = hostKey.getString("algorithm")
    val hostKeyKnownFingerprint = hostKey.getString("knownFingerprint")
    require(hostKeyPort in 0..65535 &&
      hostKeyHost.toByteArray(Charsets.UTF_8).size <= HOST_MAX_BYTES &&
      hostKeyFingerprint.toByteArray(Charsets.UTF_8).size <= FINGERPRINT_MAX_BYTES &&
      hostKeyAlgorithm.toByteArray(Charsets.UTF_8).size <= ALGORITHM_MAX_BYTES &&
      hostKeyKnownFingerprint.toByteArray(Charsets.UTF_8).size <= FINGERPRINT_MAX_BYTES &&
      listOf(hostKeyHost, hostKeyFingerprint, hostKeyAlgorithm, hostKeyKnownFingerprint)
        .none { it.any(Char::isISOControl) }) {
      "The native runtime browse is invalid."
    }
    val active = if (root.isNull("activeTerminalId")) null else root.getString("activeTerminalId")
    if (active != null) requireDecimalBrowseValue(active)
    val warning = if (root.isNull("cleanupWarning")) null else {
      val value = root.getJSONObject("cleanupWarning")
      val id = value.getString("id")
      requireDecimalBrowseValue(id)
      require(value.getString("code") == "layout_restore_unconfirmed") {
        "The native runtime browse is invalid."
      }
      mapOf(
        "id" to id,
        "code" to "layout_restore_unconfirmed",
        "message" to sanitize(value.getString("message"), RUNTIME_ERROR_MAX_BYTES),
      )
    }
    return mapOf(
      "token" to token,
      "browseGeneration" to browseGeneration,
      "discoveryRevision" to revision.toInt(),
      "phase" to phase,
      "discovery" to normalizedDiscovery,
      "hostKey" to mapOf(
        "pending" to hostKeyPending,
        "host" to sanitize(hostKeyHost, HOST_MAX_BYTES),
        "port" to hostKeyPort,
        "fingerprint" to sanitize(hostKeyFingerprint, FINGERPRINT_MAX_BYTES),
        "algorithm" to sanitize(hostKeyAlgorithm, ALGORITHM_MAX_BYTES),
        "knownFingerprint" to sanitize(hostKeyKnownFingerprint, FINGERPRINT_MAX_BYTES),
      ),
      "errorCode" to sanitizeErrorCode(root.optString("errorCode", "")),
      "errorMessage" to sanitize(root.optString("errorMessage", ""), RUNTIME_ERROR_MAX_BYTES),
      "cleanupWarning" to warning,
      "activeTerminalId" to active,
    )
  }

  private fun ensureHandle(terminalId: String): Long {
    val existing = TerminalRegistry.handleFor(terminalId)
    if (existing != 0L) return existing
    return TerminalRegistry.ensure(terminalId, DEFAULT_COLUMNS, DEFAULT_ROWS).also {
      if (it == 0L) {
        throw IllegalStateException("The native terminal could not be created.")
      }
    }
  }

  private fun connectionState(handle: Long): Map<String, Any?> {
    val fields = MeetermNative.sshConnectionState(handle)
      ?: throw IllegalStateException("Native connection state is unavailable.")
    if (fields.size != STATE_FIELD_COUNT) {
      throw IllegalStateException("Native connection state is unavailable.")
    }

    val stateCode = fields[0].toIntOrNull()
      ?.takeIf { it in STATE_DISCONNECTED..STATE_MAX }
      ?: throw IllegalStateException("Native connection state is unavailable.")
    val state = stateName(stateCode)
    val port = fields[2].toIntOrNull()
      ?.takeIf { it in 0..65535 }
      ?: throw IllegalStateException("Native connection state is unavailable.")
    return mapOf(
      "state" to state,
      "host" to sanitize(fields[1], 256),
      "port" to port,
      "fingerprint" to sanitize(fields[3], 128),
      "algorithm" to sanitize(fields[4], 64),
      "knownFingerprint" to sanitize(fields[5], 128),
      "errorCode" to sanitizeErrorCode(fields[6]),
      "errorMessage" to sanitize(fields[7], 256),
    )
  }

  internal data class SshOptions(
    val host: String,
    val port: Int,
    val username: String,
    val authMethod: String,
    val privateKey: String,
    val passphrase: String,
    val password: String,
  ) {
    companion object {
      fun from(values: Map<String, Any?>): SshOptions {
        val host = (values["host"] as? String)?.trim()
        val port = numberAsInt(values["port"])
        val username = (values["username"] as? String)?.trim()
        if (host.isNullOrEmpty() || host.any { it.isISOControl() } ||
          port == null || port !in 1..65535 ||
          username.isNullOrEmpty() || username.any { it.isISOControl() }
        ) {
          throw IllegalArgumentException("The SSH connection options are invalid.")
        }

        val authMethod = when {
          !values.containsKey("authMethod") -> PUBLIC_KEY_AUTH_METHOD
          values["authMethod"] == PUBLIC_KEY_AUTH_METHOD -> PUBLIC_KEY_AUTH_METHOD
          values["authMethod"] == PASSWORD_AUTH_METHOD -> PASSWORD_AUTH_METHOD
          else -> throw IllegalArgumentException("The SSH connection options are invalid.")
        }

        return when (authMethod) {
          PUBLIC_KEY_AUTH_METHOD -> {
            val privateKey = values["privateKey"] as? String
            val passphrase = values["passphrase"] as? String
            if (privateKey.isNullOrEmpty() || privateKey.any { it == '\u0000' } ||
              passphrase == null || passphrase.any { it == '\u0000' }
            ) {
              throw IllegalArgumentException("The SSH connection options are invalid.")
            }
            SshOptions(host, port, username, authMethod, privateKey, passphrase, "")
          }
          PASSWORD_AUTH_METHOD -> {
            val password = values["password"] as? String
            if (password.isNullOrEmpty() || password.any { it == '\u0000' }) {
              throw IllegalArgumentException("The SSH connection options are invalid.")
            }
            SshOptions(host, port, username, authMethod, "", "", password)
          }
          else -> error("unreachable authentication method")
        }
      }

      private fun numberAsInt(value: Any?): Int? {
        val number = value as? Number ?: return null
        val double = number.toDouble()
        if (!double.isFinite() || double != double.toInt().toDouble()) return null
        return double.toInt()
      }

    }
  }

  private companion object {
    const val PUBLIC_KEY_AUTH_METHOD = "publicKey"
    const val PASSWORD_AUTH_METHOD = "password"
    const val TMUX_BACKEND = "tmux"
    const val HERDR_BACKEND = "herdr"
    const val DEFAULT_COLUMNS = 80
    const val DEFAULT_ROWS = 24
    const val STATE_FIELD_COUNT = 8
    const val STATE_DISCONNECTED = 0
    const val STATE_RECONNECTING = 10
    const val STATE_MAX = 14
    const val RUNTIME_CANDIDATE_LIMIT = 256
    const val RUNTIME_DISCOVERY_MAX_BYTES = 1024 * 1024
    const val RUNTIME_BROWSE_MAX_BYTES = 1024 * 1024
    const val RUNTIME_ID_MAX_BYTES = 256
    const val RUNTIME_NAME_MAX_BYTES = 256
    const val HOST_MAX_BYTES = 256
    const val ALGORITHM_MAX_BYTES = 64
    const val FINGERPRINT_MAX_BYTES = 128
    const val RUNTIME_ERROR_CODE_MAX_BYTES = 64
    const val RUNTIME_ERROR_MAX_BYTES = 256
    const val TMUX_CREATE_NAME_MAX_BYTES = 64
    val RUNTIME_BROWSE_PHASES = setOf("starting", "discovering", "ready", "committing", "committed", "failed", "cancelled")

    fun normalizeTerminalId(value: String): String {
      val normalized = value.trim()
      if (normalized.isEmpty() || normalized.any { it.isISOControl() }) {
        throw IllegalArgumentException("The terminal ID is invalid.")
      }
      return normalized
    }

    fun requireValidPort(port: Int) {
      if (port !in 1..65535) {
        throw IllegalArgumentException("The SSH port is invalid.")
      }
    }

    fun requireValidBrowseToken(value: String) {
      require(value.isNotEmpty() && value.length <= 20 && value.all { it in '0'..'9' } &&
        value.toULongOrNull()?.let { it != 0UL } == true) {
        "The runtime browse token is invalid."
      }
    }

    fun requireDecimalBrowseValue(value: String) {
      require(value.isNotEmpty() && value.length <= 20 && value.all { it in '0'..'9' } && value.toULongOrNull() != null) {
        "The runtime browse identity is invalid."
      }
    }

    fun stateName(code: Int): String = when (code) {
      0 -> "Disconnected"
      1 -> "Connecting"
      2 -> "HostKeyPending"
      3 -> "Authenticating"
      4 -> "OpeningPty"
      5 -> "Ready"
      6 -> "Closing"
      8 -> "AttachingTmux"
      9 -> "Synchronizing"
      10 -> "Reconnecting"
      11 -> "DiscoveringRuntimes"
      12 -> "AwaitingRuntimeSelection"
      13 -> "AttachingRuntime"
      14 -> "CreatingRuntime"
      else -> "Failed"
    }

    fun sanitize(value: String?, maxLength: Int): String {
      return value.orEmpty()
        .filterNot(Char::isISOControl)
        .take(maxLength)
    }

    fun sanitizeErrorCode(value: String?): String {
      val code = value.orEmpty()
      if (code.isEmpty()) return ""
      return code.take(64).let {
        if (it.all { character ->
            character in 'a'..'z' || character in '0'..'9' || character == '_'
          }
        ) {
          it
        } else {
          "native_error"
        }
      }
    }

    private fun jsonObjectValue(value: JSONObject): Map<String, Any?> =
      value.keys().asSequence().associateWith { jsonValue(value.get(it)) }

    private fun jsonArrayValue(value: JSONArray): List<Any?> =
      (0 until value.length()).map { jsonValue(value.get(it)) }

    private fun jsonValue(value: Any?): Any? = when (value) {
      JSONObject.NULL -> null
      is JSONObject -> jsonObjectValue(value)
      is JSONArray -> jsonArrayValue(value)
      else -> value
    }
  }
}
