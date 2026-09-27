import Foundation

/**
 * Rust-owned attachment operation state (attachment-ffi contract).
 *
 * The core operation lifecycle is authoritative; this model only decodes the
 * fixed-size snapshot record and pins the adapter-side transition guards:
 * a second upload is refused while an op is live, snapshots from a stale
 * attachment id are dropped, and insert/delete capabilities follow the phase.
 */
enum AttachmentOpPhase: Int {
  case pending = 0
  case uploading = 1
  case uploaded = 2
  case inserted = 3
  case failed = 4
  case cancelled = 5

  var wireName: String {
    switch self {
    case .pending: return "pending"
    case .uploading: return "uploading"
    case .uploaded: return "uploaded"
    case .inserted: return "inserted"
    case .failed: return "failed"
    case .cancelled: return "cancelled"
    }
  }
}

struct AttachmentOperation: Equatable {
  let attachmentId: UInt64
  let phase: AttachmentOpPhase
  let flags: UInt32
  let bytesUploaded: UInt64
  let sizeBytes: UInt64
  let remotePath: String
  let displayName: String
  let errorCode: String
  let errorMessage: String

  /// flags & 0x1: the input queue accepted the line; delivery unconfirmed.
  var insertUnconfirmed: Bool { flags & 0x1 != 0 }

  /// flags & 0x2: the meeterm-created remote file was explicitly deleted.
  var remoteRemoved: Bool { flags & 0x2 != 0 }

  /// flags & 0x4: a job (upload / verify+insert / remove) is in flight on
  /// this operation. The core clears the previous reason at job start and
  /// drops the flag when that attempt's outcome lands — UI busy display
  /// and polling key off this bit, never off a stale errorCode.
  var jobInFlight: Bool { flags & 0x4 != 0 }

  /// Explicit Retry upload: pending/failed ops, plus an uploaded op whose
  /// remote file was deleted — the core re-uploads that same operation.
  /// Refused while a job is in flight.
  var canRetryUpload: Bool {
    !jobInFlight && (phase == .pending || phase == .failed ||
      (phase == .uploaded && remoteRemoved))
  }

  /// Insert is one verified job: intent check → fresh fence → remote
  /// lstat → single-line paste. It is allowed on an uploaded op only —
  /// an inserted op is never re-inserted — and never while another job
  /// runs.
  var canInsert: Bool {
    !remoteRemoved && !jobInFlight && phase == .uploaded
  }

  /// Cancel applies while transfer work may still be running.
  var canCancel: Bool { phase == .pending || phase == .uploading }

  /// Remote delete covers every phase that may still own a file; refused
  /// while a job is in flight (one in-flight job per operation).
  var canDeleteRemote: Bool {
    !remoteRemoved && !jobInFlight &&
      (phase == .uploaded || phase == .inserted || phase == .failed || phase == .cancelled)
  }
}

/**
 * Tracks the live core operation. The snapshot is authoritative, but adapter
 * guards keep a stale attachment id or a second upload from corrupting state.
 */
final class AttachmentOpMachine {
  private(set) var operation: AttachmentOperation?

  /// A new upload may only replace a fully terminal (or absent) operation.
  func canBeginUpload() -> Bool {
    guard let op = operation else { return true }
    return op.phase == .cancelled || op.phase == .failed
  }

  /// Record the attachment id accepted by the core; false means refused.
  @discardableResult
  func recordBegin(attachmentId: UInt64, sizeBytes: UInt64, displayName: String) -> Bool {
    guard attachmentId > 0, canBeginUpload() else { return false }
    operation = AttachmentOperation(
      attachmentId: attachmentId,
      phase: .uploading,
      flags: 0,
      bytesUploaded: 0,
      sizeBytes: sizeBytes,
      remotePath: "",
      displayName: displayName,
      errorCode: "",
      errorMessage: ""
    )
    return true
  }

  /// Apply the authoritative core snapshot; stale ids are dropped.
  @discardableResult
  func applySnapshot(_ snapshot: AttachmentOperation) -> Bool {
    guard let current = operation, current.attachmentId == snapshot.attachmentId else {
      return false
    }
    // A locally-cancelled op keeps its phase until the core confirms the
    // cancel (or finishes); late upload bytes never resurrect it.
    if current.phase == .cancelled, snapshot.phase == .uploading {
      return true
    }
    operation = snapshot
    return true
  }

  /// Local cancel mark; the core drops delayed completions itself.
  func markCancelled() {
    guard var op = operation else { return }
    op = AttachmentOperation(
      attachmentId: op.attachmentId,
      phase: .cancelled,
      flags: op.flags,
      bytesUploaded: op.bytesUploaded,
      sizeBytes: op.sizeBytes,
      remotePath: op.remotePath,
      displayName: op.displayName,
      errorCode: op.errorCode,
      errorMessage: op.errorMessage
    )
    operation = op
  }

  func clear() {
    operation = nil
  }
}

/// Fixed-offset decoder for `meeterm_attachment_snapshot_t` (1000 bytes).
enum AttachmentOperationCodec {
  static let recordSize = 1000

  static func decode(_ bytes: Data) -> AttachmentOperation? {
    guard bytes.count >= recordSize else { return nil }
    guard let phaseValue = readUInt32(bytes, at: 0),
          let phase = AttachmentOpPhase(rawValue: Int(phaseValue)) else { return nil }
    let flags = readUInt32(bytes, at: 4) ?? 0
    let attachmentId = readUInt64(bytes, at: 8) ?? 0
    let bytesUploaded = readUInt64(bytes, at: 16) ?? 0
    let sizeBytes = readUInt64(bytes, at: 24) ?? 0
    return AttachmentOperation(
      attachmentId: attachmentId,
      phase: phase,
      flags: flags,
      bytesUploaded: bytesUploaded,
      sizeBytes: sizeBytes,
      remotePath: field(bytes, lengthOffset: 32, dataOffset: 34, capacity: 512),
      displayName: field(bytes, lengthOffset: 546, dataOffset: 548, capacity: 128),
      errorCode: field(bytes, lengthOffset: 676, dataOffset: 678, capacity: 64),
      errorMessage: field(bytes, lengthOffset: 742, dataOffset: 744, capacity: 256)
    )
  }

  private static func readUInt32(_ data: Data, at offset: Int) -> UInt32? {
    guard offset + 4 <= data.count else { return nil }
    return data.withUnsafeBytes { raw in
      raw.load(fromByteOffset: offset, as: UInt32.self)
    }
  }

  private static func readUInt64(_ data: Data, at offset: Int) -> UInt64? {
    guard offset + 8 <= data.count else { return nil }
    return data.withUnsafeBytes { raw in
      raw.load(fromByteOffset: offset, as: UInt64.self)
    }
  }

  private static func field(
    _ data: Data,
    lengthOffset: Int,
    dataOffset: Int,
    capacity: Int
  ) -> String {
    guard lengthOffset + 2 <= data.count else { return "" }
    let declared = data.withUnsafeBytes { raw -> Int in
      Int(raw.load(fromByteOffset: lengthOffset, as: UInt16.self))
    }
    let length = min(declared, capacity)
    guard dataOffset + length <= data.count else { return "" }
    return String(decoding: data[dataOffset ..< dataOffset + length], as: UTF8.self)
  }
}
