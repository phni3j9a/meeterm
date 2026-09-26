package dev.meeterm.terminal

/**
 * Issue #28 attachment limits and header inspection.
 *
 * Everything in this file is deliberately free of Android classes so the same
 * rules run inside host JVM unit tests. The pipeline that uses it
 * ([AttachmentNormalize]) must keep byte/pixel checks ahead of any decode.
 */
internal object AttachmentLimits {
  /** App-owned staging input ceiling, applied while the provider streams. */
  const val MAX_SOURCE_BYTES: Long = 24L * 1024L * 1024L

  /** Bytes read from a staged file to validate magic + dimensions. */
  const val HEADER_SNIFF_BYTES: Int = 64 * 1024

  /** Per-side input ceiling; larger images are rejected before decode. */
  const val MAX_INPUT_DIMENSION: Int = 16_384

  /** Total input pixel ceiling; larger images are rejected before decode. */
  const val MAX_INPUT_PIXELS: Long = 100_000_000L

  /** Per-side output ceiling. Ordinary phone screenshots stay untouched. */
  const val MAX_OUTPUT_DIMENSION: Int = 4_096

  /** Total output pixel ceiling used to bound the decoded bitmap. */
  const val MAX_OUTPUT_PIXELS: Long = 4_096L * 4_096L

  /** Re-encoded output ceiling; a larger result is an explicit error. */
  const val MAX_OUTPUT_BYTES: Long = 16L * 1024L * 1024L

  /** JPEG re-encode quality for JPEG-sourced attachments. */
  const val JPEG_OUTPUT_QUALITY: Int = 90

  const val ERROR_INPUT_TOO_LARGE = "attachment_too_large"
  const val ERROR_OUTPUT_TOO_LARGE = "attachment_output_too_large"
  const val ERROR_UNSUPPORTED = "attachment_unsupported_format"
  const val ERROR_UNSUPPORTED_HEIC = "attachment_unsupported_heic"
  const val ERROR_MALFORMED = "attachment_malformed_header"
  const val ERROR_DIMENSIONS = "attachment_too_many_pixels"
  const val ERROR_DECODE = "attachment_decode_failed"
  const val ERROR_IO = "attachment_io_failed"
  const val ERROR_STATE = "attachment_invalid_state"
  const val ERROR_MISSING = "attachment_missing"
  const val ERROR_TARGET = "attachment_target_mismatch"
  const val ERROR_ARGUMENT = "attachment_invalid_argument"
  const val REASON_COMPOSING = "composing"
  const val REASON_NO_ATTACHMENT = "no_attachment"
  const val REASON_CORE_PENDING = "core_contract_pending"
}

internal enum class AttachmentImageFormat(val extension: String) {
  PNG("png"),
  JPEG("jpg"),
}

/** Detected magic + declared dimensions, read without decoding pixels. */
internal data class AttachmentImageHeader(
  val format: AttachmentImageFormat,
  val width: Int,
  val height: Int,
)

/** Magic-byte and header-dimension sniffing shared by both pick paths. */
internal object AttachmentImageSniffer {
  private val PNG_MAGIC = byteArrayOf(
    0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
  )
  private val JPEG_SOI = byteArrayOf(0xFF.toByte(), 0xD8.toByte())
  private val HEIC_BRANDS = setOf(
    "heic", "heix", "hevc", "hevx", "heim", "heis", "hevm", "hevs", "mif1", "msf1",
  )

  sealed class SniffResult {
    data class Ok(val header: AttachmentImageHeader) : SniffResult()
    data class Rejected(val errorCode: String) : SniffResult()
  }

  fun sniff(prefix: ByteArray): SniffResult {
    if (prefix.size >= 8 && prefix.copyOfRange(0, 8).contentEquals(PNG_MAGIC)) {
      return sniffPng(prefix)
    }
    if (prefix.size >= 2 && prefix[0] == JPEG_SOI[0] && prefix[1] == JPEG_SOI[1]) {
      return sniffJpeg(prefix)
    }
    if (isHeicFamily(prefix)) {
      return SniffResult.Rejected(AttachmentLimits.ERROR_UNSUPPORTED_HEIC)
    }
    return SniffResult.Rejected(AttachmentLimits.ERROR_UNSUPPORTED)
  }

