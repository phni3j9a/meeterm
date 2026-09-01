package dev.meeterm.terminal

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets

internal const val SNAPSHOT_HEADER_SIZE = 28
internal const val SNAPSHOT_CELL_METADATA_SIZE = 28
private const val SNAPSHOT_VERSION = 1

internal const val FLAG_INVERSE = 1 shl 0
internal const val FLAG_BOLD = 1 shl 1
internal const val FLAG_UNDERLINE = 1 shl 3
internal const val FLAG_WIDE_CHAR = 1 shl 5
internal const val FLAG_HIDDEN = 1 shl 8
internal const val FLAG_DOUBLE_UNDERLINE = 1 shl 11
internal const val FLAG_UNDERCURL = 1 shl 12
internal const val FLAG_DOTTED_UNDERLINE = 1 shl 13
internal const val FLAG_DASHED_UNDERLINE = 1 shl 14

internal data class TerminalCell(
  val row: Int,
  val column: Int,
  val width: Int,
  val flags: Int,
  val foreground: Int,
  val background: Int,
  val base: String,
  val combining: String,
) {
  val text: String
    get() = base + combining

  val isHidden: Boolean
    get() = flags and FLAG_HIDDEN != 0

  val isBold: Boolean
    get() = flags and FLAG_BOLD != 0

  val isUnderlined: Boolean
    get() = flags and (
    FLAG_UNDERLINE or FLAG_DOUBLE_UNDERLINE or FLAG_UNDERCURL or
      FLAG_DOTTED_UNDERLINE or FLAG_DASHED_UNDERLINE
  ) != 0
}

internal data class TerminalSnapshot(
  val columns: Int,
  val rows: Int,
  val cursorRow: Int,
  val cursorColumn: Int,
  val cells: List<TerminalCell>,
)

/** Defensive decoder for the native MTRM versioned little-endian snapshot. */
internal object TerminalSnapshotParser {
  fun parse(bytes: ByteArray): TerminalSnapshot? {
    if (bytes.size < SNAPSHOT_HEADER_SIZE) return null

    val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
    if (buffer.get() != 'M'.code.toByte() ||
      buffer.get() != 'T'.code.toByte() ||
      buffer.get() != 'R'.code.toByte() ||
      buffer.get() != 'M'.code.toByte()
    ) {
      return null
    }

    val version = buffer.getShort().toInt() and 0xffff
    val headerSize = buffer.getShort().toInt() and 0xffff
    if (version != SNAPSHOT_VERSION ||
      headerSize < SNAPSHOT_HEADER_SIZE ||
      headerSize > bytes.size
    ) {
      return null
    }

    val columns = buffer.getInt()
    val rows = buffer.getInt()
    val cursorRow = buffer.getInt()
    val cursorColumn = buffer.getInt()
    val cellCount = buffer.getInt()

    if (columns < 2 || rows < 1 ||
      columns > 4096 || rows > 4096 ||
      cellCount < 0 || cellCount > columns.toLong() * rows.toLong()
    ) {
      return null
    }

    if (headerSize > SNAPSHOT_HEADER_SIZE) {
      buffer.position(headerSize)
    }

    val cells = ArrayList<TerminalCell>(cellCount)
    repeat(cellCount) {
      if (buffer.remaining() < SNAPSHOT_CELL_METADATA_SIZE) return null

      val row = buffer.getInt()
      val column = buffer.getInt()
      val width = buffer.get().toInt() and 0xff
      buffer.get() // Reserved byte.
      val flags = buffer.getShort().toInt() and 0xffff
      val foreground = rgba(buffer)
      val background = rgba(buffer)
      val baseLength = buffer.getInt()
      val combiningLength = buffer.getInt()

      if (row < 0 || row >= rows || column < 0 || column >= columns ||
        (width != 1 && width != 2) ||
        baseLength < 0 || combiningLength < 0
      ) {
        return null
      }

      val payloadLength = baseLength.toLong() + combiningLength.toLong()
      if (payloadLength > buffer.remaining().toLong()) return null

      val base = readUtf8(buffer, baseLength) ?: return null
      val combining = readUtf8(buffer, combiningLength) ?: return null
      cells += TerminalCell(
        row = row,
        column = column,
        width = width,
        flags = flags,
        foreground = foreground,
        background = background,
        base = base,
        combining = combining,
      )
    }

    if (buffer.hasRemaining()) return null
    return TerminalSnapshot(columns, rows, cursorRow, cursorColumn, cells)
  }

  private fun rgba(buffer: ByteBuffer): Int {
    val red = buffer.get().toInt() and 0xff
    val green = buffer.get().toInt() and 0xff
    val blue = buffer.get().toInt() and 0xff
    val alpha = buffer.get().toInt() and 0xff
    return (alpha shl 24) or (red shl 16) or (green shl 8) or blue
  }

  private fun readUtf8(buffer: ByteBuffer, length: Int): String? {
    if (length == 0) return ""
    val payload = ByteArray(length)
    buffer.get(payload)
    return try {
      StandardCharsets.UTF_8
        .newDecoder()
        .onMalformedInput(CodingErrorAction.REPORT)
        .onUnmappableCharacter(CodingErrorAction.REPORT)
        .decode(ByteBuffer.wrap(payload))
        .toString()
    } catch (_: CharacterCodingException) {
      null
    }
  }
}
