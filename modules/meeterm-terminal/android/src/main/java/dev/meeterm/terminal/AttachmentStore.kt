package dev.meeterm.terminal

import android.content.Context
import android.net.Uri
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.security.MessageDigest
import java.security.SecureRandom

/**
 * App-owned attachment files under `cacheDir/attachments/`.
 *
 * Sources are stream-copied through a bounded buffer; the picker never hands
 * the pipeline an unbounded byte array. Names are app-generated so provider
 * filenames can never traverse or collide inside the directory.
 */
internal class AttachmentStore(private val context: Context) {
  private val random = SecureRandom()

  val directory: File
    get() = File(context.cacheDir, "attachments")

  private fun newFileName(extension: String): String {
    val bytes = ByteArray(12).also { random.nextBytes(it) }
    val token = bytes.joinToString("") { "%02x".format(it) }
    return "att_$token.$extension"
  }

  fun newStagingFileName(): String = newFileName("bin")

  fun newPreparedFileName(format: AttachmentImageFormat): String = newFileName(format.extension)

  fun resolveAppFile(name: String): File? {
    if (!AttachmentFileNames.isValid(name)) return null
    val file = File(directory, name)
    return if (file.parentFile?.canonicalPath == directory.canonicalPath) file else null
  }

  fun stagingFile(name: String): File? = resolveAppFile(name)?.takeIf {
    AttachmentFileNames.isStagingName(name)
  }

  fun preparedFile(name: String): File? = resolveAppFile(name)?.takeIf {
    AttachmentFileNames.isPreparedName(name)
  }

  sealed class StageCopy {
    data class Ok(val fileName: String, val byteCount: Long) : StageCopy()
    data class Rejected(val errorCode: String) : StageCopy()
  }

  /** Bounded Uri → staging-file copy; refuses early on size overruns. */
  fun stageFromUri(uri: Uri, fileName: String): StageCopy {
    val target = stagingFile(fileName) ?: return StageCopy.Rejected(
      AttachmentLimits.ERROR_ARGUMENT,
    )
    directory.mkdirs()
    return try {
      context.contentResolver.openInputStream(uri).use { input ->
        if (input == null) return StageCopy.Rejected(AttachmentLimits.ERROR_IO)
        var total = 0L
        val buffer = ByteArray(STREAM_BUFFER_BYTES)
        FileOutputStream(target).use { output ->
          while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            total += read
            if (total > AttachmentLimits.MAX_SOURCE_BYTES) {
              output.close()
              target.delete()
              return StageCopy.Rejected(AttachmentLimits.ERROR_INPUT_TOO_LARGE)
            }
            output.write(buffer, 0, read)
          }
        }
        if (total <= 0L) {
          target.delete()
          StageCopy.Rejected(AttachmentLimits.ERROR_IO)
        } else {
          StageCopy.Ok(fileName, total)
        }
      }
    } catch (e: SecurityException) {
      target.delete()
      StageCopy.Rejected(AttachmentLimits.ERROR_IO)
    } catch (e: IOException) {
      target.delete()
      StageCopy.Rejected(AttachmentLimits.ERROR_IO)
    }
  }

  fun readHeader(fileName: String): Pair<ByteArray, Long>? {
    val file = stagingFile(fileName) ?: return null
    if (!file.isFile) return null
    val length = file.length()
    return try {
      FileInputStream(file).use { input ->
        val buffer = ByteArray(AttachmentLimits.HEADER_SNIFF_BYTES)
        var offset = 0
        while (offset < buffer.size) {
          val read = input.read(buffer, offset, buffer.size - offset)
          if (read < 0) break
          offset += read
        }
        buffer.copyOf(offset) to length
      }
    } catch (e: IOException) {
      null
    }
  }

  fun stagingPath(fileName: String): File? = stagingFile(fileName)?.takeIf { it.isFile }

  fun preparedPath(fileName: String): File? = preparedFile(fileName)?.takeIf { it.isFile }

  fun previewUri(fileName: String): String {
    val file = preparedFile(fileName) ?: return ""
    return Uri.fromFile(file).toString()
  }

  fun sha256(fileName: String, staging: Boolean): String? {
    val file = (if (staging) stagingFile(fileName) else preparedFile(fileName)) ?: return null
    if (!file.isFile) return null
    val digest = MessageDigest.getInstance("SHA-256")
    return try {
      FileInputStream(file).use { input ->
        val buffer = ByteArray(STREAM_BUFFER_BYTES)
        while (true) {
          val read = input.read(buffer)
          if (read < 0) break
          digest.update(buffer, 0, read)
        }
        digest.digest().joinToString("") { "%02x".format(it) }
      }
    } catch (e: IOException) {
      null
    }
  }

  fun delete(vararg names: String?) {
    for (name in names) {
      val file = name?.let { resolveAppFile(it) } ?: continue
      if (file.isFile) file.delete()
    }
  }

  /**
   * Startup reclaim of abandoned files from a previous process. Anything not
   * referenced by the live session goes away; an active session's files stay.
   */
  fun reclaimStale(keptNames: Set<String>) {
    val dir = directory
    val files = dir.listFiles() ?: return
    for (file in files) {
      if (file.name in keptNames) continue
      // Unrecognized files are reclaimed too: only the app writes here.
      file.delete()
    }
  }

  companion object {
    private const val STREAM_BUFFER_BYTES = 64 * 1024
  }
}
