package dev.meeterm.terminal

import java.nio.charset.StandardCharsets
import org.junit.Assert.assertFalse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class InputSessionTest {
  private class RecordingSink : NativeInputSink {
    val commits = mutableListOf<ByteArray>()
    val modifiedCommits = mutableListOf<Pair<ByteArray, Int>>()
    val specials = mutableListOf<TerminalSpecialKey>()
    val modifiedKeys = mutableListOf<Pair<TerminalSpecialKey, Int>>()
    var currentOperationEpoch: String? = null
    val epochCommitAttempts = mutableListOf<String>()
    val epochKeyAttempts = mutableListOf<String>()

    override fun commitUtf8(bytes: ByteArray): Boolean {
      commits += bytes
      return true
    }

    override fun commitModifiedUtf8(bytes: ByteArray, modifiers: Int): Boolean {
      modifiedCommits += bytes to modifiers
      return true
    }

    override fun commitUtf8AtEpoch(operationEpoch: String, bytes: ByteArray): Boolean {
      epochCommitAttempts += operationEpoch
      if (operationEpoch != currentOperationEpoch) return false
      return commitUtf8(bytes)
    }

    override fun commitModifiedUtf8AtEpoch(
      operationEpoch: String,
      bytes: ByteArray,
      modifiers: Int,
    ): Boolean {
      epochCommitAttempts += operationEpoch
      if (operationEpoch != currentOperationEpoch) return false
      return commitModifiedUtf8(bytes, modifiers)
    }

    override fun sendSpecial(key: TerminalSpecialKey): Boolean {
      specials += key
      return true
    }

    override fun sendKey(key: TerminalSpecialKey, modifiers: Int): Boolean {
      modifiedKeys += key to modifiers
      return if (modifiers == 0) sendSpecial(key) else true
    }

    override fun sendSpecialAtEpoch(operationEpoch: String, key: TerminalSpecialKey): Boolean {
      epochKeyAttempts += operationEpoch
      if (operationEpoch != currentOperationEpoch) return false
      return sendSpecial(key)
    }

    override fun sendKeyAtEpoch(
      operationEpoch: String,
      key: TerminalSpecialKey,
      modifiers: Int,
    ): Boolean {
      epochKeyAttempts += operationEpoch
      if (operationEpoch != currentOperationEpoch) return false
      return sendKey(key, modifiers)
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

  @Test
  fun oldInputSessionIsRejectedAfterOperationEpochChanges() {
    val sink = RecordingSink()
    sink.currentOperationEpoch = "41"
    val oldSession = InputSession(sink, operationEpoch = "41")

    assertTrue(oldSession.commitText("before-revoke"))
    assertEquals(1, sink.commits.size)

    // Revoke/reacquire changes only the native operation epoch. The old
    // session remains immutable and cannot send either IME text or a key.
    sink.currentOperationEpoch = "42"
    oldSession.setComposingText("stale-ime")
    assertFalse(oldSession.commitText("stale-ime"))
    assertFalse(oldSession.handleKey(InputSession.KEYCODE_ENTER))
    assertEquals(1, sink.commits.size)
    assertEquals(listOf("41", "41"), sink.epochCommitAttempts)
    assertEquals(listOf("41"), sink.epochKeyAttempts)

    val freshSession = InputSession(sink, operationEpoch = "42")
    assertTrue(freshSession.commitText("after-reacquire"))
    assertTrue(freshSession.handleKey(InputSession.KEYCODE_ENTER))
    assertEquals(2, sink.commits.size)
    assertEquals(listOf("41", "41", "42"), sink.epochCommitAttempts)
    assertEquals(listOf("41", "42"), sink.epochKeyAttempts)
  }

  @Test
  fun stalePasteEpochGateRejectsReacquiredTerminalButAllowsCurrentOne() {
    assertTrue(OperationEpochGate.matches("41", "41"))
    assertFalse(OperationEpochGate.matches("41", "42"))
    assertFalse(OperationEpochGate.matches(null, "41"))
    assertFalse(OperationEpochGate.matches("41", null))
  }

  @Test
  fun recoveryBridgeArgumentsStayDecimalLosslessAndBounded() {
    assertEquals(
      "18446744073709551615",
      RecoveryBridgeValidation.parseOperationEpoch("18446744073709551615"),
    )
    assertEquals("0007", RecoveryBridgeValidation.parseOperationEpoch("0007"))

    assertInvalidEpoch("")
    assertInvalidEpoch("18446744073709551616")
    assertInvalidEpoch("1.0")
    assertInvalidEpoch("＋1")
    assertInvalidEpoch(" 1")

    assertTrue(RecoveryBridgeValidation.validRecoveryToken("confirm-日本語"))
    assertTrue(RecoveryBridgeValidation.validRecoveryToken("あ".repeat(42))) // 126 UTF-8 bytes
    assertFalse(RecoveryBridgeValidation.validRecoveryToken("a".repeat(129)))
    assertFalse(RecoveryBridgeValidation.validRecoveryToken("あ".repeat(43))) // 129 UTF-8 bytes
    assertFalse(RecoveryBridgeValidation.validRecoveryToken("line\nfeed"))
    assertFalse(RecoveryBridgeValidation.validRecoveryToken("nul\u0000token"))
  }

  @Test
  fun runtimeBoundaryResultsPreserveNativeMeaningAndPreCallRejection() {
    assertEquals(mapOf("status" to "not_invoked", "errorCode" to "invalid_argument"),
      RuntimeBoundaryBridgeResult.notInvoked())
    assertEquals(mapOf("status" to "accepted"), RuntimeBoundaryBridgeResult.fromNativeCode(0))
    assertEquals(mapOf("status" to "rejected_before_boundary", "errorCode" to "recovery_stale"),
      RuntimeBoundaryBridgeResult.fromNativeCode(-14))
    assertEquals(mapOf("status" to "accepted_after_failure", "errorCode" to "boundary_accepted_failure"),
      RuntimeBoundaryBridgeResult.fromNativeCode(-15))

    var unknownRejected = false
    try {
      RuntimeBoundaryBridgeResult.fromNativeCode(Int.MIN_VALUE)
    } catch (_: IllegalStateException) {
      unknownRejected = true
    }
    assertTrue("Unknown JNI failures must not be classified as pre-boundary", unknownRejected)
  }

  private fun assertInvalidEpoch(value: String) {
    var rejected = false
    try {
      RecoveryBridgeValidation.parseOperationEpoch(value)
    } catch (_: IllegalArgumentException) {
      rejected = true
    }
    assertTrue("Expected invalid operation epoch: $value", rejected)
  }
}
