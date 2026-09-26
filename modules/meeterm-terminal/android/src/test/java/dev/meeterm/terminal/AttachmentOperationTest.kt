package dev.meeterm.terminal

import java.nio.ByteBuffer
import java.nio.ByteOrder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-JVM coverage of the Phase B operation model: transition guards
 * (double upload, stale ids, cancel semantics) and the fixed-size
 * `meeterm_attachment_snapshot_t` codec.
 */
class AttachmentOperationTest {
  private fun record(
    attachmentId: Long = 42L,
    phase: Int = 1,
    flags: Int = 0,
    bytesUploaded: Long = 0L,
    sizeBytes: Long = 1_000L,
    remotePath: String = "",
    displayName: String = "meeterm-x.png",
    errorCode: String = "",
    errorMessage: String = "",
  ): ByteArray {
    val bytes = ByteArray(AttachmentOperationCodec.RECORD_SIZE)
    val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
    buffer.putInt(0, phase)
    buffer.putInt(4, flags)
    buffer.putLong(8, attachmentId)
    buffer.putLong(16, bytesUploaded)
    buffer.putLong(24, sizeBytes)
    fun field(value: String, lengthOffset: Int, dataOffset: Int, capacity: Int) {
      val raw = value.toByteArray(Charsets.UTF_8).copyOf(capacity)
      val length = minOf(value.toByteArray(Charsets.UTF_8).size, capacity)
      buffer.putShort(lengthOffset, length.toShort())
      raw.copyInto(bytes, dataOffset)
    }
    field(remotePath, 32, 34, 512)
    field(displayName, 546, 548, 128)
    field(errorCode, 676, 678, 64)
    field(errorMessage, 742, 744, 256)
    return bytes
  }

  @Test
  fun decodesCompleteSnapshotRecord() {
    val decoded = AttachmentOperationCodec.decode(
      record(
        attachmentId = 42L,
        phase = 2,
        flags = 0,
        bytesUploaded = 1_000L,
        sizeBytes = 1_000L,
        remotePath = "/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png",
      ),
    )!!
    assertEquals(42L, decoded.attachmentId)
    assertEquals(AttachmentOpPhase.UPLOADED, decoded.phase)
    assertEquals("uploaded", decoded.wirePhase)
    assertEquals(1_000L, decoded.bytesUploaded)
    assertEquals("/home/dev/.local/share/meeterm/attachments/meeterm-20260101-120000-0123456789abcdef.png", decoded.remotePath)
    assertFalse(decoded.insertUnconfirmed)
  }

  @Test
  fun decodesInsertUnconfirmedFlagAndErrors() {
    val decoded = AttachmentOperationCodec.decode(
      record(phase = 3, flags = 1, errorCode = "input_unconfirmed", errorMessage = "unconfirmed"),
    )!!
    assertEquals(AttachmentOpPhase.INSERTED, decoded.phase)
    assertTrue(decoded.insertUnconfirmed)
    assertEquals("input_unconfirmed", decoded.errorCode)
    assertEquals("unconfirmed", decoded.errorMessage)
  }

  @Test
  fun rejectsShortRecordsAndUnknownPhases() {
    assertNull(AttachmentOperationCodec.decode(record().copyOf(512)))
    assertNull(AttachmentOperationCodec.decode(record(phase = 99)))
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
      AttachmentOpPhase.CANCELLED, AttachmentOpPhase.FAILED, AttachmentOpPhase.DELETED,
    )) {
      machine.applySnapshot(operation(machine.operation!!.attachmentId, phase))
      assertTrue("expected re-begin after $phase", machine.canBeginUpload())
      assertTrue(machine.recordBegin(70L + phase.wireValue, 100L, "a.png"))
    }
  }

  private fun operation(id: Long, phase: AttachmentOpPhase) = AttachmentOperation(
    attachmentId = id,
    phase = phase,
    flags = 0,
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
  fun clearsOperationForDiscardAndNewPick() {
    val machine = AttachmentOpMachine()
    machine.recordBegin(7L, 100L, "a.png")
    machine.clear()
    assertNull(machine.operation)
    assertTrue(machine.canBeginUpload())
  }
}
