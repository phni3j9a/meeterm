package dev.meeterm.terminal

import org.junit.Assert.assertEquals
import org.junit.Test

class ClientPreferencesTest {
  @Test fun persistedSettingsKeepBoundsAndNativeTypes() {
    val values = mapOf("fontSize" to 24.0, "theme" to "light", "scrollbackLines" to 50000.0, "automaticReconnect" to false)
    assertEquals(mapOf("fontSize" to 24, "theme" to "light", "scrollbackLines" to 50000, "automaticReconnect" to false), ClientStore.validatePreferences(values))
  }

  @Test fun invalidSettingsCannotAllocateUnboundedHistoryOrInvalidMetrics() {
    val defaults = ClientStore.defaults()
    for ((field, value) in listOf("fontSize" to 9, "fontSize" to 25, "fontSize" to 15.5,
      "fontSize" to Double.NaN, "scrollbackLines" to 999, "scrollbackLines" to 50001,
      "theme" to "unknown", "automaticReconnect" to 1)) {
      var rejected = false
      try { ClientStore.validatePreferences(defaults + (field to value)) }
      catch (_: IllegalArgumentException) { rejected = true }
      assertEquals("Invalid preference must be rejected: $field", true, rejected)
    }
  }
}
