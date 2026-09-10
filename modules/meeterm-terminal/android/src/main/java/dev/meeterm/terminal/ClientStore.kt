package dev.meeterm.terminal

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import android.util.Base64
import java.io.File
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray
import org.json.JSONObject

/** Local client metadata only. Credential ciphertext is device-bound by Android
 * Keystore and kept in noBackupFilesDir; plaintext never leaves the native path
 * when a saved profile is opened. The metadata+ciphertext update is atomic. */
internal object ClientStore {
  private const val KEY_ALIAS = "dev.meeterm.credentials.v1"
  private const val MAX_PROFILES = 100
  private const val MAX_STORE_BYTES = 16 * 1024 * 1024

  private fun file(context: Context) = AtomicFile(File(context.noBackupFilesDir, "meeterm-client-v1.json"))

  private fun read(context: Context): JSONObject {
    val source = file(context)
    if (!source.baseFile.exists()) return JSONObject().put("version", 1).put("profiles", JSONArray())
    check(source.baseFile.length() <= MAX_STORE_BYTES) { "Client storage is unavailable." }
    return JSONObject(source.openRead().use { it.readBytes().toString(Charsets.UTF_8) }).also {
      check(it.getInt("version") == 1 && it.getJSONArray("profiles").length() <= MAX_PROFILES) {
        "Client storage is unavailable."
      }
    }
  }

  private fun write(context: Context, state: JSONObject) {
    val bytes = state.toString().toByteArray(Charsets.UTF_8)
    check(bytes.size <= MAX_STORE_BYTES) { "Client storage capacity was exceeded." }
    val destination = file(context)
    val stream = destination.startWrite()
    try {
      stream.write(bytes)
      destination.finishWrite(stream)
    } catch (failure: Exception) {
      destination.failWrite(stream)
      throw failure
    }
  }

