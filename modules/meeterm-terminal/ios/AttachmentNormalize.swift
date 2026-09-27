import Foundation
import ImageIO

/**
 * Validation and re-encode pipeline for staged images.
 *
 * Order is deliberate: cheap header checks run before any decode, the
 * `MaxPixelSize` thumbnail bound keeps decode cost inside the output cap,
 * `CreateThumbnailWithTransform` bakes in the full EXIF orientation
 * (including the mirrored variants), and re-encode through
 * `CGImageDestination` writes only pixel data — EXIF/GPS stays behind.
 */
struct AttachmentNormalize {
  enum Result {
    case ok(AttachmentPreparedImage)
    case rejected(String, String)
  }

  let store: AttachmentStore

  func normalize(stagingFileName: String) -> Result {
    guard let (headerBytes, sourceBytes) = store.readHeader(stagingFileName) else {
      return .rejected(
        AttachmentLimits.errorMissing,
        "The picked image is no longer available."
      )
    }
    let header: AttachmentImageHeader
    switch AttachmentImageSniffer.sniff(headerBytes) {
    case .ok(let value):
      header = value
    case .rejected(let errorCode):
      return .rejected(
        errorCode,
        "The selected file is not a supported PNG or JPEG image."
      )
    }
    switch AttachmentDimensionPolicy.evaluate(header: header, sourceBytes: sourceBytes) {
    case .rejected(let errorCode):
      return .rejected(
        errorCode,
        "The image exceeds the attachment size limits."
      )
    case .ok:
      break
    }
    guard let stagingURL = store.stagingURL(stagingFileName) else {
      return .rejected(
        AttachmentLimits.errorMissing,
        "The picked image is no longer available."
      )
    }

    guard let source = CGImageSourceCreateWithURL(stagingURL as CFURL, nil) else {
      return .rejected(
        AttachmentLimits.errorDecode,
        "The image could not be decoded."
      )
    }
    let hasAlpha = (CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [String: Any])
      .map { ($0[kCGImagePropertyHasAlpha as String] as? Bool) ?? false } ?? false

    // The thumbnail decode bounds pixels before touching full image data.
    let options: [String: Any] = [
      kCGImageSourceShouldCache as String: false,
      kCGImageSourceCreateThumbnailFromImageAlways as String: true,
      kCGImageSourceThumbnailMaxPixelSize as String: AttachmentLimits.maxOutputDimension,
      kCGImageSourceCreateThumbnailWithTransform as String: true,
    ]
    guard let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
      return .rejected(
        AttachmentLimits.errorDecode,
        "The image could not be decoded."
      )
    }

    // PNG stays PNG; JPEG re-encodes as JPEG unless the decode carries alpha.
    let format: AttachmentImageFormat =
      header.format == .png || hasAlpha ? .png : .jpeg
    let preparedName = store.newPreparedFileName(format)
    guard let preparedURL = store.preparedFile(preparedName) else {
      return .rejected(
        AttachmentLimits.errorArgument,
        "The prepared image name was invalid."
      )
    }
    guard let destination = CGImageDestinationCreateWithURL(
      preparedURL as CFURL,
      format.uniformTypeIdentifier as CFString,
      1,
      nil
    ) else {
      return .rejected(
        AttachmentLimits.errorIO,
        "The normalized image could not be written."
      )
    }
    // Only encode settings are attached: source metadata is not carried over.
    var properties: [String: Any] = [:]
    if format == .jpeg {
      properties[kCGImageDestinationLossyCompressionQuality as String] =
        AttachmentLimits.jpegOutputQuality
    }
    CGImageDestinationAddImage(destination, thumbnail, properties as CFDictionary)
    guard CGImageDestinationFinalize(destination) else {
      try? FileManager.default.removeItem(at: preparedURL)
      return .rejected(
        AttachmentLimits.errorIO,
        "The normalized image could not be written."
      )
    }
    guard let size = (try? FileManager.default.attributesOfItem(
      atPath: preparedURL.path
    ))?[.size] as? NSNumber else {
      try? FileManager.default.removeItem(at: preparedURL)
      return .rejected(
        AttachmentLimits.errorIO,
        "The normalized image could not be written."
      )
    }
    if size.int64Value > AttachmentLimits.maxOutputBytes {
      try? FileManager.default.removeItem(at: preparedURL)
      return .rejected(
        AttachmentLimits.errorOutputTooLarge,
        "The normalized image is too large to attach."
      )
    }
    return .ok(
      AttachmentPreparedImage(
        fileName: preparedName,
        format: format,
        width: thumbnail.width,
        height: thumbnail.height,
        byteCount: size.int64Value,
        sourceByteCount: sourceBytes
      )
    )
  }
}
