package dev.meeterm.terminal

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class ClientStoreRuntimeHintTest {
  private val previous = JSONObject()
    .put("id", "00000000-0000-4000-8000-000000000021")
    .put("name", "Fixture")
    .put("host", "server.example.com")
    .put("port", 22)
    .put("username", "developer")
    .put("authMethod", "password")
    .put("backend", "herdr")
    .put("runtime", "dev-session")

  private fun values(host: String = "server.example.com") = mapOf<String, Any?>(
    "id" to previous.getString("id"),
    "name" to "Renamed fixture",
    "host" to host,
    "port" to 22,
    "username" to "developer",
    "authMethod" to "password",
  )

  @Test
  fun editingServerFieldsWithoutRuntimeKeysPreservesSameEndpointHint() {
    val migrated = ClientStore.validateProfile(values(), previous)

    assertEquals("herdr", migrated.getString("backend"))
    assertEquals("dev-session", migrated.getString("runtime"))
  }

  @Test
  fun changingEndpointClearsHintToTmuxCompatibilityDefaults() {
    val migrated = ClientStore.validateProfile(values(host = "other.example.com"), previous)

    assertEquals("tmux", migrated.getString("backend"))
    assertEquals("", migrated.getString("runtime"))
  }

  @Test
  fun namedTmuxRuntimeIsValidAsANonAuthoritativeHint() {
    val migrated = ClientStore.validateProfile(values().toMutableMap().apply {
      put("backend", "tmux")
      put("runtime", "release-prep")
    }, previous)

    assertEquals("tmux", migrated.getString("backend"))
    assertEquals("release-prep", migrated.getString("runtime"))
  }

  @Test
  fun tmuxHintAcceptsUnicodeAndSpacesWithinPublicNameLimit() {
    val runtime = "release 東京 session"
    val migrated = ClientStore.validateProfile(values().toMutableMap().apply {
      put("backend", "tmux")
      put("runtime", runtime)
    }, previous)

    assertEquals("tmux", migrated.getString("backend"))
    assertEquals(runtime, migrated.getString("runtime"))
  }

  @Test
  fun tmuxHintRejectsOversizedUtf8AndControlCharacters() {
    val oversized = values().toMutableMap().apply {
      put("backend", "tmux")
      put("runtime", "あ".repeat(86))
    }
    assertThrows(IllegalArgumentException::class.java) {
      ClientStore.validateProfile(oversized, previous)
    }

    val control = values().toMutableMap().apply {
      put("backend", "tmux")
      put("runtime", "release\nsession")
    }
    assertThrows(IllegalArgumentException::class.java) {
      ClientStore.validateProfile(control, previous)
    }
  }

  @Test
  fun herdrHintRetainsBoundedAsciiRules() {
    val valid = ClientStore.validateProfile(values().toMutableMap().apply {
      put("backend", "herdr")
      put("runtime", "dev-session")
    }, previous)
    assertEquals("dev-session", valid.getString("runtime"))

    listOf("release 東京", "release session", "a".repeat(65), ".", "..", "release\u0000session").forEach { runtime ->
      val invalid = values().toMutableMap().apply {
        put("backend", "herdr")
        put("runtime", runtime)
      }
      assertThrows(IllegalArgumentException::class.java) {
        ClientStore.validateProfile(invalid, previous)
      }
    }
  }

  @Test
  fun malformedHerdrHintCannotBeAcceptedAsAProfileMigration() {
    val invalid = values().toMutableMap().apply {
      put("backend", "herdr")
      put("runtime", "../named-session")
    }

    assertThrows(IllegalArgumentException::class.java) {
      ClientStore.validateProfile(invalid, previous)
    }
  }
}
