package dev.meeterm.terminal

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SurfaceCoverGateTest {
  @Test
  fun coveredUntilFirstValidFrame() {
    val gate = SurfaceCoverGate()
    assertTrue(gate.covered)
    gate.onValidFrame(1)
    assertFalse(gate.covered)
  }

  @Test
  fun armRequiresStrictlyNewerFrame() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(1)
    assertFalse(gate.covered)

    // A surface lifecycle event at frame 1: the frame that already presented
    // must not reveal the cover; the next valid frame does.
    gate.arm(1)
    assertTrue(gate.covered)
    gate.onValidFrame(1)
    assertTrue(gate.covered)
    gate.onValidFrame(2)
    assertFalse(gate.covered)
  }

  @Test
  fun staleRevealQueuedBeforeArmCannotUncover() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(5)
    gate.arm(6)
    gate.onValidFrame(5)
    assertTrue(gate.covered)
    gate.onValidFrame(6)
    assertTrue(gate.covered)
    gate.onValidFrame(7)
    assertFalse(gate.covered)
  }

  @Test
  fun destroyedSurfaceStaysCoveredUntilNextSurfaceFrame() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(3)
    gate.arm(3)
    assertTrue(gate.covered)
    // A destroyed surface draws nothing; only a later frame can reveal.
    gate.onValidFrame(4)
    assertFalse(gate.covered)
  }

  @Test
  fun laterValidFramesNeverReArm() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(1)
    gate.onValidFrame(2)
    gate.onValidFrame(3)
    assertFalse(gate.covered)
  }

  @Test
  fun reArmMonotonicAcrossRapidEvents() {
    val gate = SurfaceCoverGate()
    gate.arm(2)
    gate.arm(5)
    gate.onValidFrame(4)
    assertTrue(gate.covered)
    gate.onValidFrame(6)
    assertFalse(gate.covered)
  }
}
