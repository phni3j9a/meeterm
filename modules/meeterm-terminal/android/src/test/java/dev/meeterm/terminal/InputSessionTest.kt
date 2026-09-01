package dev.meeterm.terminal

import java.nio.charset.StandardCharsets
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class InputSessionTest {
  private class RecordingSink : NativeInputSink {
    val commits = mutableListOf<ByteArray>()
    val specials = mutableListOf<TerminalSpecialKey>()

    override fun commitUtf8(bytes: ByteArray) {
      commits += bytes
    }

    override fun sendSpecial(key: TerminalSpecialKey) {
      specials += key
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
}
