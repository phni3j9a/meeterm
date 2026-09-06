package dev.meeterm.terminal

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition

class MeetermTerminalModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("MeetermTerminal")

    AsyncFunction("connect") { terminalId: String, options: Map<String, Any?> ->
      val normalizedId = normalizeTerminalId(terminalId)
      val nativeOptions = SshOptions.from(options)
      val handle = ensureHandle(normalizedId)
      val context = requireNotNull(appContext.reactContext?.applicationContext) {
        "Native application context is unavailable."
      }
      val result = MeetermNative.sshConnect(
        handle,
        nativeOptions.host,
        nativeOptions.port,
        nativeOptions.username,
        nativeOptions.privateKey,
        nativeOptions.passphrase,
        KnownHostsStore.path(context),
      )
      if (result != 0) {
        throw IllegalStateException("The SSH connection could not be started.")
      }
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
      require(paneId.matches(Regex("%[0-9]+"))) { "The pane ID is invalid." }
      val pane = paneId.drop(1).toLongOrNull()
        ?: throw IllegalArgumentException("The pane ID is invalid.")
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      check(MeetermNative.tmuxSelectPane(handle, pane) == 0) { "The terminal could not be selected." }
    }

    AsyncFunction("getSessionState") { terminalId: String ->
      val handle = ensureHandle(normalizeTerminalId(terminalId))
      val fields = MeetermNative.tmuxSessionState(handle)
        ?: throw IllegalStateException("Native session state is unavailable.")
      check(fields.size % 5 == 0) { "Native session state is unavailable." }
      mapOf("panes" to fields.toList().chunked(5).map { pane ->
        mapOf(
          "windowId" to "@${pane[0]}",
          "paneId" to "%${pane[1]}",
          "terminalId" to "native:${pane[2]}",
          "windowName" to sanitize(pane[3], 256),
          "selected" to (pane[4] == "1"),
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
      Prop("terminalId", "poc-main") { view: MeetermTerminalView, terminalId: String ->
        view.bindTerminal(terminalId)
      }
      Events("onNativeReady", "onMetrics")

      OnViewDestroys { view: MeetermTerminalView ->
        view.releaseBindingForLifecycle()
      }
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

  private data class SshOptions(
    val host: String,
    val port: Int,
    val username: String,
    val privateKey: String,
    val passphrase: String,
  ) {
    companion object {
      fun from(values: Map<String, Any?>): SshOptions {
        val host = (values["host"] as? String)?.trim()
        val port = numberAsInt(values["port"])
        val username = (values["username"] as? String)?.trim()
        val privateKey = values["privateKey"] as? String
        val passphrase = values["passphrase"] as? String
        if (host.isNullOrEmpty() || host.any { it.isISOControl() } ||
          port == null || port !in 1..65535 ||
          username.isNullOrEmpty() || username.any { it.isISOControl() } ||
          privateKey.isNullOrEmpty() || privateKey.any { it == '\u0000' } ||
          passphrase == null || passphrase.any { it == '\u0000' }
        ) {
          throw IllegalArgumentException("The SSH connection options are invalid.")
        }
        return SshOptions(host, port, username, privateKey, passphrase)
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
  }
}
