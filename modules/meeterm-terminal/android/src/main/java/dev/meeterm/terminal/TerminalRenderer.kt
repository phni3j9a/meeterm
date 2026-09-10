package dev.meeterm.terminal

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Typeface
import android.opengl.GLES20
import android.opengl.GLSurfaceView
import android.opengl.GLUtils
import android.util.Log
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import javax.microedition.khronos.egl.EGLConfig
import javax.microedition.khronos.opengles.GL10
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

internal data class RendererMetrics(
  val cellWidthPx: Int,
  val cellHeightPx: Int,
  val columns: Int,
  val rows: Int,
)

private data class FontMetrics(
  val sizeSp: Float,
  val cellWidthPx: Int,
  val cellHeightPx: Int,
  val generation: Long,
)

/**
 * GLES 2 renderer for the native-only MTRM snapshot.
 *
 * Rust owns the terminal and the snapshot is fetched directly from JNI on the
 * GL thread. No terminal bytes, cells, or frames are sent through JS.
 */
internal class TerminalRenderer(context: Context) : GLSurfaceView.Renderer {
  private val appContext = context.applicationContext
  private val density = appContext.resources.displayMetrics.density
  private val typeface = loadTypeface()
  private val fontPaint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG).apply {
    this.typeface = this@TerminalRenderer.typeface
    textSize = FONT_SIZE_SP * density
    color = Color.WHITE
  }
  @Volatile private var fontMetrics = FontMetrics(
    sizeSp = FONT_SIZE_SP,
    cellWidthPx = max(1, ceil(fontPaint.measureText("M")).toInt()),
    cellHeightPx = max(
      1,
      ceil(fontPaint.fontMetrics.descent - fontPaint.fontMetrics.ascent + density * 2f).toInt(),
    ),
    generation = 0L,
  )
  private var appliedFontGeneration = -1L

  private val positionBuffer = directFloatBuffer(8)
  private val textureBuffer = directFloatBuffer(8)
  private var solidProgram = 0
  private var textureProgram = 0
  @Volatile private var surfaceWidth = 0
  @Volatile private var surfaceHeight = 0
  @Volatile private var terminalHandle = 0L
  @Volatile private var preedit = ""
  @Volatile private var lightTheme = false
  private var atlas: GlyphAtlas? = null
  @Volatile private var latestMetrics = RendererMetrics(
    fontMetrics.cellWidthPx,
    fontMetrics.cellHeightPx,
    0,
    0,
  )
  private var loggedSnapshot = false
  private var loggedFirstFrame = false
  private var loggedAtlasResetCount = 0

  val cellWidthPx: Int
    get() = fontMetrics.cellWidthPx

  val cellHeightPx: Int
    get() = fontMetrics.cellHeightPx

  val metrics: RendererMetrics
    get() = latestMetrics

  fun setFontSize(value: Double) {
    val normalized = (if (value.isFinite()) value else FONT_SIZE_SP.toDouble())
      .toFloat()
      .coerceIn(10f, 24f)
    val current = fontMetrics
    if (normalized == current.sizeSp) return
    fontPaint.textSize = normalized * density
    val width = max(1, ceil(fontPaint.measureText("M")).toInt())
    val height = max(
      1,
      ceil(fontPaint.fontMetrics.descent - fontPaint.fontMetrics.ascent + density * 2f).toInt(),
    )
    fontMetrics = FontMetrics(normalized, width, height, current.generation + 1)
    latestMetrics = latestMetrics.copy(cellWidthPx = width, cellHeightPx = height)
  }

  fun setTheme(light: Boolean) {
    lightTheme = light
  }

  fun attachTerminal(handle: Long) {
    terminalHandle = handle
  }

  fun setPreedit(value: String) {
    preedit = value
  }

  fun updateSurfaceSize(width: Int, height: Int) {
    surfaceWidth = width
    surfaceHeight = height
    val metrics = fontMetrics
    latestMetrics = latestMetrics.copy(
      cellWidthPx = metrics.cellWidthPx,
      cellHeightPx = metrics.cellHeightPx,
      columns = width / metrics.cellWidthPx,
      rows = height / metrics.cellHeightPx,
    )
  }

  override fun onSurfaceCreated(gl: GL10?, config: EGLConfig?) {
    solidProgram = createProgram(SOLID_VERTEX_SHADER, SOLID_FRAGMENT_SHADER)
    textureProgram = createProgram(TEXTURE_VERTEX_SHADER, TEXTURE_FRAGMENT_SHADER)
    val metrics = fontMetrics
    atlas = GlyphAtlas(
      typeface,
      metrics.cellWidthPx,
      metrics.cellHeightPx,
      metrics.sizeSp * density,
    ).also { it.createTexture() }
    appliedFontGeneration = metrics.generation
    loggedAtlasResetCount = 0
    setClearColor()
    GLES20.glDisable(GLES20.GL_DEPTH_TEST)
    GLES20.glEnable(GLES20.GL_BLEND)
    GLES20.glBlendFunc(GLES20.GL_SRC_ALPHA, GLES20.GL_ONE_MINUS_SRC_ALPHA)
    Log.i(
      TAG,
      "GLES surface created cell=${metrics.cellWidthPx}x${metrics.cellHeightPx}",
    )
  }

  override fun onSurfaceChanged(gl: GL10?, width: Int, height: Int) {
    surfaceWidth = width
    surfaceHeight = height
    GLES20.glViewport(0, 0, width, height)
  }

  override fun onDrawFrame(gl: GL10?) {
    val metrics = fontMetrics
    if (appliedFontGeneration != metrics.generation || atlas == null) {
      val replacement = GlyphAtlas(
        typeface,
        metrics.cellWidthPx,
        metrics.cellHeightPx,
        metrics.sizeSp * density,
      ).also { it.createTexture() }
      atlas?.deleteTexture()
      atlas = replacement
      appliedFontGeneration = metrics.generation
      loggedAtlasResetCount = 0
      Log.i(
        TAG,
        "font metrics updated cell=${metrics.cellWidthPx}x${metrics.cellHeightPx} " +
          "size=${metrics.sizeSp}sp",
      )
    }
    setClearColor()
    GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)

    val handle = terminalHandle
    if (handle == 0L) return

    val nativeBytes = MeetermNative.snapshot(handle) ?: return
    val snapshot = TerminalSnapshotParser.parse(nativeBytes) ?: run {
      Log.w(TAG, "Ignoring malformed native terminal snapshot")
      return
    }
    if (!loggedSnapshot) {
      val visibleGlyphs = snapshot.cells.count { !it.isHidden && it.text.isNotBlank() }
      Log.i(
        TAG,
        "snapshot parsed columns=${snapshot.columns} rows=${snapshot.rows} " +
          "cells=${snapshot.cells.size} visibleGlyphs=$visibleGlyphs bytes=${nativeBytes.size}",
      )
      loggedSnapshot = true
    }
    latestMetrics = latestMetrics.copy(
      columns = snapshot.columns,
      rows = snapshot.rows,
    )

    // Backgrounds are a separate pass so every cell's ANSI background remains
    // visible even when its glyph atlas entry is empty.
    snapshot.cells.forEach { cell ->
      val foreground = if (cell.flags and FLAG_INVERSE != 0) cell.background else cell.foreground
      val background = if (cell.flags and FLAG_INVERSE != 0) cell.foreground else cell.background
      drawSolid(cellLeft(cell.column, snapshot.columns), cellTop(cell.row, snapshot.rows),
        cellRight(cell.column, cell.width, snapshot.columns), cellBottom(cell.row, snapshot.rows),
        background)
      if (!cell.isHidden && cell.text.isNotBlank()) {
        drawGlyph(
          text = cell.text,
          column = cell.column,
          row = cell.row,
          width = cell.width,
          columns = snapshot.columns,
          rows = snapshot.rows,
          color = foreground,
          bold = cell.isBold,
        )
      }
      if (!cell.isHidden && cell.isUnderlined) {
        drawUnderline(cell, snapshot.columns, snapshot.rows, foreground)
      }
    }

    drawCursor(snapshot)
    drawPreedit(snapshot)

    val atlasResetCount = atlas?.resetCount ?: 0
    if (atlasResetCount > loggedAtlasResetCount) {
      Log.i(TAG, "MEETERM_GLYPH_ATLAS_RESET count=$atlasResetCount")
      loggedAtlasResetCount = atlasResetCount
    }

    if (!loggedFirstFrame) {
      Log.i(TAG, "MEETERM_SMOKE_FIRST_FRAME")
      loggedFirstFrame = true
    }
  }

  private fun drawGlyph(
    text: String,
    column: Int,
    row: Int,
    width: Int,
    columns: Int,
    rows: Int,
    color: Int,
    bold: Boolean,
  ) {
    val glyph = atlas?.glyph(text, bold) ?: return
    val left = cellLeft(column, columns)
    val right = cellRight(column, width, columns)
    val top = cellTop(row, rows)
    val bottom = cellBottom(row, rows)
    val position = positionBuffer
    position.clear()
    position.put(floatArrayOf(left, top, right, top, left, bottom, right, bottom)).flip()
    val texture = textureBuffer
    texture.clear()
    texture.put(floatArrayOf(glyph.u0, glyph.v0, glyph.u1, glyph.v0, glyph.u0, glyph.v1, glyph.u1, glyph.v1)).flip()

    GLES20.glUseProgram(textureProgram)
    val positionLocation = GLES20.glGetAttribLocation(textureProgram, "aPosition")
    val textureLocation = GLES20.glGetAttribLocation(textureProgram, "aTexCoord")
    val colorLocation = GLES20.glGetUniformLocation(textureProgram, "uColor")
    GLES20.glEnableVertexAttribArray(positionLocation)
    GLES20.glEnableVertexAttribArray(textureLocation)
    GLES20.glVertexAttribPointer(positionLocation, 2, GLES20.GL_FLOAT, false, 0, position)
    GLES20.glVertexAttribPointer(textureLocation, 2, GLES20.GL_FLOAT, false, 0, texture)
    putColor(colorLocation, color)
    GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
    GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, atlas?.textureId ?: 0)
    GLES20.glUniform1i(GLES20.glGetUniformLocation(textureProgram, "uTexture"), 0)
    GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
    GLES20.glDisableVertexAttribArray(positionLocation)
    GLES20.glDisableVertexAttribArray(textureLocation)
  }

  private fun drawSolid(left: Float, top: Float, right: Float, bottom: Float, color: Int) {
    positionBuffer.clear()
    positionBuffer.put(floatArrayOf(left, top, right, top, left, bottom, right, bottom)).flip()

    GLES20.glUseProgram(solidProgram)
    val positionLocation = GLES20.glGetAttribLocation(solidProgram, "aPosition")
    val colorLocation = GLES20.glGetUniformLocation(solidProgram, "uColor")
    GLES20.glEnableVertexAttribArray(positionLocation)
    GLES20.glVertexAttribPointer(positionLocation, 2, GLES20.GL_FLOAT, false, 0, positionBuffer)
    putColor(colorLocation, color)
    GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
    GLES20.glDisableVertexAttribArray(positionLocation)
  }

  private fun drawUnderline(cell: TerminalCell, columns: Int, rows: Int, color: Int) {
    val lineHeight = max(0.006f, 2f / max(1, surfaceHeight).toFloat())
    val bottom = cellBottom(cell.row, rows)
    drawSolid(
      cellLeft(cell.column, columns),
      bottom - lineHeight * 2f,
      cellRight(cell.column, cell.width, columns),
      bottom,
      color,
    )
  }

  private fun drawCursor(snapshot: TerminalSnapshot) {
    val row = snapshot.cursorRow
    val column = snapshot.cursorColumn
    if (row !in 0 until snapshot.rows || column !in 0 until snapshot.columns) return

    val lineWidth = max(0.004f, 2f / max(1, surfaceWidth).toFloat())
    val lineHeight = max(0.006f, 2f / max(1, surfaceHeight).toFloat())
    val left = cellLeft(column, snapshot.columns)
    val right = cellRight(column, 1, snapshot.columns)
    val top = cellTop(row, snapshot.rows)
    val bottom = cellBottom(row, snapshot.rows)
    val cursorColor = Color.argb(220, 232, 238, 246)
    drawSolid(left, top, min(right, left + lineWidth), bottom, cursorColor)
    drawSolid(max(left, right - lineWidth), top, right, bottom, cursorColor)
    drawSolid(left, top, right, min(bottom, top + lineHeight), cursorColor)
    drawSolid(left, max(top, bottom - lineHeight), right, bottom, cursorColor)
  }

  private fun drawPreedit(snapshot: TerminalSnapshot) {
    if (preedit.isEmpty()) return
    var column = snapshot.cursorColumn.coerceIn(0, snapshot.columns - 1)
    val row = snapshot.cursorRow
    if (row !in 0 until snapshot.rows) return

    var index = 0
    while (index < preedit.length && column < snapshot.columns) {
      val codePoint = preedit.codePointAt(index)
      val count = Character.charCount(codePoint)
      val text = preedit.substring(index, index + count)
      val width = preeditWidth(codePoint).coerceAtMost(snapshot.columns - column)
      drawGlyph(
        text = text,
        column = column,
        row = row,
        width = width,
        columns = snapshot.columns,
        rows = snapshot.rows,
        color = Color.rgb(255, 201, 92),
        bold = false,
      )
      val underline = TerminalCell(row, column, width, FLAG_UNDERLINE, Color.rgb(255, 201, 92), Color.TRANSPARENT, text, "")
      drawUnderline(underline, snapshot.columns, snapshot.rows, Color.rgb(255, 201, 92))
      column += width
      index += count
    }
  }

  private fun putColor(location: Int, color: Int) {
    GLES20.glUniform4f(
      location,
      Color.red(color) / 255f,
      Color.green(color) / 255f,
      Color.blue(color) / 255f,
      Color.alpha(color) / 255f,
    )
  }

  private fun cellLeft(column: Int, columns: Int): Float = -1f + 2f * column / max(1, columns)

  private fun cellRight(column: Int, width: Int, columns: Int): Float =
    -1f + 2f * min(columns, column + width) / max(1, columns)

  private fun cellTop(row: Int, rows: Int): Float = 1f - 2f * row / max(1, rows)

  private fun cellBottom(row: Int, rows: Int): Float = 1f - 2f * (row + 1) / max(1, rows)

  private fun loadTypeface(): Typeface {
    return try {
      Typeface.createFromAsset(appContext.assets, FONT_ASSET)
    } catch (error: RuntimeException) {
      Log.w(TAG, "Bundled font unavailable; falling back to platform monospace", error)
      Typeface.MONOSPACE
    }
  }

  private fun setClearColor() {
    val background = if (lightTheme) Color.rgb(251, 247, 239) else Color.rgb(36, 33, 29)
    GLES20.glClearColor(
      Color.red(background) / 255f,
      Color.green(background) / 255f,
      Color.blue(background) / 255f,
      1f,
    )
  }

  private fun preeditWidth(codePoint: Int): Int {
    return when {
      codePoint in 0x1100..0x11ff ||
        codePoint in 0x2e80..0xa4cf ||
        codePoint in 0xac00..0xd7a3 ||
        codePoint in 0xf900..0xfaff ||
        codePoint in 0xfe10..0xfe6f ||
        codePoint in 0xff01..0xff60 ||
        codePoint > 0x1f000 -> 2
      else -> 1
    }
  }

  private fun directFloatBuffer(size: Int): FloatBuffer = ByteBuffer
    .allocateDirect(size * Float.SIZE_BYTES)
    .order(ByteOrder.nativeOrder())
    .asFloatBuffer()

  private fun createProgram(vertexSource: String, fragmentSource: String): Int {
    val vertex = compileShader(GLES20.GL_VERTEX_SHADER, vertexSource)
    val fragment = compileShader(GLES20.GL_FRAGMENT_SHADER, fragmentSource)
    val program = GLES20.glCreateProgram()
    check(program != 0) { "Could not create GLES program" }
    GLES20.glAttachShader(program, vertex)
    GLES20.glAttachShader(program, fragment)
    GLES20.glLinkProgram(program)
    val status = IntArray(1)
    GLES20.glGetProgramiv(program, GLES20.GL_LINK_STATUS, status, 0)
    check(status[0] != 0) { "Could not link GLES program: ${GLES20.glGetProgramInfoLog(program)}" }
    GLES20.glDeleteShader(vertex)
    GLES20.glDeleteShader(fragment)
    return program
  }

  private fun compileShader(type: Int, source: String): Int {
    val shader = GLES20.glCreateShader(type)
    check(shader != 0) { "Could not create GLES shader" }
    GLES20.glShaderSource(shader, source)
    GLES20.glCompileShader(shader)
    val status = IntArray(1)
    GLES20.glGetShaderiv(shader, GLES20.GL_COMPILE_STATUS, status, 0)
    check(status[0] != 0) { "Could not compile GLES shader: ${GLES20.glGetShaderInfoLog(shader)}" }
    return shader
  }

  private class GlyphAtlas(
    typeface: Typeface,
    private val cellWidth: Int,
    private val cellHeight: Int,
    private val textSize: Float,
  ) {
    private val bitmap = Bitmap.createBitmap(ATLAS_SIZE, ATLAS_SIZE, Bitmap.Config.ARGB_8888)
    private val canvas = Canvas(bitmap)
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG).apply {
      this.typeface = typeface
      textSize = this@GlyphAtlas.textSize
      color = Color.WHITE
      isSubpixelText = true
    }
    private val entries = HashMap<String, Glyph>()
    private val packing = GlyphAtlasPacking(ATLAS_SIZE, ATLAS_PADDING)
    private val uploadPixels = IntArray(GLYPH_MAX_SIZE * GLYPH_MAX_SIZE)
    private val uploadBuffer = ByteBuffer.allocateDirect(
      GLYPH_MAX_SIZE * GLYPH_MAX_SIZE * Int.SIZE_BYTES,
    )
    var textureId: Int = 0
      private set

    val resetCount: Int
      get() = packing.resetCount

    fun createTexture() {
      val texture = IntArray(1)
      GLES20.glGenTextures(1, texture, 0)
      textureId = texture[0]
      GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, textureId)
      GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
      GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
      GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
      GLES20.glTexParameteri(GLES20.GL_TEXTURE_2D, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
      bitmap.eraseColor(Color.TRANSPARENT)
      uploadBitmap()
    }

    fun deleteTexture() {
      if (textureId == 0) return
      GLES20.glDeleteTextures(1, intArrayOf(textureId), 0)
      textureId = 0
    }

    fun glyph(text: String, bold: Boolean): Glyph? {
      val key = "$bold\u0000$text"
      entries[key]?.let { return it }
      val measuredWidth = ceil(paint.measureText(text)).toInt()
      val glyphWidth = (measuredWidth + ATLAS_PADDING * 2)
        .coerceIn(cellWidth + ATLAS_PADDING * 2, cellWidth * 2 + ATLAS_PADDING * 2)
        .coerceAtMost(GLYPH_MAX_SIZE)
      val glyphHeight = (cellHeight + ATLAS_PADDING * 2).coerceAtMost(GLYPH_MAX_SIZE)
      val placement = packing.place(glyphWidth, glyphHeight)
      if (placement.reset) {
        entries.clear()
        bitmap.eraseColor(Color.TRANSPARENT)
        uploadBitmap()
      }

      paint.isFakeBoldText = bold
      val metrics = paint.fontMetrics
      val baseline = placement.y + ATLAS_PADDING - metrics.ascent
      canvas.drawText(text, placement.x + ATLAS_PADDING.toFloat(), baseline, paint)
      val result = Glyph(
        u0 = placement.x.toFloat() / ATLAS_SIZE,
        v0 = placement.y.toFloat() / ATLAS_SIZE,
        u1 = (placement.x + glyphWidth).toFloat() / ATLAS_SIZE,
        v1 = (placement.y + glyphHeight).toFloat() / ATLAS_SIZE,
      )
      entries[key] = result
      uploadRegion(placement.x, placement.y, glyphWidth, glyphHeight)
      return result
    }

    private fun uploadBitmap() {
      GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, textureId)
      GLUtils.texImage2D(GLES20.GL_TEXTURE_2D, 0, bitmap, 0)
    }

    private fun uploadRegion(x: Int, y: Int, width: Int, height: Int) {
      bitmap.getPixels(uploadPixels, 0, width, x, y, width, height)
      uploadBuffer.clear()
      repeat(width * height) { index ->
        val argb = uploadPixels[index]
        uploadBuffer.put((argb ushr 16).toByte())
        uploadBuffer.put((argb ushr 8).toByte())
        uploadBuffer.put(argb.toByte())
        uploadBuffer.put((argb ushr 24).toByte())
      }
      uploadBuffer.flip()
      GLES20.glBindTexture(GLES20.GL_TEXTURE_2D, textureId)
      GLES20.glTexSubImage2D(
        GLES20.GL_TEXTURE_2D,
        0,
        x,
        y,
        width,
        height,
        GLES20.GL_RGBA,
        GLES20.GL_UNSIGNED_BYTE,
        uploadBuffer,
      )
    }
  }

  private data class Glyph(
    val u0: Float,
    val v0: Float,
    val u1: Float,
    val v1: Float,
  )

  private companion object {
    const val TAG = "MeetermRenderer"
    const val FONT_ASSET = "fonts/MPLUS1Code[wght].ttf"
    const val FONT_SIZE_SP = 15f
    const val ATLAS_SIZE = 1024
    const val ATLAS_PADDING = 2
    const val GLYPH_MAX_SIZE = 256

    const val SOLID_VERTEX_SHADER = """
      attribute vec2 aPosition;
      void main() {
        gl_Position = vec4(aPosition, 0.0, 1.0);
      }
    """

    const val SOLID_FRAGMENT_SHADER = """
      precision mediump float;
      uniform vec4 uColor;
      void main() {
        gl_FragColor = uColor;
      }
    """

    const val TEXTURE_VERTEX_SHADER = """
      attribute vec2 aPosition;
      attribute vec2 aTexCoord;
      varying vec2 vTexCoord;
      void main() {
        gl_Position = vec4(aPosition, 0.0, 1.0);
        vTexCoord = aTexCoord;
      }
    """

    const val TEXTURE_FRAGMENT_SHADER = """
      precision mediump float;
      uniform sampler2D uTexture;
      uniform vec4 uColor;
      varying vec2 vTexCoord;
      void main() {
        float alpha = texture2D(uTexture, vTexCoord).a;
        gl_FragColor = vec4(uColor.rgb, uColor.a * alpha);
      }
    """
  }
}
