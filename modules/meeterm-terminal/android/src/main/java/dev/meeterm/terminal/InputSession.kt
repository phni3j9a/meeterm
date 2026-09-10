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
  private val onModifiersChanged: (Int) -> Unit = {},
) {
  private var preedit = ""
  private var oneShotModifiers = 0

  val composingText: String
    get() = preedit

  /** Toggle a native toolbar modifier for the next input operation only. */
  fun toggleModifier(modifier: Int) {
    oneShotModifiers = oneShotModifiers xor (modifier and KNOWN_MODIFIERS)
    onModifiersChanged(oneShotModifiers)
  }

  fun modifierIsActive(modifier: Int): Boolean = oneShotModifiers and modifier != 0

  /** Clear composition and toolbar state when this view changes terminal. */
  fun cancel() {
    clearPreedit()
    clearModifiers()
  }

  fun setComposingText(text: CharSequence?) {
    preedit = text?.toString().orEmpty()
    onPreeditChanged(preedit)
  }

  fun commitText(text: CharSequence?, modifiers: Int = 0): Boolean {
    val committed = text?.toString().orEmpty()
    clearPreedit()
    val selectedModifiers = consumeModifiers(modifiers)
    return commitBytes(committed, selectedModifiers)
  }

  fun finishComposingText(): Boolean {
    val committed = preedit
    clearPreedit()
    return commitBytes(committed, consumeModifiers(0))
  }

  /** Drop only the active IME composition before an out-of-band paste. */
  fun clearComposition() {
    clearPreedit()
    clearModifiers()
  }

  fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
    val before = beforeLength.coerceAtLeast(0)
    val after = afterLength.coerceAtLeast(0)
    val selectedModifiers = consumeModifiers(0)

    if (preedit.isNotEmpty()) {
      // The editor keeps the composing cursor at the end. Delete code points
      // rather than UTF-16 code units so surrogate pairs remain intact.
      preedit = preedit.dropLastCodePoints(before)
      onPreeditChanged(preedit)
      return before > 0 || after > 0
    }

    var accepted = true
    var first = true
    repeat(before) {
      accepted = sendWithModifiers(
        TerminalSpecialKey.Backspace,
        if (first) selectedModifiers else 0,
      ) && accepted
      first = false
    }
    repeat(after) {
      accepted = sendWithModifiers(
        TerminalSpecialKey.Delete,
        if (first) selectedModifiers else 0,
      ) && accepted
      first = false
    }
    return (before > 0 || after > 0) && accepted
  }

  fun sendSpecial(key: TerminalSpecialKey): Boolean {
    clearPreedit()
    return sendWithModifiers(key, consumeModifiers(0))
  }

  fun sendKey(key: TerminalSpecialKey, modifiers: Int): Boolean {
    clearPreedit()
    return sendWithModifiers(key, consumeModifiers(modifiers))
  }

  /** Consume key-up events without emitting their terminal bytes twice. */
  fun handleKeyEvent(
    action: Int,
    keyCode: Int,
    unicodeCodePoint: Int = 0,
    metaState: Int = 0,
  ): Boolean {
    val modifiers = modifiersForMetaState(metaState)
    return when (action) {
      ACTION_DOWN, ACTION_MULTIPLE -> handleKey(keyCode, unicodeCodePoint, modifiers)
      ACTION_UP -> {
        // A physical key-up must not leave an accessory modifier armed when
        // Android delivered the matching key-down through another editor
        // callback. Toolbar buttons emit their operation on touch-down and
        // therefore do not depend on this path.
        clearModifiers()
        canHandleKey(keyCode, unicodeCodePoint)
      }
      else -> false
    }
  }

  /**
   * Translate Android key-code values without taking an Android dependency in
   * this pure input-session class. Values are the platform KeyEvent constants.
   */
  fun handleKey(keyCode: Int, unicodeCodePoint: Int = 0, modifiers: Int = 0): Boolean {
    val special = specialForKeyCode(keyCode)

    if (special != null) {
      return sendKey(special, modifiers)
    }

    val text = keyText(keyCode, unicodeCodePoint)
    if (text != null) {
      return commitText(text, modifiers)
    }

    return false
  }

  private fun canHandleKey(keyCode: Int, unicodeCodePoint: Int): Boolean =
    specialForKeyCode(keyCode) != null ||
      isPrintableCodePoint(unicodeCodePoint) ||
      letterForKeyCode(keyCode) != null

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
      KEYCODE_MOVE_HOME, KEYCODE_HOME -> TerminalSpecialKey.Home
      KEYCODE_MOVE_END -> TerminalSpecialKey.End
      KEYCODE_FORWARD_DEL -> TerminalSpecialKey.Delete
      KEYCODE_INSERT -> TerminalSpecialKey.Insert
      KEYCODE_PAGE_UP -> TerminalSpecialKey.PageUp
      KEYCODE_PAGE_DOWN -> TerminalSpecialKey.PageDown
      KEYCODE_F1 -> TerminalSpecialKey.F1
      KEYCODE_F2 -> TerminalSpecialKey.F2
      KEYCODE_F3 -> TerminalSpecialKey.F3
      KEYCODE_F4 -> TerminalSpecialKey.F4
      KEYCODE_F5 -> TerminalSpecialKey.F5
      KEYCODE_F6 -> TerminalSpecialKey.F6
      KEYCODE_F7 -> TerminalSpecialKey.F7
      KEYCODE_F8 -> TerminalSpecialKey.F8
      KEYCODE_F9 -> TerminalSpecialKey.F9
      KEYCODE_F10 -> TerminalSpecialKey.F10
      KEYCODE_F11 -> TerminalSpecialKey.F11
      KEYCODE_F12 -> TerminalSpecialKey.F12
      else -> null
    }

  private fun keyText(keyCode: Int, unicodeCodePoint: Int): String? {
    if (isPrintableCodePoint(unicodeCodePoint)) {
      return String(Character.toChars(unicodeCodePoint))
    }
    // Android reports a C0 control code for Ctrl-letter key events. Recover
    // the letter from the physical key so Rust can apply its canonical
    // Ctrl/Alt mapping instead of dropping the event.
    return letterForKeyCode(keyCode) ?: if (keyCode == KEYCODE_SPACE) " " else null
  }

  private fun letterForKeyCode(keyCode: Int): String? {
    if (keyCode !in KEYCODE_A..KEYCODE_Z) return null
    return ('a'.code + keyCode - KEYCODE_A).toChar().toString()
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

  private fun clearModifiers() {
    if (oneShotModifiers != 0) {
      oneShotModifiers = 0
      onModifiersChanged(0)
    }
  }

  private fun consumeModifiers(explicit: Int): Int {
    val selected = if (explicit != 0) explicit else oneShotModifiers
    clearModifiers()
    return selected and KNOWN_MODIFIERS
  }

  private fun commitBytes(text: String, modifiers: Int): Boolean {
    if (text.isEmpty()) return true
    val bytes = text.toByteArray(StandardCharsets.UTF_8)
    return if (modifiers == 0) sink.commitUtf8(bytes)
    else sink.commitModifiedUtf8(bytes, modifiers)
  }

  private fun sendWithModifiers(key: TerminalSpecialKey, modifiers: Int): Boolean =
    if (modifiers == 0) sink.sendSpecial(key) else sink.sendKey(key, modifiers)

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
    const val KEYCODE_HOME = 3
    const val KEYCODE_TAB = 61
    const val KEYCODE_DEL = 67
    const val KEYCODE_FORWARD_DEL = 112
    const val KEYCODE_INSERT = 124
    const val KEYCODE_PAGE_UP = 92
    const val KEYCODE_PAGE_DOWN = 93
    const val KEYCODE_MOVE_HOME = 122
    const val KEYCODE_MOVE_END = 123
    const val KEYCODE_ENTER = 66
    const val KEYCODE_ESCAPE = 111
    const val KEYCODE_SPACE = 62
    const val KEYCODE_A = 29
    const val KEYCODE_C = 31
    const val KEYCODE_Z = 54
    const val KEYCODE_F1 = 131
    const val KEYCODE_F2 = 132
    const val KEYCODE_F3 = 133
    const val KEYCODE_F4 = 134
    const val KEYCODE_F5 = 135
    const val KEYCODE_F6 = 136
    const val KEYCODE_F7 = 137
    const val KEYCODE_F8 = 138
    const val KEYCODE_F9 = 139
    const val KEYCODE_F10 = 140
    const val KEYCODE_F11 = 141
    const val KEYCODE_F12 = 142

    const val MOD_CTRL = 1
    const val MOD_ALT = 2
    const val MOD_SHIFT = 4
    private const val KNOWN_MODIFIERS = MOD_CTRL or MOD_ALT or MOD_SHIFT

    // Android's META_* values are stable bit fields. Include left/right
    // variants so physical keyboards produce the same Rust ABI bits.
    const val META_SHIFT_MASK = 0xC1
    const val META_ALT_MASK = 0x32
    const val META_CTRL_MASK = 0x7000

    fun modifiersForMetaState(metaState: Int): Int {
      var modifiers = 0
      if (metaState and META_CTRL_MASK != 0) modifiers = modifiers or MOD_CTRL
      if (metaState and META_ALT_MASK != 0) modifiers = modifiers or MOD_ALT
      if (metaState and META_SHIFT_MASK != 0) modifiers = modifiers or MOD_SHIFT
      return modifiers
    }
  }
}
