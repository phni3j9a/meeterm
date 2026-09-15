package dev.meeterm.terminal

import org.junit.Assert.assertEquals
import org.junit.Test

class SshOptionsTest {
  @Test
  fun missingAuthMethodKeepsTheLegacyPublicKeyShape() {
    val options = MeetermTerminalModule.SshOptions.from(
      mapOf(
        "host" to "server.example.com",
        "port" to 22,
        "username" to "developer",
        "privateKey" to "-----BEGIN OPENSSH PRIVATE KEY-----\nkey\n-----END OPENSSH PRIVATE KEY-----",
        "passphrase" to "",
      ),
    )

    assertEquals("publicKey", options.authMethod)
    assertEquals("-----BEGIN OPENSSH PRIVATE KEY-----\nkey\n-----END OPENSSH PRIVATE KEY-----", options.privateKey)
    assertEquals("", options.password)
  }

  @Test
  fun passwordModePreservesWhitespaceAndClearsKeyFieldsForTheAbi() {
    val options = MeetermTerminalModule.SshOptions.from(
      mapOf(
        "host" to "server.example.com",
        "port" to 22,
        "username" to "developer",
        "authMethod" to "password",
        "password" to "  pass phrase  ",
      ),
    )

    assertEquals("password", options.authMethod)
    assertEquals("  pass phrase  ", options.password)
    assertEquals("", options.privateKey)
    assertEquals("", options.passphrase)
  }

  @Test
  fun legacyBackendAndRuntimeHintsAreIgnoredByHostOptions() {
    val options = MeetermTerminalModule.SshOptions.from(
      mapOf(
        "host" to "server.example.com", "port" to 22, "username" to "developer",
        "backend" to "herdr", "runtime" to "mobile-1",
        "privateKey" to "key", "passphrase" to "",
      ),
    )
    assertEquals("server.example.com", options.host)
    assertEquals("developer", options.username)
    assertEquals("publicKey", options.authMethod)
  }

  @Test
  fun malformedLegacyBackendAndRuntimeHintsAreIgnoredByHostOptions() {
    val options = MeetermTerminalModule.SshOptions.from(mapOf(
      "host" to "fixture.invalid", "port" to 22, "username" to "fixture",
      "backend" to 1, "privateKey" to "key", "passphrase" to "",
    ))
    assertEquals("fixture.invalid", options.host)
    assertEquals("publicKey", options.authMethod)
  }

  @Test
  fun legacyNamedTmuxRuntimeDoesNotChangeHostOnlyConnect() {
    val options = MeetermTerminalModule.SshOptions.from(mapOf(
      "host" to "server.example.com", "port" to 22, "username" to "developer",
      "runtime" to "named", "privateKey" to "key", "passphrase" to "",
    ))
    assertEquals("server.example.com", options.host)
    assertEquals("publicKey", options.authMethod)
  }

  @Test(expected = IllegalArgumentException::class)
  fun unknownAuthMethodIsRejected() {
    MeetermTerminalModule.SshOptions.from(
      mapOf(
        "host" to "server.example.com",
        "port" to 22,
        "username" to "developer",
        "authMethod" to "keyboardInteractive",
        "password" to "secret",
      ),
    )
  }

  @Test(expected = IllegalArgumentException::class)
  fun emptyPasswordIsRejected() {
    MeetermTerminalModule.SshOptions.from(
      mapOf(
        "host" to "server.example.com",
        "port" to 22,
        "username" to "developer",
        "authMethod" to "password",
        "password" to "",
      ),
    )
  }

  @Test(expected = IllegalArgumentException::class)
  fun nulInPasswordIsRejected() {
    MeetermTerminalModule.SshOptions.from(
      mapOf(
        "host" to "server.example.com",
        "port" to 22,
        "username" to "developer",
        "authMethod" to "password",
        "password" to "secret\u0000value",
      ),
    )
  }
}
