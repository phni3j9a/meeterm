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
