package dev.meeterm.terminal

/**
 * The only input dependency used by the editor session.
 *
 * Keeping this interface independent of Android makes composition and key
 * mapping testable without a device. The implementation used by the view
 * forwards bytes directly to the Rust terminal; it never goes through JS.
 */
internal interface NativeInputSink {
  /** Returns false when Rust rejected the input because transport is closed. */
  fun commitUtf8(bytes: ByteArray): Boolean

  /** Returns false when Rust rejected the input because transport is closed. */
  fun sendSpecial(key: TerminalSpecialKey): Boolean
}

internal enum class TerminalSpecialKey(val nativeCode: Int) {
  Escape(0),
  Tab(1),
  Enter(2),
  Backspace(3),
  Up(4),
  Down(5),
  Left(6),
  Right(7),
}