  fun isHeicFamily(prefix: ByteArray): Boolean {
    // ISO BMFF: a box length (4 bytes) followed by "ftyp" and a brand.
    if (prefix.size < 12 || prefix[4] != 'f'.code.toByte() || prefix[5] != 't'.code.toByte() ||
      prefix[6] != 'y'.code.toByte() || prefix[7] != 'p'.code.toByte()
    ) {
      return false
    }
    val brand = String(prefix.copyOfRange(8, 12), Charsets.US_ASCII).lowercase()
    return HEIC_BRANDS.contains(brand)
  }

  private fun sniffPng(prefix: ByteArray): SniffResult {
    // Signature (8) + IHDR length/type (8) + IHDR payload (13 bytes minimum).
    if (prefix.size < 29) {
      return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
    }
    val ihdrLength = readInt32(prefix, 8)
    val ihdrType = String(prefix.copyOfRange(12, 16), Charsets.US_ASCII)
    if (ihdrLength != 13 || ihdrType != "IHDR") {
      return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
    }
    val width = readInt32(prefix, 16)
    val height = readInt32(prefix, 20)
    if (width <= 0 || height <= 0) {
      return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
    }
    return SniffResult.Ok(AttachmentImageHeader(AttachmentImageFormat.PNG, width, height))
  }

  private fun sniffJpeg(prefix: ByteArray): SniffResult {
    var offset = 2
    val limit = prefix.size
    while (true) {
      // Skip 0xFF fill bytes to the next marker prefix.
      while (offset < limit && prefix[offset] == 0xFF.toByte()) offset += 1
      if (offset + 4 > limit) {
        return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
      }
      val marker = prefix[offset].toInt() and 0xFF
      offset += 1
      when (marker) {
        // Standalone markers without a length field.
        0x01, in 0xD0..0xD7 -> continue
        // Entropy-coded scan reached before a frame header: unusable.
        0xDA -> return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
        // End of image before a frame header.
        0xD9 -> return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
        // Frame headers carry width/height. C4 (DHT) and C8 (JPG) are not.
        in 0xC0..0xCF -> {
          if (marker == 0xC4 || marker == 0xC8 || marker == 0xCC) {
            offset = skipSegment(prefix, offset) ?: return SniffResult.Rejected(
              AttachmentLimits.ERROR_MALFORMED,
            )
            continue
          }
          if (offset + 7 > limit) {
            return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
          }
          val segmentLength = readUint16(prefix, offset)
          if (segmentLength < 8) {
            return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
          }
          val height = readUint16(prefix, offset + 3)
          val width = readUint16(prefix, offset + 5)
          if (width <= 0 || height <= 0) {
            return SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED)
          }
          return SniffResult.Ok(
            AttachmentImageHeader(AttachmentImageFormat.JPEG, width, height),
          )
        }
        else -> {
          offset = skipSegment(prefix, offset) ?: return SniffResult.Rejected(
            AttachmentLimits.ERROR_MALFORMED,
          )
        }
      }
    }
  }

  private fun skipSegment(prefix: ByteArray, offset: Int): Int? {
    if (offset + 2 > prefix.size) return null
    val segmentLength = readUint16(prefix, offset)
    if (segmentLength < 2) return null
    val next = offset + segmentLength
    return if (next in offset + 1..prefix.size) next else null
  }

  private fun readUint16(bytes: ByteArray, offset: Int): Int =
    ((bytes[offset].toInt() and 0xFF) shl 8) or (bytes[offset + 1].toInt() and 0xFF)

  private fun readInt32(bytes: ByteArray, offset: Int): Int =
    ((bytes[offset].toInt() and 0xFF) shl 24) or
      ((bytes[offset + 1].toInt() and 0xFF) shl 16) or
      ((bytes[offset + 2].toInt() and 0xFF) shl 8) or
      (bytes[offset + 3].toInt() and 0xFF)
}

/**
 * Pre-decode dimension validation and downsampling decisions. Failures are
 * ordered so a caller sees the input-size rejection before pixel math.
 */
internal object AttachmentDimensionPolicy {
  sealed class Check {
    data class Ok(val sampleSize: Int, val needsScale: Boolean) : Check()
    data class Rejected(val errorCode: String) : Check()
  }

