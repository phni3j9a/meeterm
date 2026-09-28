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
  fun newSurfaceGenerationReArmsAndRevealsOnItsFrame() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(1)
    assertFalse(gate.covered)

    gate.arm(2)
    assertTrue(gate.covered)
    gate.onValidFrame(2)
    assertFalse(gate.covered)
  }

  @Test
  fun staleGenerationFrameCannotReveal() {
    val gate = SurfaceCoverGate()
    gate.arm(2)
    gate.onValidFrame(1)
    assertTrue(gate.covered)
    gate.onValidFrame(2)
    assertFalse(gate.covered)
  }

  @Test
  fun destroyArmRequiresStrictlyNewerGeneration() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(1)
    assertFalse(gate.covered)

    // A destroyed surface's next valid frame belongs to a later generation.
    gate.arm(2)
    gate.onValidFrame(1)
    assertTrue(gate.covered)
    gate.onValidFrame(2)
    assertFalse(gate.covered)
  }

  @Test
  fun laterValidFramesNeverReArm() {
    val gate = SurfaceCoverGate()
    gate.onValidFrame(1)
    gate.onValidFrame(1)
    gate.onValidFrame(2)
    assertFalse(gate.covered)
  }

  @Test
  fun skippedGenerationStillReveals() {
    val gate = SurfaceCoverGate()
    gate.arm(1)
    gate.arm(3)
    gate.onValidFrame(3)
    assertFalse(gate.covered)
  }
}
