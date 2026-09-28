package dev.meeterm.terminal

/**
 * UI-thread gate for the themed cover over the terminal surface.
 *
 * A created, destroyed or resized holder surface has no presented buffer at
 * the new state, so the terminal rectangle is composited black until the
 * renderer swaps a frame belonging to that surface lifetime. The gate keys
 * the cover on the SurfaceHolder lifetime, which is independent of EGL
 * context creation: with `setPreserveEGLContextOnPause(true)` a recreated
 * surface need not produce Renderer.onSurfaceCreated.
 *
 * The renderer stamps each valid frame with the lifetime current when the
 * frame STARTED (before the snapshot is fetched), so a frame begun on a
 * previous surface cannot be mistaken for a new-lifetime completion, and a
 * stale reveal queued before an arm cannot uncover a later gap. Theme-only
 * updates and remote content changes are not holder events and never
 * re-raise the cover.
 */
internal class SurfaceCoverGate {
  /**
   * Bumped on the UI thread for every holder surface create/change/destroy.
   * Read by the GL thread at frame start.
   */
  @Volatile var currentLifetime = 0L
    private set

  private var armedLifetime = 0L
  private var armedHasSurface = true
  private var revealedLifetime = -1L

  // Read by the GL thread to skip reveal posts in steady state; UI-mutated only.
  @Volatile var covered = true
    private set

  /** New holder surface: the first valid frame of this lifetime reveals. */
  fun surfaceCreated() = arm(hasSurface = true)

  /** Same surface resized: earlier buffers no longer cover the new bounds. */
  fun surfaceChanged() = arm(hasSurface = true)

  /** No surface exists; nothing may reveal until the next armed lifetime. */
  fun surfaceDestroyed() = arm(hasSurface = false)

  private fun arm(hasSurface: Boolean) {
    currentLifetime += 1
    if (currentLifetime <= revealedLifetime) return
    armedLifetime = currentLifetime
    armedHasSurface = hasSurface
    covered = true
  }

  /**
   * UI-thread completion for a valid snapshot frame. [lifetime] is the value
   * captured when that frame started, not the latest lifetime.
   */
  fun onValidFrame(lifetime: Long) {
    if (covered && armedHasSurface && lifetime == armedLifetime) {
      revealedLifetime = lifetime
      covered = false
    }
  }
}
