package dev.meeterm.terminal

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-JVM coverage of the Issue #28 attachment pure layer: magic sniffing,
 * dimension/byte caps, the eight-orientation model, filename validation, and
 * the IME-safe insertion policy. The Android/Bitmap pipeline is exercised on
 * device; these tests pin the rules that guard it.
 */
class AttachmentLimitsTest {
  private fun png(width: Int, height: Int): ByteArray {
    val signature = byteArrayOf(
      0x89.toByte(), 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
    )
    val ihdr = ByteArray(25)
    ihdr[3] = 13
    "IHDR".toByteArray(Charsets.US_ASCII).copyInto(ihdr, 4)
    ihdr[8] = (width shr 24).toByte()
    ihdr[9] = (width shr 16).toByte()
    ihdr[10] = (width shr 8).toByte()
    ihdr[11] = width.toByte()
    ihdr[12] = (height shr 24).toByte()
    ihdr[13] = (height shr 16).toByte()
    ihdr[14] = (height shr 8).toByte()
    ihdr[15] = height.toByte()
    return signature + ihdr
  }

  /** Minimal JFIF layout: SOI, APP0, then a baseline SOF0 frame header. */
  private fun jpeg(width: Int, height: Int): ByteArray {
    val bytes = ByteArray(64)
    bytes[0] = 0xFF.toByte()
    bytes[1] = 0xD8.toByte()
    bytes[2] = 0xFF.toByte()
    bytes[3] = 0xE0.toByte()
    bytes[4] = 0x00.toByte()
    bytes[5] = 0x10.toByte()
    "JFIF".toByteArray(Charsets.US_ASCII).copyInto(bytes, 6)
    var offset = 2 + 2 + 0x10
    bytes[offset] = 0xFF.toByte()
    bytes[offset + 1] = 0xC0.toByte()
    bytes[offset + 2] = 0x00.toByte()
    bytes[offset + 3] = 0x11.toByte()
    bytes[offset + 4] = 8
    bytes[offset + 5] = (height shr 8).toByte()
    bytes[offset + 6] = height.toByte()
    bytes[offset + 7] = (width shr 8).toByte()
    bytes[offset + 8] = width.toByte()
    return bytes
  }

  @Test
  fun sniffsPngHeaderWithDimensions() {
    val result = AttachmentImageSniffer.sniff(png(640, 480))
    assertEquals(
      AttachmentImageSniffer.SniffResult.Ok(
        AttachmentImageHeader(AttachmentImageFormat.PNG, 640, 480),
      ),
      result,
    )
  }

  @Test
  fun sniffsJpegHeaderAfterAppSegments() {
    val result = AttachmentImageSniffer.sniff(jpeg(1024, 768))
    assertEquals(
      AttachmentImageSniffer.SniffResult.Ok(
        AttachmentImageHeader(AttachmentImageFormat.JPEG, 1024, 768),
      ),
      result,
    )
  }

