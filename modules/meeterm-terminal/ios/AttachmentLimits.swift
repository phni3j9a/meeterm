import Foundation

/**
 * Issue #28 attachment limits and header inspection.
 *
 * Everything here is Foundation-only so host XCTest bundles can pin the rules
 * without UIKit. The ImageIO pipeline in `AttachmentNormalize` must keep
 * byte/pixel checks ahead of decode.
 */
enum AttachmentLimits {
  /// App-owned staging input ceiling, applied while the provider streams.
  static let maxSourceBytes: Int64 = 24 * 1024 * 1024

  /// Bytes read from a staged file to validate magic + dimensions.
  static let headerSniffBytes: Int = 64 * 1024

  /// Per-side input ceiling; larger images are rejected before decode.
  static let maxInputDimension: Int = 16_384

  /// Total input pixel ceiling; larger images are rejected before decode.
  static let maxInputPixels: Int64 = 100_000_000

  /// Per-side output ceiling. Ordinary phone screenshots stay untouched.
  static let maxOutputDimension: Int = 4_096

  /// Total output pixel ceiling used to bound the decoded image.
  static let maxOutputPixels: Int64 = 4_096 * 4_096

  /// Re-encoded output ceiling; a larger result is an explicit error.
  static let maxOutputBytes: Int64 = 16 * 1024 * 1024

  /// JPEG re-encode quality for JPEG-sourced attachments.
  static let jpegOutputQuality: Double = 0.9

  static let errorInputTooLarge = "attachment_too_large"
  static let errorOutputTooLarge = "attachment_output_too_large"
  static let errorUnsupported = "attachment_unsupported_format"
  static let errorUnsupportedHeic = "attachment_unsupported_heic"
  static let errorMalformed = "attachment_malformed_header"
  static let errorDimensions = "attachment_too_many_pixels"
  static let errorDecode = "attachment_decode_failed"
  static let errorIO = "attachment_io_failed"
  static let errorState = "attachment_invalid_state"
  static let errorMissing = "attachment_missing"
  static let errorArgument = "attachment_invalid_argument"
  // Contract error names (attachment-ffi.md) surfaced for destination
  // fencing; kept unprefixed so JS sees one vocabulary end to end.
  static let errorUnknownIntent = "unknown_intent"
  static let errorDestinationChanged = "destination_changed"
  static let errorDestinationMissing = "destination_missing"
  static let reasonComposing = "composing"
  static let reasonNoAttachment = "no_attachment"
  static let reasonCorePending = "core_contract_pending"
}

enum AttachmentImageFormat: String {
  case png
  case jpeg

  var fileExtension: String { self == .png ? "png" : "jpg" }
  var uniformTypeIdentifier: String { self == .png ? "public.png" : "public.jpeg" }
}

/// Detected magic + declared dimensions, read without decoding pixels.
struct AttachmentImageHeader: Equatable {
  let format: AttachmentImageFormat
  let width: Int
  let height: Int
}

/// Magic-byte and header-dimension sniffing shared by both pick paths.
enum AttachmentImageSniffer {
  enum SniffResult: Equatable {
    case ok(AttachmentImageHeader)
    case rejected(String)
  }

