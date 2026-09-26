package dev.meeterm.terminal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-JVM coverage of the operation model: transition guards
 * (double upload, stale ids, cancel semantics), the `flags & 0x2`
 * remote-removal representation, and the JNI `attachmentSnapshot`
 * flat string array codec.
 */
class AttachmentOperationTest {
  private fun fields(
    attachmentId: Long = 42L,
    phase: Int = 1,
    flags: Int = 0,
    bytesUploaded: Long = 0L,
    sizeBytes: Long = 1_000L,
    remotePath: String = "",
    displayName: String = "meeterm-x.png",
    errorCode: String = "",
    errorMessage: String = "",
  ): Array<String> = arrayOf(
    phase.toString(),
    flags.toString(),
    attachmentId.toString(),
    bytesUploaded.toString(),
    sizeBytes.toString(),
    remotePath,
    displayName,
    errorCode,
    errorMessage,
  )

  @Test
  fun decodesCompleteSnapshotFields() {
    val decoded = AttachmentOperationCodec.decode(
      fields(
        attachmentId = 42L,
        phase = 2,
        bytesUploaded = 1_000L,
        remotePath = "/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png",
      ),
    )!!
    assertEquals(42L, decoded.attachmentId)
    assertEquals(AttachmentOpPhase.UPLOADED, decoded.phase)
    assertEquals("uploaded", decoded.wirePhase)
    assertEquals(1_000L, decoded.bytesUploaded)
    assertEquals("/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png", decoded.remotePath)
    assertFalse(decoded.insertUnconfirmed)
    assertFalse(decoded.remoteRemoved)
  }

  @Test
  fun decodesFlagsAndErrorFields() {
    val decoded = AttachmentOperationCodec.decode(
      fields(phase = 3, flags = 3, errorCode = "input_unconfirmed", errorMessage = "unconfirmed"),
    )!!
    assertEquals(AttachmentOpPhase.INSERTED, decoded.phase)
    assertTrue(decoded.insertUnconfirmed)
    assertTrue(decoded.remoteRemoved)
    assertEquals("input_unconfirmed", decoded.errorCode)
    assertEquals("unconfirmed", decoded.errorMessage)
  }

  @Test
  fun rejectsShortArraysAndUnknownPhases() {
    assertNull(AttachmentOperationCodec.decode(arrayOf("1", "0")))
    assertNull(AttachmentOperationCodec.decode(fields(phase = 99)))
    assertNull(AttachmentOperationCodec.decode(fields(phase = 6)))
    assertNull(AttachmentOperationCodec.decode(fields().also { it[0] = "pending" }))
  }

  @Test
  fun refusesSecondUploadWhileOperationIsLive() {
    val machine = AttachmentOpMachine()
    assertTrue(machine.canBeginUpload())
    assertTrue(machine.recordBegin(7L, 100L, "a.png"))
    assertFalse(machine.canBeginUpload())
    assertFalse(machine.recordBegin(8L, 100L, "b.png"))
    assertEquals(7L, machine.operation?.attachmentId)
  }

  @Test
  fun allowsNewUploadOnlyFromTerminalPhases() {
    val machine = AttachmentOpMachine()
    machine.recordBegin(7L, 100L, "a.png")
    for (phase in listOf(
      AttachmentOpPhase.CANCELLED, AttachmentOpPhase.FAILED,
    )) {
      machine.applySnapshot(operation(machine.operation!!.attachmentId, phase))
      assertTrue("expected re-begin after $phase", machine.canBeginUpload())
      assertTrue(machine.recordBegin(70L + phase.wireValue, 100L, "a.png"))
    }
  }

  private fun operation(id: Long, phase: AttachmentOpPhase, flags: Int = 0) = AttachmentOperation(
    attachmentId = id,
    phase = phase,
    flags = flags,
    bytesUploaded = 0L,
    sizeBytes = 100L,
    remotePath = "",
    displayName = "a.png",
    errorCode = "",
    errorMessage = "",
  )