  @Test
  fun rejectsTruncatedPngHeader() {
    assertEquals(
      AttachmentImageSniffer.SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED),
      AttachmentImageSniffer.sniff(png(10, 10).copyOf(20)),
    )
  }

  @Test
  fun rejectsPngWithZeroDimensions() {
    assertEquals(
      AttachmentImageSniffer.SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED),
      AttachmentImageSniffer.sniff(png(0, 480)),
    )
  }

  @Test
  fun rejectsJpegTruncatedInsideFrameSegment() {
    val truncated = jpeg(1024, 768).copyOf(24)
    assertEquals(
      AttachmentImageSniffer.SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED),
      AttachmentImageSniffer.sniff(truncated),
    )
  }

  @Test
  fun rejectsJpegWhoseFirstMarkerStartsEntropyScan() {
    val bytes = byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xDA.toByte(), 0, 8, 1, 1)
    assertEquals(
      AttachmentImageSniffer.SniffResult.Rejected(AttachmentLimits.ERROR_MALFORMED),
      AttachmentImageSniffer.sniff(bytes),
    )
  }

  @Test
  fun rejectsUnsupportedAndHeicFamiliesSeparately() {
    val gif = "GIF89a".toByteArray(Charsets.US_ASCII) + ByteArray(16)
    assertEquals(
      AttachmentImageSniffer.SniffResult.Rejected(AttachmentLimits.ERROR_UNSUPPORTED),
      AttachmentImageSniffer.sniff(gif),
    )
    val heic = ByteArray(16)
    heic[3] = 24
    "ftypheic".toByteArray(Charsets.US_ASCII).copyInto(heic, 4)
    assertEquals(
      AttachmentImageSniffer.SniffResult.Rejected(AttachmentLimits.ERROR_UNSUPPORTED_HEIC),
      AttachmentImageSniffer.sniff(heic),
    )
  }

  @Test
  fun enforcesInputByteCeilingBeforeDecode() {
    val check = AttachmentDimensionPolicy.evaluate(
      AttachmentImageHeader(AttachmentImageFormat.JPEG, 800, 600),
      AttachmentLimits.MAX_SOURCE_BYTES + 1,
    )
    assertEquals(
      AttachmentDimensionPolicy.Check.Rejected(AttachmentLimits.ERROR_INPUT_TOO_LARGE),
      check,
    )
  }

  @Test
  fun enforcesPerSideAndPixelCeilings() {
    assertEquals(
      AttachmentDimensionPolicy.Check.Rejected(AttachmentLimits.ERROR_DIMENSIONS),
      AttachmentDimensionPolicy.evaluate(
        AttachmentImageHeader(AttachmentImageFormat.PNG, AttachmentLimits.MAX_INPUT_DIMENSION + 1, 2),
        1024,
      ),
    )
    assertEquals(
      AttachmentDimensionPolicy.Check.Rejected(AttachmentLimits.ERROR_DIMENSIONS),
      AttachmentDimensionPolicy.evaluate(
        AttachmentImageHeader(AttachmentImageFormat.PNG, 16_384, 16_384),
        1024,
      ),
    )
  }

  @Test
  fun acceptsOrdinarySizesWithoutScaling() {
    val check = AttachmentDimensionPolicy.evaluate(
      AttachmentImageHeader(AttachmentImageFormat.JPEG, 4032, 3024),
      6_000_000,
    )
    assertEquals(
      AttachmentDimensionPolicy.Check.Ok(sampleSize = 1, needsScale = false),
      check,
    )
  }

  @Test
  fun choosesPowerOfTwoSubsampleAndFittedOutput() {
    assertEquals(2, AttachmentDimensionPolicy.sampleSizeFor(8_192, 4_096))
    assertEquals(4, AttachmentDimensionPolicy.sampleSizeFor(16_384, 16_384))
    assertEquals(
      4_096 to 2_048,
      AttachmentDimensionPolicy.fittedOutputSize(8_192, 4_096),
    )
    // Portrait keeps its aspect when capped.
    assertEquals(
      2_048 to 4_096,
      AttachmentDimensionPolicy.fittedOutputSize(4_096, 8_192),
    )
  }

  @Test
  fun pinsAllEightExifOrientations() {
    fun ops(orientation: Int) = AttachmentDimensionPolicy.orientationOps(orientation)
    val flip = AttachmentDimensionPolicy.OrientationOp.FLIP_HORIZONTAL
    val rotate90 = AttachmentDimensionPolicy.OrientationOp.ROTATE_90
    val rotate180 = AttachmentDimensionPolicy.OrientationOp.ROTATE_180
    val rotate270 = AttachmentDimensionPolicy.OrientationOp.ROTATE_270
    assertEquals(emptyList<Any>(), ops(1))
    assertEquals(listOf(flip), ops(2))
    assertEquals(listOf(rotate180), ops(3))
    assertEquals(listOf(rotate180, flip), ops(4))
    assertEquals(listOf(rotate90, flip), ops(5))
    assertEquals(listOf(rotate90), ops(6))
    assertEquals(listOf(rotate270, flip), ops(7))
    assertEquals(listOf(rotate270), ops(8))
    // Out-of-range EXIF values normalize to identity rather than mirroring.
    assertEquals(emptyList<Any>(), AttachmentDimensionPolicy.orientationOps(0))
    assertEquals(emptyList<Any>(), AttachmentDimensionPolicy.orientationOps(9))
    assertFalse(AttachmentDimensionPolicy.swapsAxes(4))
    assertTrue(AttachmentDimensionPolicy.swapsAxes(5))
    assertTrue(AttachmentDimensionPolicy.swapsAxes(8))
  }

  @Test
  fun validatesAppOwnedFileNames() {
    assertTrue(AttachmentFileNames.isValid("att_0011223344556677.bin"))
    assertTrue(AttachmentFileNames.isStagingName("att_0011223344556677.bin"))
    assertTrue(AttachmentFileNames.isPreparedName("att_0011223344556677.png"))
    assertTrue(AttachmentFileNames.isPreparedName("att_0011223344556677.jpg"))
    assertFalse(AttachmentFileNames.isValid("../evil.png"))
    assertFalse(AttachmentFileNames.isValid("att_0011223344556677.gif"))
    assertFalse(AttachmentFileNames.isValid("att_0011223344556677.png/escape"))
    assertFalse(AttachmentFileNames.isStagingName("att_0011223344556677.png"))
    assertFalse(AttachmentFileNames.isValid("short.png"))
  }

  @Test
  fun holdsInsertionWhileCompositionIsActive() {
    assertEquals(
      AttachmentInsertionPolicy.Verdict.HeldComposing,
      AttachmentInsertionPolicy.insert(
        composing = true,
        hasActiveSession = true,
        hasPreparedImage = true,
        hasUploadedPath = true,
        sessionTerminalId = "poc-main",
        requestTerminalId = "poc-main",
      ),
    )
    // Composition protection precedes even missing-session handling so the
    // IME state is always honored first.
    assertEquals(
      AttachmentInsertionPolicy.Verdict.HeldComposing,
      AttachmentInsertionPolicy.insert(
        composing = true,
        hasActiveSession = false,
        hasPreparedImage = false,
        hasUploadedPath = false,
        sessionTerminalId = null,
        requestTerminalId = "poc-main",
      ),
    )
  }

  @Test
  fun reportsHeldAndTargetErrorsWithoutComposition() {
    assertEquals(
      AttachmentInsertionPolicy.Verdict.HeldNoAttachment,
      AttachmentInsertionPolicy.insert(
        composing = false,
        hasActiveSession = false,
        hasPreparedImage = false,
        hasUploadedPath = false,
        sessionTerminalId = null,
        requestTerminalId = "poc-main",
      ),
    )
    assertEquals(
      AttachmentInsertionPolicy.Verdict.Rejected(
        AttachmentLimits.ERROR_TARGET,
        "The attachment belongs to a different terminal.",
      ),
      AttachmentInsertionPolicy.insert(
        composing = false,
        hasActiveSession = true,
        hasPreparedImage = true,
        hasUploadedPath = false,
        sessionTerminalId = "other-terminal",
        requestTerminalId = "poc-main",
      ),
    )
    assertEquals(
      AttachmentInsertionPolicy.Verdict.ReadyToInsert,
      AttachmentInsertionPolicy.insert(
        composing = false,
        hasActiveSession = true,
        hasPreparedImage = true,
        hasUploadedPath = false,
        sessionTerminalId = "poc-main",
        requestTerminalId = "poc-main",
      ),
    )
  }

  @Test
  fun beginsOnlyWhenCompositionIsIdle() {
    assertEquals(
      AttachmentInsertionPolicy.Verdict.HeldComposing,
      AttachmentInsertionPolicy.begin(composing = true, hasActiveSession = false),
    )
    assertEquals(
      AttachmentInsertionPolicy.Verdict.ReadyToInsert,
      AttachmentInsertionPolicy.begin(composing = false, hasActiveSession = true),
    )
  }

  @Test
  fun sessionSnapshotCarriesOnlyDisplayMetadata() {
    val session = AttachmentSession(
      AttachmentTargetIdentity("poc-main", "%1", "@1", "tmux", "meeterm", "host", 22),
    )
    assertEquals("idle", session.snapshot("")["status"])
    session.stagingFileName = "att_0011223344556677.bin"
    assertEquals("staged", session.snapshot("")["status"])
    session.prepared = AttachmentPreparedImage(
      fileName = "att_8899aabbccddeeff.png",
      format = AttachmentImageFormat.PNG,
      width = 1080,
      height = 1920,
      byteCount = 123_456,
      sourceByteCount = 4_000_000,
    )
    val snapshot = session.snapshot("file:///cache/attachments/att_8899aabbccddeeff.png")
    assertEquals("prepared", snapshot["status"])
    assertEquals("att_8899aabbccddeeff.png", snapshot["fileId"])
    assertEquals("file:///cache/attachments/att_8899aabbccddeeff.png", snapshot["previewUri"])
    assertEquals(1080, snapshot["width"])
    assertEquals(1920, snapshot["height"])
    session.remotePath = "/tmp/meeterm-attach/att.png"
    assertEquals("uploaded", session.snapshot("")["status"])
  }
}
