package dev.meeterm.terminal

/**
 * UI-thread gate for the themed cover over the terminal surface.
 *
 * A new GL surface has no presented buffer, so the terminal rectangle is
 * composited black until the renderer swaps a frame. The cover stays raised
 * until a valid snapshot frame for the armed generation (or newer) exists;
 * theme-only updates and remote content changes never re-raise it.
 */
internal class SurfaceCoverGate {
  private var armedGeneration = -1L

  var covered = true
    private set

  fun arm(generation: Long) {
    armedGeneration = maxOf(armedGeneration, generation)
    covered = true
  }

  fun onValidFrame(generation: Long) {
    if (generation >= armedGeneration) covered = false
  }
}
