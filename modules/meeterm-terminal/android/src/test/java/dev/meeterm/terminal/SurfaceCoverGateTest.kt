package dev.meeterm.terminal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Exercises the gate through the same calls the production view and renderer
 * make: holder callbacks bump the lifetime and arm; the renderer stamps each
 * valid frame with the lifetime read when the frame started and reports it
 * through onValidFrame on the UI thread.
 */
class SurfaceCoverGateTest {
  @Test
  fun firstValidFrameOfLifetimeReveals() {
    val gate = SurfaceCoverGate({}, {})
    assertTrue(gate.covered)
    gate.surfaceCreated()
    val stampedAtStart = gate.currentLifetime
    gate.onValidFrame(stampedAtStart)
    assertFalse(gate.covered)
  }

  @Test
  fun recreateWithoutContextRecreatedStillReveals() {
    val gate = SurfaceCoverGate({}, {})
    // holder create -> EGL context+surface create once -> valid frame reveals
    gate.surfaceCreated()
    gate.onValidFrame(gate.currentLifetime)
    assertFalse(gate.covered)

    // holder destroy with the GL thread drained first (GLSurfaceView's own
    // callback ordering), then a recreate where the preserved context means
    // Renderer.onSurfaceCreated never runs again.
    gate.surfaceDestroyed()
    assertTrue(gate.covered)
    gate.surfaceCreated()
    assertTrue(gate.covered)
    gate.onValidFrame(gate.currentLifetime)
    assertFalse(gate.covered)
  }

  @Test
  fun frameStartedBeforeDestroyCannotRevealAfterRecreate() {
    val gate = SurfaceCoverGate({}, {})
    gate.surfaceCreated()
    // Renderer read the lifetime when this frame STARTED, before destroy.
    val oldSurfaceStamp = gate.currentLifetime
    gate.surfaceDestroyed()
    gate.surfaceCreated()
    // The completion reaches the UI thread only after the new lifetime armed.
    gate.onValidFrame(oldSurfaceStamp)
    assertTrue(gate.covered)
    gate.onValidFrame(gate.currentLifetime)
    assertFalse(gate.covered)
  }

  @Test
  fun noSurfaceNeverReveals() {
    val gate = SurfaceCoverGate({}, {})
    gate.surfaceCreated()
    gate.onValidFrame(gate.currentLifetime)
    gate.surfaceDestroyed()
    // No draw may claim the destroyed lifetime, not even one stamped with it.
    gate.onValidFrame(gate.currentLifetime)
    assertTrue(gate.covered)
  }

  @Test
  fun resizeArmsUntilFirstFrameOfThatLifetime() {
    val gate = SurfaceCoverGate({}, {})
    gate.surfaceCreated()
    gate.onValidFrame(gate.currentLifetime)
    assertFalse(gate.covered)

    gate.surfaceChanged()
    assertTrue(gate.covered)
    // A frame begun before the change carries the prior lifetime and is stale.
    gate.onValidFrame(gate.currentLifetime - 1)
    assertTrue(gate.covered)
    gate.onValidFrame(gate.currentLifetime)
    assertFalse(gate.covered)
  }

  @Test
  fun healthyContentFramesNeverReArm() {
    val gate = SurfaceCoverGate({}, {})
    gate.surfaceCreated()
    gate.onValidFrame(gate.currentLifetime)
    gate.onValidFrame(gate.currentLifetime)
    gate.onValidFrame(gate.currentLifetime)
    assertFalse(gate.covered)
  }

  @Test
  fun holderEventsRequestANewLifetimeFrameAfterShowingTheCover() {
    val events = mutableListOf<String>()
    val requestedFrames = mutableListOf<Long>()
    lateinit var gate: SurfaceCoverGate
    gate = SurfaceCoverGate(
      showCover = {
        assertTrue(gate.covered)
        events.add("cover:${gate.currentLifetime}")
      },
      requestRender = {
        assertTrue(gate.covered)
        events.add("render:${gate.currentLifetime}")
        requestedFrames.add(gate.currentLifetime)
      },
    )

    for (holderEvent in listOf(gate::surfaceCreated, gate::surfaceChanged)) {
      val inFlightFrame = gate.currentLifetime
      events.clear()
      requestedFrames.clear()
      holderEvent()
      val newLifetime = gate.currentLifetime
      assertTrue(newLifetime > inFlightFrame)
      assertEquals(listOf("cover:$newLifetime", "render:$newLifetime"), events)
      assertEquals(listOf(newLifetime), requestedFrames)
      gate.onValidFrame(inFlightFrame)
      assertTrue(gate.covered)
      gate.onValidFrame(requestedFrames.single())
      assertFalse(gate.covered)
      assertEquals(2, events.size)
    }

    events.clear()
    gate.surfaceDestroyed()
    assertEquals(listOf("cover:${gate.currentLifetime}"), events)
    requestedFrames.clear()
    gate.surfaceCreated()
    assertEquals(listOf(gate.currentLifetime), requestedFrames)
    gate.onValidFrame(requestedFrames.single())
    assertFalse(gate.covered)
  }
}