  fun evaluate(header: AttachmentImageHeader, sourceBytes: Long): Check {
    if (sourceBytes > AttachmentLimits.MAX_SOURCE_BYTES) {
      return Check.Rejected(AttachmentLimits.ERROR_INPUT_TOO_LARGE)
    }
    val width = header.width.toLong()
    val height = header.height.toLong()
    if (header.width > AttachmentLimits.MAX_INPUT_DIMENSION ||
      header.height > AttachmentLimits.MAX_INPUT_DIMENSION
    ) {
      return Check.Rejected(AttachmentLimits.ERROR_DIMENSIONS)
    }
    val pixels = width * height
    // toLong() above makes a 32-bit pixel overflow impossible for inputs
    // already bounded by MAX_INPUT_DIMENSION.
    if (pixels > AttachmentLimits.MAX_INPUT_PIXELS || pixels <= 0) {
      return Check.Rejected(AttachmentLimits.ERROR_DIMENSIONS)
    }
    return Check.Ok(
      sampleSize = sampleSizeFor(header.width, header.height),
      needsScale = header.width > AttachmentLimits.MAX_OUTPUT_DIMENSION ||
        header.height > AttachmentLimits.MAX_OUTPUT_DIMENSION ||
        pixels > AttachmentLimits.MAX_OUTPUT_PIXELS,
    )
  }

  /**
   * Power-of-two decoder subsample that lands the bitmap near the output cap.
   * Keep halving while the next halving still leaves the longest side at or
   * above the cap; a final exact fit happens in [fittedOutputSize].
   */
  fun sampleSizeFor(width: Int, height: Int): Int {
    var sample = 1
    var longest = maxOf(width, height)
    while (longest / 2 >= AttachmentLimits.MAX_OUTPUT_DIMENSION && sample < 16) {
      sample *= 2
      longest /= 2
    }
    return sample
  }

  /** Exact fitted output size once the sampled bitmap is available. */
  fun fittedOutputSize(width: Int, height: Int): Pair<Int, Int> {
    if (width <= 0 || height <= 0) return width to height
    val limit = AttachmentLimits.MAX_OUTPUT_DIMENSION.toDouble()
    val scale = minOf(limit / width, limit / height, 1.0)
    val outWidth = maxOf(1, (width * scale).toInt())
    val outHeight = maxOf(1, (height * scale).toInt())
    return outWidth to outHeight
  }

  /** Mirror-aware EXIF orientation model kept identical across platforms. */
  fun normalizedExifOrientation(value: Int): Int = if (value in 1..8) value else 1

  /** True for orientations 5–8, which swap width and height when applied. */
  fun swapsAxes(orientation: Int): Boolean = normalizedExifOrientation(orientation) >= 5

  enum class OrientationOp { ROTATE_90, ROTATE_180, ROTATE_270, FLIP_HORIZONTAL }

  /**
   * Canonical EXIF orientation ops, applied in list order. Mirrors are folded
   * into a horizontal flip composed with a rotation, matching the platform
   * matrix pipeline; the JVM tests pin every value, including 5 and 7.
   */
  fun orientationOps(orientation: Int): List<OrientationOp> =
    when (normalizedExifOrientation(orientation)) {
      2 -> listOf(OrientationOp.FLIP_HORIZONTAL)
      3 -> listOf(OrientationOp.ROTATE_180)
      4 -> listOf(OrientationOp.ROTATE_180, OrientationOp.FLIP_HORIZONTAL)
      5 -> listOf(OrientationOp.ROTATE_90, OrientationOp.FLIP_HORIZONTAL)
      6 -> listOf(OrientationOp.ROTATE_90)
      7 -> listOf(OrientationOp.ROTATE_270, OrientationOp.FLIP_HORIZONTAL)
      8 -> listOf(OrientationOp.ROTATE_270)
      else -> emptyList()
    }
}

/** Files inside the app-owned attachment directory are named by the app. */
internal object AttachmentFileNames {
  private val NAME_PATTERN = Regex("^[A-Za-z0-9_-]{8,64}\\.(bin|png|jpg)$")

  fun isValid(value: String): Boolean = NAME_PATTERN.matches(value)

  fun isStagingName(value: String): Boolean = isValid(value) && value.endsWith(".bin")
  fun isPreparedName(value: String): Boolean =
    isValid(value) && (value.endsWith(".png") || value.endsWith(".jpg"))
}
