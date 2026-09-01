package dev.meeterm.terminal

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.StandardCharsets
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalSnapshotTest {
  @Test
  fun decodesLittleEndianWideAndCombiningCell() {
    val snapshot = TerminalSnapshotParser.parse(snapshotBytes())

    assertNotNull(snapshot)
    assertEquals(4, snapshot!!.columns)
    assertEquals(2, snapshot.rows)
    assertEquals(1, snapshot.cursorRow)
    assertEquals(2, snapshot.cursorColumn)
    assertEquals(1, snapshot.cells.size)

    val cell = snapshot.cells.single()
    assertEquals(0, cell.row)
    assertEquals(1, cell.column)
    assertEquals(2, cell.width)
    assertTrue(cell.isBold)
    assertTrue(cell.isUnderlined)
    assertEquals("界́", cell.text)
    assertEquals(0xff112233.toInt(), cell.foreground)
    assertEquals(0xff040506.toInt(), cell.background)
  }

  @Test
  fun rejectsTruncatedPayloadAndTrailingBytes() {
    val bytes = snapshotBytes()
    assertNull(TerminalSnapshotParser.parse(bytes.copyOf(bytes.size - 1)))
    assertNull(TerminalSnapshotParser.parse(bytes + byteArrayOf(0)))
  }

  @Test
  fun acceptsAValidReplacementCharacter() {
    val snapshot = TerminalSnapshotParser.parse(
      snapshotBytes(base = "�".toByteArray(StandardCharsets.UTF_8)),
    )

    assertNotNull(snapshot)
    assertEquals("�", snapshot!!.cells.single().base)
  }

  @Test
  fun rejectsMalformedUtf8() {
    val malformed = byteArrayOf(0xc3.toByte(), 0x28)
    assertNull(TerminalSnapshotParser.parse(snapshotBytes(base = malformed)))
  }

  private fun snapshotBytes(
    base: ByteArray = "界".toByteArray(StandardCharsets.UTF_8),
    combining: ByteArray = "́".toByteArray(StandardCharsets.UTF_8),
  ): ByteArray {
    val buffer = ByteBuffer
      .allocate(SNAPSHOT_HEADER_SIZE + SNAPSHOT_CELL_METADATA_SIZE + base.size + combining.size)
      .order(ByteOrder.LITTLE_ENDIAN)

    buffer.put(byteArrayOf('M'.code.toByte(), 'T'.code.toByte(), 'R'.code.toByte(), 'M'.code.toByte()))
    buffer.putShort(1)
    buffer.putShort(SNAPSHOT_HEADER_SIZE.toShort())
    buffer.putInt(4)
    buffer.putInt(2)
    buffer.putInt(1)
    buffer.putInt(2)
    buffer.putInt(1)

    buffer.putInt(0)
    buffer.putInt(1)
    buffer.put(2)
    buffer.put(0)
    buffer.putShort((FLAG_BOLD or FLAG_UNDERLINE).toShort())
    buffer.put(byteArrayOf(0x11, 0x22, 0x33, 0xff.toByte()))
    buffer.put(byteArrayOf(0x04, 0x05, 0x06, 0xff.toByte()))
    buffer.putInt(base.size)
    buffer.putInt(combining.size)
    buffer.put(base)
    buffer.put(combining)
    return buffer.array()
  }
}