  private static let pngMagic: [UInt8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
  private static let heicBrands: Set<String> = [
    "heic", "heix", "hevc", "hevx", "heim", "heis", "hevm", "hevs", "mif1", "msf1",
  ]

  static func sniff(_ prefix: Data) -> SniffResult {
    if prefix.count >= 8, prefix.prefix(8).elementsEqual(pngMagic) {
      return sniffPng(prefix)
    }
    if prefix.count >= 2, prefix[0] == 0xFF, prefix[1] == 0xD8 {
      return sniffJpeg(prefix)
    }
    if isHeicFamily(prefix) {
      return .rejected(AttachmentLimits.errorUnsupportedHeic)
    }
    return .rejected(AttachmentLimits.errorUnsupported)
  }

  static func isHeicFamily(_ prefix: Data) -> Bool {
    // ISO BMFF: a box length (4 bytes) followed by "ftyp" and a brand.
    guard prefix.count >= 12,
          prefix[4] == 0x66, prefix[5] == 0x74, prefix[6] == 0x79, prefix[7] == 0x70 else {
      return false
    }
    let brand = String(decoding: prefix[8..<12], as: UTF8.self).lowercased()
    return heicBrands.contains(brand)
  }

  private static func sniffPng(_ prefix: Data) -> SniffResult {
    // Signature (8) + IHDR length/type (8) + IHDR payload (13 bytes minimum).
    guard prefix.count >= 29 else { return .rejected(AttachmentLimits.errorMalformed) }
    let ihdrLength = readInt32(prefix, 8)
    let ihdrType = String(decoding: prefix[12..<16], as: UTF8.self)
    guard ihdrLength == 13, ihdrType == "IHDR" else {
      return .rejected(AttachmentLimits.errorMalformed)
    }
    let width = readInt32(prefix, 16)
    let height = readInt32(prefix, 20)
    guard width > 0, height > 0 else { return .rejected(AttachmentLimits.errorMalformed) }
    return .ok(AttachmentImageHeader(format: .png, width: Int(width), height: Int(height)))
  }

  private static func sniffJpeg(_ prefix: Data) -> SniffResult {
    var offset = 2
    let limit = prefix.count
    while true {
      while offset < limit, prefix[offset] == 0xFF { offset += 1 }
      guard offset + 4 <= limit else { return .rejected(AttachmentLimits.errorMalformed) }
      let marker = Int(prefix[offset])
      offset += 1
      switch marker {
      // Standalone markers without a length field.
      case 0x01, 0xD0...0xD7:
        continue
      // Entropy-coded scan or end of image reached before a frame header.
      case 0xDA, 0xD9:
        return .rejected(AttachmentLimits.errorMalformed)
      case 0xC0...0xCF:
        // Frame headers carry width/height. C4 (DHT) and C8 (JPG) are not.
        if marker == 0xC4 || marker == 0xC8 || marker == 0xCC {
          guard let next = skipSegment(prefix, offset) else {
            return .rejected(AttachmentLimits.errorMalformed)
          }
          offset = next
          continue
        }
        guard offset + 7 <= limit else { return .rejected(AttachmentLimits.errorMalformed) }
        let segmentLength = readUint16(prefix, offset)
        guard segmentLength >= 8 else { return .rejected(AttachmentLimits.errorMalformed) }
        let height = readUint16(prefix, offset + 3)
        let width = readUint16(prefix, offset + 5)
        guard width > 0, height > 0 else { return .rejected(AttachmentLimits.errorMalformed) }
        return .ok(AttachmentImageHeader(format: .jpeg, width: width, height: height))
      default:
        guard let next = skipSegment(prefix, offset) else {
          return .rejected(AttachmentLimits.errorMalformed)
        }
        offset = next
      }
    }
  }

  private static func skipSegment(_ prefix: Data, _ offset: Int) -> Int? {
    guard offset + 2 <= prefix.count else { return nil }
    let segmentLength = readUint16(prefix, offset)
    guard segmentLength >= 2 else { return nil }
    let next = offset + segmentLength
    return next > offset && next <= prefix.count ? next : nil
  }

  private static func readUint16(_ data: Data, _ offset: Int) -> Int {
    (Int(data[offset]) << 8) | Int(data[offset + 1])
  }

  private static func readInt32(_ data: Data, _ offset: Int) -> Int32 {
    Int32(bitPattern:
      (UInt32(data[offset]) << 24) |
      (UInt32(data[offset + 1]) << 16) |
      (UInt32(data[offset + 2]) << 8) |
      UInt32(data[offset + 3]))
  }
}

/// Pre-decode dimension validation; failures order input bytes before pixels.
enum AttachmentDimensionPolicy {
  enum Check: Equatable {
    case ok(needsScale: Bool)
    case rejected(String)
  }

  static func evaluate(header: AttachmentImageHeader, sourceBytes: Int64) -> Check {
    if sourceBytes > AttachmentLimits.maxSourceBytes {
      return .rejected(AttachmentLimits.errorInputTooLarge)
    }
    if header.width > AttachmentLimits.maxInputDimension ||
       header.height > AttachmentLimits.maxInputDimension {
      return .rejected(AttachmentLimits.errorDimensions)
    }
    let pixels = Int64(header.width) * Int64(header.height)
    if pixels > AttachmentLimits.maxInputPixels || pixels <= 0 {
      return .rejected(AttachmentLimits.errorDimensions)
    }
    return .ok(needsScale:
      header.width > AttachmentLimits.maxOutputDimension ||
      header.height > AttachmentLimits.maxOutputDimension ||
      pixels > AttachmentLimits.maxOutputPixels)
  }

  /// Mirror-aware EXIF orientation model kept identical across platforms.
  static func normalizedExifOrientation(_ value: Int) -> Int { (1...8).contains(value) ? value : 1 }

  /// True for orientations 5–8, which swap width and height when applied.
  static func swapsAxes(_ orientation: Int) -> Bool { normalizedExifOrientation(orientation) >= 5 }

  enum OrientationOp: Equatable { case rotate90, rotate180, rotate270, flipHorizontal }

  /// Canonical ops per orientation, in application order. Mirrors fold into a
  /// horizontal flip composed with a rotation; tests pin all eight values.
  static func orientationOps(_ orientation: Int) -> [OrientationOp] {
    switch normalizedExifOrientation(orientation) {
    case 2: return [.flipHorizontal]
    case 3: return [.rotate180]
    case 4: return [.rotate180, .flipHorizontal]
    case 5: return [.rotate90, .flipHorizontal]
    case 6: return [.rotate90]
    case 7: return [.rotate270, .flipHorizontal]
    case 8: return [.rotate270]
    default: return []
    }
  }
}

/// Files inside the app-owned attachment directory are named by the app.
enum AttachmentFileNames {
  static func isValid(_ value: String) -> Bool {
    value.range(of: #"^[A-Za-z0-9_-]{8,64}\.(bin|png|jpg)$"#, options: .regularExpression) != nil
  }

  static func isStagingName(_ value: String) -> Bool { isValid(value) && value.hasSuffix(".bin") }
  static func isPreparedName(_ value: String) -> Bool {
    isValid(value) && (value.hasSuffix(".png") || value.hasSuffix(".jpg"))
  }
}
