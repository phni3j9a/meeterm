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

  /** Returns the encoded byte count, or a negative error value. */
  external fun sendSpecial(handle: Long, key: Int): Int

  external fun inputCommitCount(handle: Long): Long

  /** Returns one when removed, zero when already absent. */
  external fun destroy(handle: Long): Int
}

internal class RustInputSink(
  private val handleProvider: () -> Long,
) : NativeInputSink {
  override fun commitUtf8(bytes: ByteArray) {
    val handle = handleProvider()
    if (handle != 0L) {
      val count = MeetermNative.commit(handle, bytes)
      Log.i(TAG, "IME commit accepted; nativeCount=$count byteCount=${bytes.size}")
    }
  }

  override fun sendSpecial(key: TerminalSpecialKey) {
    val handle = handleProvider()
    if (handle != 0L) {
      MeetermNative.sendSpecial(handle, key.nativeCode)
    }
  }

  private companion object {
    const val TAG = "MeetermInput"
  }
}
