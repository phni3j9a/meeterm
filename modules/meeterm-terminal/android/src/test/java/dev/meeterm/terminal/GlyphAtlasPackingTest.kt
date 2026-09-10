package dev.meeterm.terminal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GlyphAtlasPackingTest {
  @Test
  fun manyDistinctGlyphsResetAtlasAndKeepAllocating() {
    val atlas = GlyphAtlasPacking(size = 32, padding = 1)
    val placements = buildList {
      repeat(10) { screen ->
        repeat(20) { glyph ->
          add(atlas.place(width = 7 + (screen + glyph) % 3, height = 6 + glyph % 2))
        }
      }
    }

    val first = placements.first()
    val firstReset = placements.first { it.reset }
    val last = placements.last()

    assertEquals(0, first.generation)
    assertEquals(1, firstReset.x)
    assertEquals(1, firstReset.y)
    assertTrue(firstReset.generation > first.generation)
    assertTrue(atlas.resetCount > 1)
    assertEquals(atlas.generation, last.generation)
    assertTrue(last.x >= 1)
    assertTrue(last.y >= 1)
    assertTrue(last.x + 9 <= 32)
    assertTrue(last.y + 7 <= 32)
  }
}
