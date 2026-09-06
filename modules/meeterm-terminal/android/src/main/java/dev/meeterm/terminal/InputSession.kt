package dev.meeterm.terminal

import java.nio.charset.StandardCharsets

/**
 * Native-only text composition and key translation state.
 *
 * Android IMEs often call setComposingText several times before one
 * commitText. Composition is deliberately kept here and is only exposed to
 * the native renderer through [onPreeditChanged]. It is never sent to the
 * Rust Term or to JavaScript until commitText arrives.
 */
internal class InputSession(
  private val sink: NativeInputSink,
  private val onPreeditChanged: (String) -> Unit = {},
) {
  private var preedit = ""

  val composingText: String
    get() = preedit

  fun setComposingText(text: CharSequence?) {
    preedit = text?.toString().orEmpty()
    onPreeditChanged(preedit)
  }

  fun commitText(text: CharSequence?): Boolean {
    val committed = text?.toString().orEmpty()
    clearPreedit()
    if (committed.isNotEmpty()) {
      return sink.commitUtf8(committed.toByteArray(StandardCharsets.UTF_8))
    }
    return true
  }

  fun finishComposingText(): Boolean {
    val committed = preedit
    clearPreedit()
    if (committed.isNotEmpty()) {
      return sink.commitUtf8(committed.toByteArray(StandardCharsets.UTF_8))
    }
    return true
  }

  fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
    val before = beforeLength.coerceAtLeast(0)
    val after = afterLength.coerceAtLeast(0)

    if (preedit.isNotEmpty()) {
      // The editor keeps the composing cursor at the end. Delete code points
      // rather than UTF-16 code units so surrogate pairs remain intact.
      preedit = preedit.dropLastCodePoints(before)
      onPreeditChanged(preedit)
      return before > 0 || after > 0
    }

    var accepted = true
    repeat(before) {
      accepted = sink.sendSpecial(TerminalSpecialKey.Backspace) && accepted
    }
    // There is no separate forward-delete key in the Issue #1 ABI. A native
    // IME normally reports deletion before the cursor; consume the after
    // count as handled without inventing a transport byte.
    return (before > 0 || after > 0) && accepted
  }

  fun sendSpecial(key: TerminalSpecialKey): Boolean {
    clearPreedit()
    return sink.sendSpecial(key)
  }

  /** Consume key-up events without emitting their terminal bytes twice. */
  fun handleKeyEvent(action: Int, keyCode: Int, unicodeCodePoint: Int = 0): Boolean {
    return when (action) {
      ACTION_DOWN, ACTION_MULTIPLE -> handleKey(keyCode, unicodeCodePoint)
      ACTION_UP -> canHandleKey(keyCode, unicodeCodePoint)
      else -> false
    }
  }

  /**
   * Translate Android key-code values without taking an Android dependency in
   * this pure input-session class. Values are the platform KeyEvent constants.
   */
  fun handleKey(keyCode: Int, unicodeCodePoint: Int = 0): Boolean {
    val special = specialForKeyCode(keyCode)

    if (special != null) {
      return sendSpecial(special)
    }

    if (isPrintableCodePoint(unicodeCodePoint)) {
      return commitText(String(Character.toChars(unicodeCodePoint)))
    }

    return false
  }

  private fun canHandleKey(keyCode: Int, unicodeCodePoint: Int): Boolean =
    specialForKeyCode(keyCode) != null || isPrintableCodePoint(unicodeCodePoint)

  private fun specialForKeyCode(keyCode: Int): TerminalSpecialKey? =
    when (keyCode) {
      KEYCODE_ESCAPE -> TerminalSpecialKey.Escape
      KEYCODE_TAB -> TerminalSpecialKey.Tab
      KEYCODE_ENTER -> TerminalSpecialKey.Enter
      KEYCODE_DEL -> TerminalSpecialKey.Backspace
      KEYCODE_DPAD_UP -> TerminalSpecialKey.Up
      KEYCODE_DPAD_DOWN -> TerminalSpecialKey.Down
      KEYCODE_DPAD_LEFT -> TerminalSpecialKey.Left
      KEYCODE_DPAD_RIGHT -> TerminalSpecialKey.Right
      else -> null
    }

  private fun isPrintableCodePoint(unicodeCodePoint: Int): Boolean =
    Character.isValidCodePoint(unicodeCodePoint) &&
      unicodeCodePoint != 0 &&
      !Character.isISOControl(unicodeCodePoint)

  private fun clearPreedit() {
    if (preedit.isNotEmpty()) {
      preedit = ""
      onPreeditChanged("")
    }
  }

  private fun String.dropLastCodePoints(count: Int): String {
    if (count == 0 || isEmpty()) return this
    var index = length
    repeat(count) {
      if (index == 0) return@repeat
      index = offsetByCodePoints(index, -1)
    }
    return substring(0, index)
  }

  internal companion object {
    // android.view.KeyEvent constants, kept here to keep JVM unit tests pure.
    const val ACTION_DOWN = 0
    const val ACTION_UP = 1
    const val ACTION_MULTIPLE = 2
    const val KEYCODE_DPAD_UP = 19
    const val KEYCODE_DPAD_DOWN = 20
    const val KEYCODE_DPAD_LEFT = 21
    const val KEYCODE_DPAD_RIGHT = 22
    const val KEYCODE_TAB = 61
    const val KEYCODE_DEL = 67
    const val KEYCODE_ENTER = 66
    const val KEYCODE_ESCAPE = 111
  }
}
