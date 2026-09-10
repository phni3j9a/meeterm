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

  /** Commit text with the Rust-owned Ctrl/Alt/Shift encoding contract. */
  fun commitModifiedUtf8(bytes: ByteArray, modifiers: Int): Boolean =
    if (modifiers == 0) commitUtf8(bytes) else false

  /** Returns false when Rust rejected the input because transport is closed. */
  fun sendSpecial(key: TerminalSpecialKey): Boolean

  /** Send one key with the shared native modifier bit field. */
  fun sendKey(key: TerminalSpecialKey, modifiers: Int): Boolean =
    if (modifiers == 0) sendSpecial(key) else false
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
  Interrupt(8),
  Home(9),
  End(10),
  Delete(11),
  Insert(12),
  PageUp(13),
  PageDown(14),
  F1(15),
  F2(16),
  F3(17),
  F4(18),
  F5(19),
  F6(20),
  F7(21),
  F8(22),
  F9(23),
  F10(24),
  F11(25),
  F12(26),
}