  @Test
  fun appliesSnapshotsOnlyForTheSameAttachmentId() {
    val machine = AttachmentOpMachine()
    machine.recordBegin(7L, 100L, "a.png")
    assertFalse(machine.applySnapshot(operation(9L, AttachmentOpPhase.UPLOADED)))
    assertEquals(AttachmentOpPhase.UPLOADING, machine.operation?.phase)
    assertTrue(machine.applySnapshot(operation(7L, AttachmentOpPhase.UPLOADED)))
    assertEquals(AttachmentOpPhase.UPLOADED, machine.operation?.phase)
  }

  @Test
  fun cancelledOperationIgnoresLateUploadSnapshots() {
    val machine = AttachmentOpMachine()
    machine.recordBegin(7L, 100L, "a.png")
    machine.markCancelled()
    // Late progress after a local cancel never resurrects the upload.
    assertTrue(machine.applySnapshot(operation(7L, AttachmentOpPhase.UPLOADING)))
    assertEquals(AttachmentOpPhase.CANCELLED, machine.operation?.phase)
    // A genuinely finished upload still surfaces its terminal state.
    assertTrue(machine.applySnapshot(operation(7L, AttachmentOpPhase.UPLOADED)))
    assertEquals(AttachmentOpPhase.UPLOADED, machine.operation?.phase)
  }

  @Test
  fun capabilityGatesFollowTheContractPhases() {
    val op = { phase: AttachmentOpPhase -> operation(1L, phase) }
    assertTrue(op(AttachmentOpPhase.PENDING).canRetryUpload)
    assertTrue(op(AttachmentOpPhase.FAILED).canRetryUpload)
    assertFalse(op(AttachmentOpPhase.UPLOADED).canRetryUpload)

    assertTrue(op(AttachmentOpPhase.UPLOADED).canInsert)
    assertTrue(op(AttachmentOpPhase.INSERTED).canInsert)
    assertFalse(op(AttachmentOpPhase.UPLOADING).canInsert)

    assertTrue(op(AttachmentOpPhase.PENDING).canCancel)
    assertTrue(op(AttachmentOpPhase.UPLOADING).canCancel)
    assertFalse(op(AttachmentOpPhase.UPLOADED).canCancel)

    for (phase in listOf(
      AttachmentOpPhase.UPLOADED, AttachmentOpPhase.INSERTED,
      AttachmentOpPhase.FAILED, AttachmentOpPhase.CANCELLED,
    )) {
      assertTrue("delete allowed in $phase", op(phase).canDeleteRemote)
    }
    assertFalse(op(AttachmentOpPhase.UPLOADING).canDeleteRemote)
    assertFalse(op(AttachmentOpPhase.PENDING).canDeleteRemote)
  }

  @Test
  fun remoteRemovalKeepsPhaseAndChangesCapabilities() {
    // `flags & 0x2` marks the verified remote deletion; the phase itself is
    // kept (an inserted line is not revoked, a failed op stays failed).
    val removedUploaded = operation(1L, AttachmentOpPhase.UPLOADED, flags = 0x2)
    assertTrue(removedUploaded.remoteRemoved)
    assertEquals("uploaded", removedUploaded.wirePhase)
    assertFalse(removedUploaded.canInsert)
    assertFalse(removedUploaded.canDeleteRemote)
    // The core re-uploads the same operation for uploaded+removed.
    assertTrue(removedUploaded.canRetryUpload)

    val removedInserted = operation(1L, AttachmentOpPhase.INSERTED, flags = 0x2)
    assertTrue(removedInserted.remoteRemoved)
    assertFalse(removedInserted.canInsert)
    assertFalse(removedInserted.canDeleteRemote)
    assertFalse(removedInserted.canRetryUpload)

    val removedFailed = operation(1L, AttachmentOpPhase.FAILED, flags = 0x2)
    assertTrue(removedFailed.canRetryUpload)
    assertFalse(removedFailed.canDeleteRemote)
  }

  @Test
  fun clearsOperationForDiscardAndNewPick() {
    val machine = AttachmentOpMachine()
    machine.recordBegin(7L, 100L, "a.png")
    machine.clear()
    assertNull(machine.operation)
    assertTrue(machine.canBeginUpload())
  }
}
