package dev.meeterm.terminal

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import java.io.File
import java.io.FileOutputStream
import java.io.IOException

/**
 * Validation and re-encode pipeline for staged images.
 *
 * Order is deliberate: cheap header checks run before any decode, inSampleSize
 * bounds the decoded bitmap, orientation is applied as matrix ops, output is
 * fitted to the pixel cap, then re-encode to PNG (alpha) or JPEG drops all
 * EXIF/GPS metadata. The byte cap is enforced on the result.
 */
internal class AttachmentNormalize(private val store: AttachmentStore) {
  sealed class Result {
    data class Ok(val image: AttachmentPreparedImage, val file: File) : Result()
    data class Rejected(val errorCode: String, val message: String) : Result()
  }

  fun normalize(stagingFileName: String): Result {
    val (headerBytes, sourceBytes) = store.readHeader(stagingFileName)
      ?: return Result.Rejected(
        AttachmentLimits.ERROR_MISSING,
        "The picked image is no longer available.",
      )
    val header = when (val sniff = AttachmentImageSniffer.sniff(headerBytes)) {
      is AttachmentImageSniffer.SniffResult.Ok -> sniff.header
      is AttachmentImageSniffer.SniffResult.Rejected -> return Result.Rejected(
        sniff.errorCode,
        "The selected file is not a supported PNG or JPEG image.",
      )
    }
    val check = AttachmentDimensionPolicy.evaluate(header, sourceBytes)
    val sampleSize = when (check) {
      is AttachmentDimensionPolicy.Check.Rejected -> return Result.Rejected(
        check.errorCode,
        "The image exceeds the attachment size limits.",
      )
      is AttachmentDimensionPolicy.Check.Ok -> check.sampleSize
    }
    val stagingFile = store.stagingPath(stagingFileName)
      ?: return Result.Rejected(
        AttachmentLimits.ERROR_MISSING,
        "The picked image is no longer available.",
      )

    val options = BitmapFactory.Options().apply {
      inSampleSize = sampleSize
      inPreferredConfig = Bitmap.Config.ARGB_8888
    }
    val decoded = try {
      BitmapFactory.decodeFile(stagingFile.absolutePath, options)
    } catch (e: OutOfMemoryError) {
      null
    } catch (e: RuntimeException) {
      null
    }
    if (decoded == null || decoded.width <= 0 || decoded.height <= 0) {
      return Result.Rejected(
        AttachmentLimits.ERROR_DECODE,
        "The image could not be decoded.",
      )
    }

    val orientation = readOrientation(stagingFile)
    val oriented = try {
      applyOrientation(decoded, orientation)
    } catch (e: OutOfMemoryError) {
      decoded.recycle()
      // A failed transform must never fall back to the unrotated bitmap:
      // the output contract requires the stored orientation to be applied.
      return Result.Rejected(
        AttachmentLimits.ERROR_DECODE,
        "The image could not be oriented.",
      )
    } catch (e: RuntimeException) {
      decoded.recycle()
      return Result.Rejected(
        AttachmentLimits.ERROR_DECODE,
        "The image could not be oriented.",
      )
    }
    if (oriented !== decoded) decoded.recycle()

    val (outWidth, outHeight) = AttachmentDimensionPolicy.fittedOutputSize(
      oriented.width,
      oriented.height,
    )
    val fitted = if (outWidth != oriented.width || outHeight != oriented.height) {
      val scaled = Bitmap.createScaledBitmap(oriented, outWidth, outHeight, true)
      oriented.recycle()
      scaled
    } else {
      oriented
    }

    val format = chooseFormat(header.format, fitted)
    val preparedName = store.newPreparedFileName(format)
    val preparedFile = store.preparedFile(preparedName)
      ?: run {
        fitted.recycle()
        return Result.Rejected(
          AttachmentLimits.ERROR_ARGUMENT,
          "The prepared image name was invalid.",
        )
    }
    return try {
      FileOutputStream(preparedFile).use { output ->
        val ok = when (format) {
          AttachmentImageFormat.PNG -> fitted.compress(
            Bitmap.CompressFormat.PNG,
            100,
            output,
          )
          AttachmentImageFormat.JPEG -> fitted.compress(
            Bitmap.CompressFormat.JPEG,
            AttachmentLimits.JPEG_OUTPUT_QUALITY,
            output,
          )
        }
        if (!ok) throw IOException("compress() returned false")
      }
      val byteCount = preparedFile.length()
      if (byteCount > AttachmentLimits.MAX_OUTPUT_BYTES) {
        preparedFile.delete()
        fitted.recycle()
        return Result.Rejected(
          AttachmentLimits.ERROR_OUTPUT_TOO_LARGE,
          "The normalized image is too large to attach.",
        )
      }
      Result.Ok(
        AttachmentPreparedImage(
          fileName = preparedName,
          format = format,
          width = fitted.width,
          height = fitted.height,
          byteCount = byteCount,
          sourceByteCount = sourceBytes,
        ),
        preparedFile,
      )
    } catch (e: IOException) {
      preparedFile.delete()
      Result.Rejected(
        AttachmentLimits.ERROR_IO,
        "The normalized image could not be written.",
      )
    } finally {
      fitted.recycle()
    }
  }

  /**
   * Keep PNG output for PNG sources and for anything that may carry alpha;
   * JPEG sources re-encode to JPEG so a normal photo stays compact.
   */
  private fun chooseFormat(
    sourceFormat: AttachmentImageFormat,
    bitmap: Bitmap,
  ): AttachmentImageFormat = when {
    sourceFormat == AttachmentImageFormat.PNG -> AttachmentImageFormat.PNG
    bitmap.hasAlpha() -> AttachmentImageFormat.PNG
    else -> AttachmentImageFormat.JPEG
  }

  private fun readOrientation(file: File): Int = try {
    ExifInterface(file).getAttributeInt(
      ExifInterface.TAG_ORIENTATION,
      ExifInterface.ORIENTATION_NORMAL,
    )
  } catch (e: IOException) {
    ExifInterface.ORIENTATION_NORMAL
  } catch (e: RuntimeException) {
    ExifInterface.ORIENTATION_NORMAL
  }

  private fun applyOrientation(source: Bitmap, orientation: Int): Bitmap {
    val ops = AttachmentDimensionPolicy.orientationOps(orientation)
    if (ops.isEmpty()) return source
    val matrix = Matrix()
    for (op in ops) {
      when (op) {
        AttachmentDimensionPolicy.OrientationOp.ROTATE_90 -> matrix.postRotate(90f)
        AttachmentDimensionPolicy.OrientationOp.ROTATE_180 -> matrix.postRotate(180f)
        AttachmentDimensionPolicy.OrientationOp.ROTATE_270 -> matrix.postRotate(270f)
        AttachmentDimensionPolicy.OrientationOp.FLIP_HORIZONTAL ->
          matrix.postScale(-1f, 1f)
      }
    }
    return Bitmap.createBitmap(
      source,
      0,
      0,
      source.width,
      source.height,
      matrix,
      true,
    )
  }
}
