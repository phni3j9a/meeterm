package dev.meeterm.terminal

import android.content.Context
import java.io.File

/**
 * Resolves the private application trust-store path used by Rust. The path is
 * created on demand and is never returned to JavaScript. Failure is fatal to
 * the operation so an SSH connection cannot silently proceed without trust
 * persistence.
 */
internal object KnownHostsStore {
  private const val DIRECTORY = "meeterm/ssh"
  private const val FILE_NAME = "known_hosts"

  fun path(context: Context): String {
    try {
      val directory = File(context.applicationContext.filesDir, DIRECTORY)
      if ((!directory.exists() && !directory.mkdirs()) || !directory.isDirectory) {
        throw IllegalStateException("SSH trust storage is unavailable.")
      }
      if (!directory.setReadable(false, false) ||
        !directory.setWritable(false, false) ||
        !directory.setReadable(true, true) ||
        !directory.setWritable(true, true)
      ) {
        throw IllegalStateException("SSH trust storage is unavailable.")
      }

      val file = File(directory, FILE_NAME)
      if (!file.exists() && !file.createNewFile() && !file.exists()) {
        throw IllegalStateException("SSH trust storage is unavailable.")
      }
      if (!file.isFile) {
        throw IllegalStateException("SSH trust storage is unavailable.")
      }

      // Rust owns the records. Keep the file private to this app's UID even on
      // devices whose default umask is permissive.
      if (!file.setReadable(false, false) ||
        !file.setWritable(false, false) ||
        !file.setReadable(true, true) ||
        !file.setWritable(true, true)
      ) {
        throw IllegalStateException("SSH trust storage is unavailable.")
      }
      return file.absolutePath
    } catch (_: Exception) {
      throw IllegalStateException("SSH trust storage is unavailable.")
    }
  }
}
