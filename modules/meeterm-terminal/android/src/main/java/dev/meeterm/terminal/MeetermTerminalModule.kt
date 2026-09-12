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
    AsyncFunction("connectProfile") { terminalId: String, profileId: String ->
      connectOptions(terminalId, ClientStore.connectionOptions(storageContext(), profileId))
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
      connectOptions(terminalId, options)
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

    AsyncFunction("reconnect") { terminalId: String ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      check(MeetermNative.sshReconnect(handle) == 0) { "The reconnect request could not be started." }
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

  private fun connectOptions(terminalId: String, options: Map<String, Any?>) {
    val nativeOptions = SshOptions.from(options)
    val handle = ensureHandle(normalizeTerminalId(terminalId))
    val preferences = ClientStore.preferences(storageContext())
    check(MeetermNative.setScrollbackLimit((preferences["scrollbackLines"] as Number).toInt()) == 0)
    check(MeetermNative.setAutomaticReconnect(handle, preferences["automaticReconnect"] as Boolean) == 0)
    check(MeetermNative.sshConnectBackend(handle, nativeOptions.host, nativeOptions.port,
      nativeOptions.username, nativeOptions.privateKey, nativeOptions.passphrase,
      KnownHostsStore.path(storageContext()), nativeOptions.authMethod, nativeOptions.password,
      nativeOptions.backend, nativeOptions.runtime) == 0) {
      "The SSH connection could not be started."
    }
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
      ?.takeIf { it in STATE_DISCONNECTED..STATE_RECONNECTING }
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
    val backend: String,
    val runtime: String,
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

        require(!values.containsKey("backend") || values["backend"] is String) { "The backend is invalid." }
        require(!values.containsKey("runtime") || values["runtime"] is String) { "The runtime is invalid." }
        val backend = values["backend"] as? String ?: MeetermTerminalModule.DEFAULT_BACKEND
        require(backend == MeetermTerminalModule.TMUX_BACKEND || backend == MeetermTerminalModule.HERDR_BACKEND) {
          "The SSH connection options are invalid."
        }
        val runtime = values["runtime"] as? String ?: ""
        requireValidRuntime(backend, runtime)

        return when (authMethod) {
          PUBLIC_KEY_AUTH_METHOD -> {
            val privateKey = values["privateKey"] as? String
            val passphrase = values["passphrase"] as? String
            if (privateKey.isNullOrEmpty() || privateKey.any { it == '\u0000' } ||
              passphrase == null || passphrase.any { it == '\u0000' }
            ) {
              throw IllegalArgumentException("The SSH connection options are invalid.")
            }
            SshOptions(host, port, username, authMethod, privateKey, passphrase, "", backend, runtime)
          }
          PASSWORD_AUTH_METHOD -> {
            val password = values["password"] as? String
            if (password.isNullOrEmpty() || password.any { it == '\u0000' }) {
              throw IllegalArgumentException("The SSH connection options are invalid.")
            }
            SshOptions(host, port, username, authMethod, "", "", password, backend, runtime)
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

      private fun requireValidRuntime(backend: String, runtime: String) {
        require(runtime.toByteArray(Charsets.UTF_8).size <= MeetermTerminalModule.HERDR_RUNTIME_MAX_BYTES &&
          runtime != "." && runtime != ".." &&
          runtime.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '.' || it == '_' || it == '-' }) {
          "The SSH connection options are invalid."
        }
        require(backend == MeetermTerminalModule.HERDR_BACKEND || runtime.isEmpty()) {
          "The SSH connection options are invalid."
        }
      }
    }
  }

  private companion object {
    const val PUBLIC_KEY_AUTH_METHOD = "publicKey"
    const val PASSWORD_AUTH_METHOD = "password"
    const val TMUX_BACKEND = "tmux"
    const val HERDR_BACKEND = "herdr"
    const val DEFAULT_BACKEND = TMUX_BACKEND
    const val HERDR_RUNTIME_MAX_BYTES = 64
    const val DEFAULT_COLUMNS = 80
    const val DEFAULT_ROWS = 24
    const val STATE_FIELD_COUNT = 8
    const val STATE_DISCONNECTED = 0
    const val STATE_RECONNECTING = 10

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
