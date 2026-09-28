package dev.meeterm.terminal

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test

class ClientPreferencesTest {
  @Test fun persistedSettingsKeepBoundsAndNativeTypes() {
    val values = mapOf("fontSize" to 24.0, "theme" to "light", "terminalTheme" to "system",
      "scrollbackLines" to 50000.0, "automaticReconnect" to false)
    assertEquals(mapOf("fontSize" to 24, "theme" to "light", "terminalTheme" to "system",
      "scrollbackLines" to 50000, "automaticReconnect" to false), ClientStore.validatePreferences(values))
  }

  @Test fun invalidSettingsCannotAllocateUnboundedHistoryOrInvalidMetrics() {
    val defaults = ClientStore.defaults()
    for ((field, value) in listOf("fontSize" to 9, "fontSize" to 25, "fontSize" to 15.5,
      "fontSize" to Double.NaN, "scrollbackLines" to 999, "scrollbackLines" to 50001,
      "theme" to "unknown", "terminalTheme" to "unknown", "terminalTheme" to "Light",
      "terminalTheme" to 1, "terminalTheme" to true, "automaticReconnect" to 1)) {
      var rejected = false
      try { ClientStore.validatePreferences(defaults + (field to value)) }
      catch (_: IllegalArgumentException) { rejected = true }
      assertEquals("Invalid preference must be rejected: $field", true, rejected)
    }
  }

  @Test fun legacyPersistedPreferencesNormalizeTerminalThemeToDark() {
    // A pre-#37 document stores only the original four keys. Parsing the real
    // persisted JSON fills the missing terminal theme with Dark while the
    // stored App appearance value is preserved untouched.
    val legacy = JSONObject(
      """{"version":1,"profiles":[],"preferences":{"fontSize":18,"theme":"dark","scrollbackLines":2000,"automaticReconnect":true}}"""
    )
    val migrated = ClientStore.normalizeStoredPreferences(legacy.getJSONObject("preferences"))
    assertEquals("dark", migrated["theme"])
    assertEquals("dark", migrated["terminalTheme"])
    assertEquals(18, migrated["fontSize"])
    assertEquals(2000, migrated["scrollbackLines"])
    assertEquals(true, migrated["automaticReconnect"])

    val stored = JSONObject(
      """{"fontSize":15,"theme":"system","terminalTheme":"light","scrollbackLines":10000,"automaticReconnect":false}"""
    )
    val roundTrip = ClientStore.normalizeStoredPreferences(stored)
    assertEquals("system", roundTrip["theme"])
    assertEquals("light", roundTrip["terminalTheme"])

    assertEquals(ClientStore.defaults(), ClientStore.normalizeStoredPreferences(null))
    assertEquals("dark", ClientStore.defaults()["terminalTheme"])
  }

  @Test fun presentButInvalidStoredTerminalThemeIsRejected() {
    for (invalid in listOf("\"unknown\"", "1", "true", "null")) {
      val stored = JSONObject(
        """{"fontSize":15,"theme":"light","terminalTheme":$invalid,"scrollbackLines":10000,"automaticReconnect":true}"""
      )
      var rejected = false
      try { ClientStore.normalizeStoredPreferences(stored) }
      catch (_: IllegalArgumentException) { rejected = true }
      assertEquals("Invalid stored terminalTheme must be rejected: $invalid", true, rejected)
    }
  }
}
