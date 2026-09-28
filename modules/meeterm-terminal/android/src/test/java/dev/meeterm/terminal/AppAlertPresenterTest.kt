package dev.meeterm.terminal

import android.content.DialogInterface
import android.content.res.Configuration
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AppAlertPresenterTest {
  private fun options(
    appearance: Any? = "dark",
    title: Any? = "Discard changes?",
    message: Any? = null,
    buttons: Any? = listOf(
      mapOf("text" to "Keep editing", "style" to "cancel"),
      mapOf("text" to "Discard", "style" to "destructive"),
    ),
    cancelable: Any? = true,
  ): Map<String, Any?> = mutableMapOf<String, Any?>(
    "appearance" to appearance,
    "title" to title,
    "buttons" to buttons,
    "cancelable" to cancelable,
  ).apply { if (message != null) this["message"] = message }

  @Test fun parsesContractAndKeepsOriginalButtonOrder() {
    for ((raw, expected) in listOf(
      "system" to AppAlertPresenter.Appearance.SYSTEM,
      "light" to AppAlertPresenter.Appearance.LIGHT,
      "dark" to AppAlertPresenter.Appearance.DARK,
    )) {
      assertEquals(expected, AppAlertPresenter.parse(options(appearance = raw)).appearance)
    }
    val parsed = AppAlertPresenter.parse(options(
      buttons = listOf(
        mapOf("text" to "System", "style" to "default"),
        mapOf("text" to "Light"),
        mapOf("text" to "Dark", "style" to "cancel"),
        mapOf("text" to "Extra", "style" to "destructive"),
      ),
    ))
    // The existing Alert contract shows at most three buttons; extras are
    // dropped and the surviving indices stay in original JS order.
    assertEquals(3, parsed.buttons.size)
    assertEquals(listOf("System", "Light", "Dark"), parsed.buttons.map { it.text })
    assertEquals(AppAlertPresenter.ButtonStyle.DEFAULT, parsed.buttons[0].style)
    assertEquals(AppAlertPresenter.ButtonStyle.DEFAULT, parsed.buttons[1].style)
    assertEquals(AppAlertPresenter.ButtonStyle.CANCEL, parsed.buttons[2].style)
    assertEquals("Discard changes?", parsed.title)
    assertNull(parsed.message)
    assertEquals(true, parsed.cancelable)
  }

  @Test fun rejectsMalformedOptions() {
    val malformed = listOf(
      options(appearance = null),
      options(appearance = "sepia"),
      options(appearance = 1),
      options(title = null),
      options(title = 42),
      options(message = 7),
      options(cancelable = null),
      options(cancelable = "yes"),
      options(buttons = null),
      options(buttons = emptyList<Map<String, Any?>>()),
      options(buttons = "Discard"),
      options(buttons = listOf("Discard")),
      options(buttons = listOf(mapOf("style" to "cancel"))),
      options(buttons = listOf(mapOf("text" to "Discard", "style" to "primary"))),
    )
    for (raw in malformed) {
      var rejected = false
      try { AppAlertPresenter.parse(raw) }
      catch (_: IllegalArgumentException) { rejected = true }
      assertEquals("Malformed options must be rejected: $raw", true, rejected)
    }
  }

  @Test fun buttonIndexToSlotMappingMatchesAlertOrder() {
    // One button: it is the positive action.
    assertEquals(DialogInterface.BUTTON_POSITIVE, AppAlertPresenter.slotForJsIndex(0, 1))
    // Two buttons: first is negative, last is positive (RN Alert pop order).
    assertEquals(DialogInterface.BUTTON_NEGATIVE, AppAlertPresenter.slotForJsIndex(0, 2))
    assertEquals(DialogInterface.BUTTON_POSITIVE, AppAlertPresenter.slotForJsIndex(1, 2))
    // Three buttons: neutral, negative, positive in original index order.
    assertEquals(DialogInterface.BUTTON_NEUTRAL, AppAlertPresenter.slotForJsIndex(0, 3))
    assertEquals(DialogInterface.BUTTON_NEGATIVE, AppAlertPresenter.slotForJsIndex(1, 3))
    assertEquals(DialogInterface.BUTTON_POSITIVE, AppAlertPresenter.slotForJsIndex(2, 3))
    val slots = (0 until 3).map { AppAlertPresenter.slotForJsIndex(it, 3) }
    assertEquals(3, slots.toSet().size)
  }

  @Test fun completionResolvesSelectionExactlyOnce() {
    val results = mutableListOf<Int?>()
    val completion = AppAlertPresenter.Completion { results.add(it) }
    completion.complete(2)
    // The dismiss callback that follows any button click must not replace
    // the reported index, and no second callback may run.
    completion.complete(null)
    completion.complete(0)
    assertEquals(listOf(2), results)
  }

  @Test fun dismissalOrFailureNeverBecomesAnApprovalIndex() {
    for (first in listOf<Int?>(null, 1)) {
      val results = mutableListOf<Int?>()
      val completion = AppAlertPresenter.Completion { results.add(it) }
      completion.complete(first)
      completion.complete(if (first == null) 1 else null)
      assertEquals(listOf(first), results)
    }
    val dismissed = mutableListOf<Int?>()
    AppAlertPresenter.Completion { dismissed.add(it) }.complete(null)
    assertEquals(listOf<Int?>(null), dismissed)
  }

  @Test fun fixedAppearanceOverridesOnlyTheDialogNightQualifier() {
    assertNull(AppAlertPresenter.nightModeOverride(AppAlertPresenter.Appearance.SYSTEM))
    assertEquals(
      Configuration.UI_MODE_NIGHT_NO,
      AppAlertPresenter.nightModeOverride(AppAlertPresenter.Appearance.LIGHT),
    )
    assertEquals(
      Configuration.UI_MODE_NIGHT_YES,
      AppAlertPresenter.nightModeOverride(AppAlertPresenter.Appearance.DARK),
    )
    assertTrue(Configuration.UI_MODE_NIGHT_YES != Configuration.UI_MODE_NIGHT_NO)
  }
}
