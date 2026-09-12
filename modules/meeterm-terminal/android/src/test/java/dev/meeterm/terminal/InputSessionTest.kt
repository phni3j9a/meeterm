package dev.meeterm.terminal

import java.nio.charset.StandardCharsets
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class InputSessionTest {
  private class RecordingSink : NativeInputSink {
    val commits = mutableListOf<ByteArray>()
    val modifiedCommits = mutableListOf<Pair<ByteArray, Int>>()
    val specials = mutableListOf<TerminalSpecialKey>()
    val modifiedKeys = mutableListOf<Pair<TerminalSpecialKey, Int>>()

    override fun commitUtf8(bytes: ByteArray): Boolean {
      commits += bytes
      return true
    }

    override fun commitModifiedUtf8(bytes: ByteArray, modifiers: Int): Boolean {
      modifiedCommits += bytes to modifiers
      return true
    }

    override fun sendSpecial(key: TerminalSpecialKey): Boolean {
      specials += key
      return true
    }

    override fun sendKey(key: TerminalSpecialKey, modifiers: Int): Boolean {
      modifiedKeys += key to modifiers
      return if (modifiers == 0) sendSpecial(key) else true
    }
  }

  @Test
  fun japaneseCompositionIsNativeUntilOneCommit() {
    val sink = RecordingSink()
    val preeditStates = mutableListOf<String>()
    val session = InputSession(sink, preeditStates::add)

    session.setComposingText("き")
    session.setComposingText("きょう")

    assertEquals(emptyList<ByteArray>(), sink.commits)
    assertEquals("きょう", session.composingText)
    assertEquals(listOf("き", "きょう"), preeditStates)

    session.commitText("今日")

    assertEquals(1, sink.commits.size)
    assertEquals("今日", String(sink.commits.single(), StandardCharsets.UTF_8))
    assertEquals("", session.composingText)
    assertEquals("", preeditStates.last())
  }

  @Test
  fun finishCompositionCommitsPendingTextOnce() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    session.setComposingText("か")
    session.setComposingText("かん")
    session.finishComposingText()
    session.finishComposingText()

    assertEquals(1, sink.commits.size)
    assertEquals("かん", String(sink.commits.single(), StandardCharsets.UTF_8))
    assertTrue(sink.specials.isEmpty())
  }

  @Test
  fun clearCompositionDropsOnlyUncommittedPreedit() {
    val sink = RecordingSink()
    val preeditStates = mutableListOf<String>()
    val session = InputSession(sink, preeditStates::add)

    session.setComposingText("仮入力")
    session.clearComposition()

    assertEquals("", session.composingText)
    assertEquals(emptyList<ByteArray>(), sink.commits)
    assertEquals("", preeditStates.last())
  }

  @Test
  fun independentCommitWithSameTextIsNotSuppressed() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    session.setComposingText("同")
    session.finishComposingText()
    session.commitText("同")

    assertEquals(2, sink.commits.size)
    assertEquals(
      listOf("同", "同"),
      sink.commits.map { String(it, StandardCharsets.UTF_8) },
    )
  }

  @Test
  fun keyDownAndUpEmitOneTerminalKey() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    assertTrue(
      session.handleKeyEvent(
        InputSession.ACTION_DOWN,
        InputSession.KEYCODE_ENTER,
      ),
    )
    assertTrue(
      session.handleKeyEvent(
        InputSession.ACTION_UP,
        InputSession.KEYCODE_ENTER,
      ),
    )

    assertEquals(listOf(TerminalSpecialKey.Enter), sink.specials)
  }

  @Test
  fun specialKeyAndAsciiMappingsAreExplicit() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    assertTrue(session.handleKey(InputSession.KEYCODE_ENTER))
    assertTrue(session.handleKey(InputSession.KEYCODE_DEL))
    assertTrue(session.handleKey(InputSession.KEYCODE_TAB))
    assertTrue(session.handleKey(InputSession.KEYCODE_ESCAPE))
    assertTrue(session.handleKey(InputSession.KEYCODE_DPAD_UP))
    assertTrue(session.handleKey(InputSession.KEYCODE_DPAD_DOWN))
    assertTrue(session.handleKey(InputSession.KEYCODE_DPAD_LEFT))
    assertTrue(session.handleKey(InputSession.KEYCODE_DPAD_RIGHT))
    assertTrue(session.handleKey(29, 'a'.code))

    assertEquals(
      listOf(
        TerminalSpecialKey.Enter,
        TerminalSpecialKey.Backspace,
        TerminalSpecialKey.Tab,
        TerminalSpecialKey.Escape,
        TerminalSpecialKey.Up,
        TerminalSpecialKey.Down,
        TerminalSpecialKey.Left,
        TerminalSpecialKey.Right,
      ),
      sink.specials,
    )
    assertEquals(1, sink.commits.size)
    assertEquals("a", String(sink.commits.single(), StandardCharsets.UTF_8))
  }

  @Test
  fun deletingPreeditEditsLocallyAndDeletingCommittedTextSendsBackspace() {
    val sink = RecordingSink()
    val preeditStates = mutableListOf<String>()
    val session = InputSession(sink, preeditStates::add)

    session.setComposingText("あ😀")
    assertTrue(session.deleteSurroundingText(1, 0))
    assertEquals("あ", session.composingText)
    assertTrue(sink.specials.isEmpty())

    session.finishComposingText()
    assertTrue(session.deleteSurroundingText(2, 0))
    assertEquals("あ", String(sink.commits.single(), StandardCharsets.UTF_8))
    assertEquals(
      listOf(TerminalSpecialKey.Backspace, TerminalSpecialKey.Backspace),
      sink.specials,
    )
  }

  @Test
  fun physicalModifiersUseSharedNativeBitsForKeysAndText() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    assertTrue(
      session.handleKey(
        InputSession.KEYCODE_DPAD_UP,
        modifiers = InputSession.MOD_CTRL or InputSession.MOD_ALT,
      ),
    )
    assertTrue(
      session.handleKey(
        InputSession.KEYCODE_C,
        unicodeCodePoint = 3,
        modifiers = InputSession.MOD_CTRL,
      ),
    )

    assertEquals(
      listOf(TerminalSpecialKey.Up to (InputSession.MOD_CTRL or InputSession.MOD_ALT)),
      sink.modifiedKeys,
    )
    assertEquals(1, sink.modifiedCommits.size)
    assertEquals("c", String(sink.modifiedCommits.single().first, StandardCharsets.UTF_8))
    assertEquals(InputSession.MOD_CTRL, sink.modifiedCommits.single().second)
  }

  @Test
  fun androidMetaStateMapsToStableModifierBits() {
    assertEquals(
      InputSession.MOD_CTRL or InputSession.MOD_ALT or InputSession.MOD_SHIFT,
      InputSession.modifiersForMetaState(
        InputSession.META_CTRL_MASK or InputSession.META_ALT_MASK or InputSession.META_SHIFT_MASK,
      ),
    )
  }

  @Test
  fun accessoryModifiersAreOneShotAcrossCommitFinishDeleteAndKeyUp() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    session.toggleModifier(InputSession.MOD_CTRL)
    assertTrue(session.commitText("a"))
    assertEquals(InputSession.MOD_CTRL, sink.modifiedCommits.single().second)
    assertTrue(session.commitText("b"))
    assertEquals(1, sink.commits.size)

    session.setComposingText("x")
    session.toggleModifier(InputSession.MOD_ALT)
    assertTrue(session.finishComposingText())
    assertEquals(InputSession.MOD_ALT, sink.modifiedCommits.last().second)

    session.toggleModifier(InputSession.MOD_CTRL)
    assertTrue(session.deleteSurroundingText(0, 1))
    assertEquals(
      TerminalSpecialKey.Delete to InputSession.MOD_CTRL,
      sink.modifiedKeys.single(),
    )
    assertTrue(session.deleteSurroundingText(0, 1))
    assertEquals(listOf(TerminalSpecialKey.Delete), sink.specials.takeLast(1))

    session.toggleModifier(InputSession.MOD_ALT)
    assertTrue(
      session.handleKeyEvent(
        InputSession.ACTION_UP,
        InputSession.KEYCODE_ENTER,
      ),
    )
    assertTrue(session.handleKey(InputSession.KEYCODE_ENTER))
    assertEquals(TerminalSpecialKey.Enter, sink.specials.last())
  }

  @Test
  fun emptyCallbacksAndLocalPreeditDeletionKeepOneShotModifierArmed() {
    val sink = RecordingSink()
    val session = InputSession(sink)

    session.toggleModifier(InputSession.MOD_CTRL)
    assertTrue(session.commitText(null))
    assertTrue(session.modifierIsActive(InputSession.MOD_CTRL))
    assertTrue(session.finishComposingText())
    assertTrue(session.modifierIsActive(InputSession.MOD_CTRL))
    assertEquals(false, session.deleteSurroundingText(0, 0))
    assertTrue(session.modifierIsActive(InputSession.MOD_CTRL))

    // This is the IME sequence that previously lost Ctrl: local composition
    // deletion consumed the modifier before the following real commit.
    session.setComposingText("x")
    assertTrue(session.deleteSurroundingText(1, 0))
    assertTrue(session.modifierIsActive(InputSession.MOD_CTRL))
    assertTrue(session.commitText("c"))
    assertEquals(InputSession.MOD_CTRL, sink.modifiedCommits.single().second)
    assertEquals(1, sink.modifiedCommits.size)
  }

  @Test
  fun cancelClearsCompositionAndAccessoryModifiers() {
    val sink = RecordingSink()
    val preeditStates = mutableListOf<String>()
    val session = InputSession(sink, preeditStates::add)

    session.setComposingText("pending")
    session.toggleModifier(InputSession.MOD_CTRL)
    session.cancel()

    assertEquals("", session.composingText)
    assertEquals("", preeditStates.last())
    assertTrue(session.commitText("a"))
    assertEquals(1, sink.commits.size)
    assertTrue(sink.modifiedCommits.isEmpty())
  }
}
