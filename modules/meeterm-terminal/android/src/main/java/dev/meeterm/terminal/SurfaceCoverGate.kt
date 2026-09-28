package dev.meeterm.terminal

/**
 * UI-thread gate for the themed cover over the terminal surface.
 *
 * A new, destroyed or resized GL surface has no presented buffer at the new
 * state, so the terminal rectangle is composited black until the renderer
 * swaps a frame. Arming records the renderer's last valid frame sequence and
 * the cover drops only on a strictly newer valid frame, so a stale reveal
 * queued before an arm cannot uncover the gap, and theme-only updates or
 * remote content changes never re-raise it.
 */
internal class SurfaceCoverGate {
  private var revealAfterFrame = -1L

  // Read by the GL thread to skip reveal posts in steady state; mutated only
  // on the UI thread.
  @Volatile var covered = true
    private set

  fun arm(currentFrameSeq: Long) {
    revealAfterFrame = maxOf(revealAfterFrame, currentFrameSeq)
    covered = true
  }

  fun onValidFrame(frameSeq: Long) {
    if (frameSeq > revealAfterFrame) covered = false
  }
}
