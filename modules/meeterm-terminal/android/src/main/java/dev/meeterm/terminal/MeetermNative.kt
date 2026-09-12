package dev.meeterm.terminal

import android.util.Log

/** JNI surface for the Rust-owned terminal registry. */
internal object MeetermNative {
  init {
    System.loadLibrary("meeterm_core")
  }

  external fun create(columns: Int, rows: Int): Long

  /** Returns a native-only MTRM snapshot, or null for an invalid handle. */
  external fun snapshot(handle: Long): ByteArray?

  /** Returns zero on success. */
  external fun resize(handle: Long, columns: Int, rows: Int): Int

  /** Returns the native commit count after accepting the byte array. */
  external fun commit(handle: Long, bytes: ByteArray): Long

  /** Returns accepted UTF-8 byte count, or a negative transport rejection. */
  external fun paste(handle: Long, bytes: ByteArray): Int

  /** Scroll history by terminal lines; positive is older history. */
  external fun scrollLines(handle: Long, lines: Int): Int

  /** Returns the encoded byte count, or a negative error value. */
  external fun sendSpecial(handle: Long, key: Int): Int
  external fun sendKey(handle: Long, key: Int, modifiers: Int): Int
  external fun commitModified(handle: Long, bytes: ByteArray, modifiers: Int): Int
  external fun selectStart(handle: Long, row: Int, column: Int): Int
  external fun selectUpdate(handle: Long, row: Int, column: Int): Int
  external fun clearSelection(handle: Long): Int
  /** Clipboard data stays native. Null means no selection or invalid handle. */
  external fun selectionText(handle: Long): String?
  external fun setTheme(handle: Long, light: Boolean): Int
  external fun setScrollbackLimit(lines: Int): Int

  external fun inputCommitCount(handle: Long): Long

  /** Returns one when removed, zero when already absent. */
  external fun destroy(handle: Long): Int

  /** Monotonic Rust-owned terminal-content revision. */
  external fun terminalRevision(handle: Long): Long
  external fun terminalExists(handle: Long): Boolean

  external fun sshReconnect(handle: Long): Int
  external fun tmuxCommand(handle: Long, operation: Int, target: Long, name: String): Int
  external fun setForeground(handle: Long, foreground: Boolean): Int
  external fun setTerminalVisible(handle: Long, visible: Boolean): Int
  external fun setAutomaticReconnect(handle: Long, enabled: Boolean): Int
  external fun tmuxSelectPane(handle: Long, pane: Long): Int
  /** One row per pane: window ID, pane ID, terminal handle, window name, selected, active, pane name. */
  external fun tmuxSessionState(handle: Long): Array<String>?

  /** Queue an SSH connect request; zero means the request was accepted. */
  external fun sshConnect(
    handle: Long,
    host: String,
    port: Int,
    username: String,
    privateKey: String,
    passphrase: String,
    knownHostsPath: String,
    authMethod: String,
    password: String,
  ): Int

  /** Queue an SSH connect request for an explicit backend/runtime. */
  external fun sshConnectBackend(
    handle: Long,
    host: String,
    port: Int,
    username: String,
    privateKey: String,
    passphrase: String,
    knownHostsPath: String,
    authMethod: String,
    password: String,
    backend: String,
    runtime: String,
  ): Int

  /** Bounded low-frequency workspace metadata; terminal bytes stay native. */
  external fun workspaceState(handle: Long): String?

  /** Queue an SSH close request; zero means accepted/already closed. */
  external fun sshDisconnect(handle: Long): Int

  /** Native-only lifecycle fields: state, host, port, fingerprint, algorithm,
   * known fingerprint, error code, and sanitized error message. */
  external fun sshConnectionState(handle: Long): Array<String>?

  /** Answer an explicit Rust host-key prompt; zero means accepted. */
  external fun sshRespondHostKey(handle: Long, fingerprint: String, accept: Boolean): Int

  /** Remove one endpoint's trusted key from the Rust-owned trust store. */
  external fun sshForgetHostKey(host: String, port: Int, knownHostsPath: String): Int
}

internal class RustInputSink(
  private val handleProvider: () -> Long,
) : NativeInputSink {
  override fun commitUtf8(bytes: ByteArray): Boolean {
    val handle = handleProvider()
    if (handle == 0L) {
      Log.i(TAG, "IME commit rejected; reason=unbound")
      return false
    }

    val count = try {
      MeetermNative.clearSelection(handle)
      MeetermNative.commit(handle, bytes)
    } catch (_: RuntimeException) {
      // The JNI boundary may surface a transient Rust queue rejection as a
      // Java exception while a connection is opening or has closed. IME
      // callbacks must consume that rejection without taking down the view.
      Log.i(TAG, "IME commit rejected; reason=native_exception")
      return false
    }
    if (count > 0L) {
      // Keep the existing observability signal, but never claim success for a
      // transport rejection (Rust returns zero in that case).
      Log.i(TAG, "IME commit accepted; nativeCount=$count byteCount=${bytes.size}")
      return true
    }
    Log.i(TAG, "IME commit rejected; reason=native_rejection")
    return false
  }

  override fun commitModifiedUtf8(bytes: ByteArray, modifiers: Int): Boolean {
    val handle = handleProvider()
    if (handle == 0L) return false
    return try {
      MeetermNative.clearSelection(handle)
      MeetermNative.commitModified(handle, bytes, modifiers) >= 0
    } catch (_: RuntimeException) {
      false
    }
  }

  override fun sendSpecial(key: TerminalSpecialKey): Boolean {
    val handle = handleProvider()
    if (handle == 0L) return false
    return try {
      MeetermNative.clearSelection(handle)
      MeetermNative.sendSpecial(handle, key.nativeCode) >= 0
    } catch (_: RuntimeException) {
      false
    }
  }

  override fun sendKey(key: TerminalSpecialKey, modifiers: Int): Boolean {
    val handle = handleProvider()
    if (handle == 0L) return false
    return try {
      MeetermNative.clearSelection(handle)
      MeetermNative.sendKey(handle, key.nativeCode, modifiers) >= 0
    } catch (_: RuntimeException) {
      false
    }
  }

  private companion object {
    const val TAG = "MeetermInput"
  }
}
