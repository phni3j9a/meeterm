package dev.meeterm.terminal

/**
 * One placement in the fixed-size glyph atlas.
 *
 * [reset] tells the renderer that the previous atlas contents have been
 * discarded before this placement. The generation changes at the same
 * boundary so tests can identify the cache-invalidating reset.
 */
internal data class GlyphAtlasPlacement(
  val x: Int,
  val y: Int,
  val generation: Int,
  val reset: Boolean,
)

/**
 * Bounded row packing for the renderer's atlas.
 *
 * The terminal renderer submits each glyph immediately, so dropping all
 * placements at capacity is safe: GL command ordering preserves earlier draws
 * before the texture is reused, and later cells receive fresh placements. This
 * helper deliberately owns only packing state; texture/cache invalidation
 * remains in [TerminalRenderer]'s atlas implementation.
 */
internal class GlyphAtlasPacking(
  private val size: Int,
  private val padding: Int,
) {
  init {
    require(size > 0) { "Atlas size must be positive" }
    require(padding >= 0) { "Atlas padding must be non-negative" }
    require(size > padding * 2) { "Atlas size must leave room for a glyph" }
  }

  private var cursorX = padding
  private var cursorY = padding
  private var rowHeight = 0

  var generation: Int = 0
    private set

  val resetCount: Int
    get() = generation

  fun place(width: Int, height: Int): GlyphAtlasPlacement {
    require(width > 0 && height > 0) { "Glyph dimensions must be positive" }
    require(width + padding <= size) { "Glyph width exceeds atlas capacity" }
    require(height + padding <= size) { "Glyph height exceeds atlas capacity" }

    if (cursorX + width > size) {
      cursorX = padding
      cursorY += rowHeight + padding
      rowHeight = 0
    }
    var reset = false
    if (cursorY + height > size) {
      cursorX = padding
      cursorY = padding
      rowHeight = 0
      generation += 1
      reset = true
    }

    val placement = GlyphAtlasPlacement(cursorX, cursorY, generation, reset)
    cursorX += width + padding
    rowHeight = maxOf(rowHeight, height)
    return placement
  }
}