  private fun key(create: Boolean): SecretKey {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
    check(create) { "Saved credentials are unavailable. Enter them again." }
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
      init(KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256).build())
    }.generateKey()
  }

  private fun identity(profile: JSONObject): ByteArray = JSONArray().apply {
    for (field in listOf("id", "host", "port", "username", "authMethod")) put(profile.get(field))
  }.toString().toByteArray(Charsets.UTF_8)

  private fun encrypt(profile: JSONObject, credential: JSONObject): JSONObject {
    val plain = credential.toString().toByteArray(Charsets.UTF_8)
    try {
      val cipher = Cipher.getInstance("AES/GCM/NoPadding")
      cipher.init(Cipher.ENCRYPT_MODE, key(true))
      cipher.updateAAD(identity(profile))
      return JSONObject()
        .put("iv", Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
        .put("data", Base64.encodeToString(cipher.doFinal(plain), Base64.NO_WRAP))
    } finally { plain.fill(0) }
  }

  private fun decrypt(profile: JSONObject): JSONObject {
    val encrypted = profile.optJSONObject("credential")
      ?: throw IllegalStateException("No credential is saved for this server.")
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    cipher.init(Cipher.DECRYPT_MODE, key(false), GCMParameterSpec(128, Base64.decode(encrypted.getString("iv"), Base64.NO_WRAP)))
    cipher.updateAAD(identity(profile))
    val plain = cipher.doFinal(Base64.decode(encrypted.getString("data"), Base64.NO_WRAP))
    try { return JSONObject(plain.toString(Charsets.UTF_8)) }
    finally { plain.fill(0) }
  }

  private fun record(profile: JSONObject): Map<String, Any> = mapOf(
    "id" to profile.getString("id"), "name" to profile.getString("name"),
    "host" to profile.getString("host"), "port" to profile.getInt("port"),
    "username" to profile.getString("username"), "authMethod" to profile.getString("authMethod"),
    "credentialSaved" to profile.has("credential"),
  )

  @Synchronized fun profiles(context: Context): List<Map<String, Any>> = guarded {
    val profiles = read(context).getJSONArray("profiles")
    (0 until profiles.length()).map { record(profiles.getJSONObject(it)) }
  }

  @Synchronized fun saveProfile(context: Context, values: Map<String, Any?>,
    credential: Map<String, Any?>?, keepCredential: Boolean): Map<String, Any> = guarded {
    val state = read(context)
    val profiles = state.getJSONArray("profiles")
    val profile = validateProfile(values)
    val id = profile.getString("id")
    val index = (0 until profiles.length()).firstOrNull { profiles.getJSONObject(it).getString("id") == id }
    check(index != null || profiles.length() < MAX_PROFILES) { "Too many saved servers." }
    val previous = index?.let { profiles.getJSONObject(it) }
    if (credential != null) {
      val normalized = validateCredential(profile, credential)
      profile.put("credential", encrypt(profile, normalized))
    } else if (keepCredential && previous != null && identity(previous).contentEquals(identity(profile))) {
      previous.optJSONObject("credential")?.let { profile.put("credential", it) }
    }
    if (index == null) profiles.put(profile) else profiles.put(index, profile)
    write(context, state)
    record(profile)
  }

  @Synchronized fun deleteProfile(context: Context, id: String) = guarded {
    val state = read(context)
    val profiles = state.getJSONArray("profiles")
    val remaining = JSONArray()
    for (index in 0 until profiles.length()) {
      val profile = profiles.getJSONObject(index)
      if (profile.getString("id") != id) remaining.put(profile)
    }
    state.put("profiles", remaining)
    write(context, state)
  }

  /** This result is consumed only by the native connect implementation. */
  @Synchronized fun connectionOptions(context: Context, id: String): Map<String, Any?> = guarded {
    val profiles = read(context).getJSONArray("profiles")
    val profile = (0 until profiles.length()).map { profiles.getJSONObject(it) }
      .firstOrNull { it.getString("id") == id }
      ?: throw IllegalArgumentException("The saved server no longer exists.")
    val secret = decrypt(profile)
    val values = record(profile).toMutableMap<String, Any?>()
    for (field in listOf("privateKey", "passphrase", "password")) {
      if (secret.has(field)) values[field] = secret.getString(field)
    }
    MeetermTerminalModule.SshOptions.from(values)
    values
  }

  @Synchronized fun preferences(context: Context): Map<String, Any> = guarded {
    read(context).optJSONObject("preferences")?.let { validatePreferences(jsonMap(it)) } ?: defaults()
  }

  @Synchronized fun setPreferences(context: Context, values: Map<String, Any?>) = guarded {
    val preferences = validatePreferences(values)
    val state = read(context).put("preferences", JSONObject(preferences))
    write(context, state)
  }

  internal fun defaults(): Map<String, Any> = mapOf("fontSize" to 15, "theme" to "system",
    "scrollbackLines" to 10000, "automaticReconnect" to true)

  internal fun validatePreferences(values: Map<String, Any?>): Map<String, Any> {
    val size = integer(values["fontSize"])
    val history = integer(values["scrollbackLines"])
    val theme = values["theme"] as? String
    val automatic = values["automaticReconnect"] as? Boolean
    require(size != null && size in 10..24 && history != null && history in 1000..50000 &&
      theme in listOf("system", "light", "dark") && automatic != null) { "The terminal preferences are invalid." }
    return mapOf("fontSize" to size, "theme" to requireNotNull(theme), "scrollbackLines" to history,
      "automaticReconnect" to automatic)
  }

  private fun validateProfile(values: Map<String, Any?>): JSONObject {
    val suppliedId = values["id"] as? String ?: ""
    val id = if (suppliedId.isEmpty()) UUID.randomUUID().toString() else suppliedId
    require(id.matches(Regex("[a-fA-F0-9-]{36}"))) { "The server ID is invalid." }
    val profile = JSONObject().put("id", id)
    for (field in listOf("name", "host", "username")) {
      val value = (values[field] as? String)?.trim().orEmpty()
      require(value.isNotEmpty() && value.length <= 256 && value.none(Char::isISOControl)) { "The server details are invalid." }
      profile.put(field, value)
    }
    val port = integer(values["port"])
    require(port != null && port in 1..65535) { "The SSH port is invalid." }
    val method = values["authMethod"]
    require(method == "publicKey" || method == "password") { "The authentication method is invalid." }
    return profile.put("port", port).put("authMethod", method)
  }

  private fun validateCredential(profile: JSONObject, values: Map<String, Any?>): JSONObject {
    require(values["authMethod"] == profile.getString("authMethod")) { "The credential does not match this server." }
    val merged = record(profile).toMutableMap<String, Any?>().apply { putAll(values) }
    val options = MeetermTerminalModule.SshOptions.from(merged)
    val result = JSONObject().put("authMethod", options.authMethod)
    if (options.authMethod == "password") {
      require(options.password.length <= 65536)
      result.put("password", options.password)
    } else {
      require(options.privateKey.length <= 65536 && options.passphrase.length <= 65536)
      result.put("privateKey", options.privateKey).put("passphrase", options.passphrase)
    }
    return result
  }

  private fun integer(value: Any?): Int? {
    val number = (value as? Number)?.toDouble() ?: return null
    return number.toInt().takeIf { number.isFinite() && it.toDouble() == number }
  }
  private fun jsonMap(json: JSONObject): Map<String, Any?> = json.keys().asSequence().associateWith { json.get(it) }
  private inline fun <T> guarded(block: () -> T): T = try { block() }
    catch (_: Exception) { throw IllegalStateException("Saved server storage is unavailable or its values are invalid. Check the details or enter credentials again.") }
}
