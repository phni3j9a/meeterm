import Foundation

/**
 * App-owned attachment files under `Caches/attachments/`.
 *
 * Sources stream through a bounded buffer; the picker never hands the
 * pipeline an unbounded Data. Names are app-generated so provider filenames
 * can never traverse or collide inside the directory.
 */
final class AttachmentStore {
  enum StageCopy {
    case ok(fileName: String, byteCount: Int64)
    case rejected(String)
  }

  private let fileManager = FileManager.default

  let directoryURL: URL

  init() throws {
    let base = try fileManager.url(
      for: .cachesDirectory,
      in: .userDomainMask,
      appropriateFor: nil,
      create: true
    )
    directoryURL = base.appendingPathComponent("attachments", isDirectory: true)
    try fileManager.createDirectory(
      at: directoryURL,
      withIntermediateDirectories: true
    )
  }

  init(directoryURL: URL) throws {
    self.directoryURL = directoryURL
    try fileManager.createDirectory(
      at: directoryURL,
      withIntermediateDirectories: true
    )
  }

  private func newFileName(_ fileExtension: String) -> String {
    let token = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
    return "att_\(String(token.prefix(24))).\(fileExtension)"
  }

  func newStagingFileName() -> String { newFileName("bin") }

  func newPreparedFileName(_ format: AttachmentImageFormat) -> String {
    newFileName(format.fileExtension)
  }

  func resolveAppFile(_ name: String) -> URL? {
    guard AttachmentFileNames.isValid(name) else { return nil }
    let url = directoryURL.appendingPathComponent(name, isDirectory: false)
    guard url.deletingLastPathComponent().standardizedFileURL.path ==
          directoryURL.standardizedFileURL.path else { return nil }
    return url
  }

  func stagingFile(_ name: String) -> URL? {
    guard AttachmentFileNames.isStagingName(name) else { return nil }
    return resolveAppFile(name)
  }

  func preparedFile(_ name: String) -> URL? {
    guard AttachmentFileNames.isPreparedName(name) else { return nil }
    return resolveAppFile(name)
  }

  /// Bounded URL → staging-file copy; refuses early on size overruns.
  func stage(from sourceURL: URL, fileName: String, securityScoped: Bool) -> StageCopy {
    guard let target = stagingFile(fileName) else {
      return .rejected(AttachmentLimits.errorArgument)
    }
    if securityScoped, !sourceURL.startAccessingSecurityScopedResource() {
      return .rejected(AttachmentLimits.errorIO)
    }
    defer {
      if securityScoped { sourceURL.stopAccessingSecurityScopedResource() }
    }
    guard let input = InputStream(url: sourceURL) else {
      return .rejected(AttachmentLimits.errorIO)
    }
    input.open()
    defer { input.close() }
    guard let output = OutputStream(url: target, append: false) else {
      return .rejected(AttachmentLimits.errorIO)
    }
    output.open()
    defer { output.close() }
    var total: Int64 = 0
    var buffer = [UInt8](repeating: 0, count: 64 * 1024)
    while true {
      let read = input.read(&buffer, maxLength: buffer.count)
      if read < 0 { try? fileManager.removeItem(at: target); return .rejected(AttachmentLimits.errorIO) }
      if read == 0 { break }
      total += Int64(read)
      if total > AttachmentLimits.maxSourceBytes {
        try? fileManager.removeItem(at: target)
        return .rejected(AttachmentLimits.errorInputTooLarge)
      }
      var written = 0
      while written < read {
        let amount = buffer.withUnsafeBytes { raw -> Int in
          guard let base = raw.baseAddress?.advanced(by: written) else { return -1 }
          return output.write(base.assumingMemoryBound(to: UInt8.self), maxLength: read - written)
        }
        if amount <= 0 {
          try? fileManager.removeItem(at: target)
          return .rejected(AttachmentLimits.errorIO)
        }
        written += amount
      }
    }
    guard total > 0 else {
      try? fileManager.removeItem(at: target)
      return .rejected(AttachmentLimits.errorIO)
    }
    return .ok(fileName: fileName, byteCount: total)
  }

  /// Move a picker-provided temp file into staging with a size check first.
  func stageFile(_ sourceURL: URL, fileName: String) -> StageCopy {
    guard let target = stagingFile(fileName) else {
      return .rejected(AttachmentLimits.errorArgument)
    }
    do {
      let attributes = try fileManager.attributesOfItem(atPath: sourceURL.path)
      let size = (attributes[.size] as? NSNumber)?.int64Value ?? 0
      guard size > 0 else { return .rejected(AttachmentLimits.errorIO) }
      guard size <= AttachmentLimits.maxSourceBytes else {
        return .rejected(AttachmentLimits.errorInputTooLarge)
      }
      try? fileManager.removeItem(at: target)
      try fileManager.moveItem(at: sourceURL, to: target)
      return .ok(fileName: fileName, byteCount: size)
    } catch {
      return .rejected(AttachmentLimits.errorIO)
    }
  }

  func readHeader(_ fileName: String) -> (header: Data, byteCount: Int64)? {
    guard let url = stagingFile(fileName),
          fileManager.fileExists(atPath: url.path),
          let size = (try? fileManager.attributesOfItem(atPath: url.path))?[.size] as? NSNumber,
          let handle = try? FileHandle(forReadingFrom: url) else { return nil }
    defer { try? handle.close() }
    let header = (try? handle.read(upToCount: AttachmentLimits.headerSniffBytes)) ?? Data()
    return (header, size.int64Value)
  }

  func stagingURL(_ fileName: String) -> URL? {
    guard let url = stagingFile(fileName), fileManager.fileExists(atPath: url.path) else { return nil }
    return url
  }

  func preparedURL(_ fileName: String) -> URL? {
    guard let url = preparedFile(fileName), fileManager.fileExists(atPath: url.path) else { return nil }
    return url
  }

  func previewUri(_ fileName: String) -> String {
    guard let url = preparedFile(fileName) else { return "" }
    return url.absoluteString
  }

  func delete(_ names: String?...) {
    for name in names {
      guard let name = name, let url = resolveAppFile(name) else { continue }
      try? fileManager.removeItem(at: url)
    }
  }

  /// Startup reclaim of abandoned files from a previous process.
  func reclaimStale(keeping keptNames: Set<String>) {
    guard let contents = try? fileManager.contentsOfDirectory(
      at: directoryURL,
      includingPropertiesForKeys: nil
    ) else { return }
    for url in contents where !keptNames.contains(url.lastPathComponent) {
      try? fileManager.removeItem(at: url)
    }
  }
}
