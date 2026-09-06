import Foundation

/// App-private host-key trust storage. The file is created before Rust starts
/// an SSH request; inability to create or inspect it fails closed.
enum KnownHostsStore {
  enum StoreError: Error {
    case unavailable
  }

  static func path() throws -> String {
    guard let applicationSupport = FileManager.default.urls(
      for: .applicationSupportDirectory,
      in: .userDomainMask
    ).first else {
      throw StoreError.unavailable
    }

    let directory = applicationSupport
      .appendingPathComponent("meeterm", isDirectory: true)
      .appendingPathComponent("ssh", isDirectory: true)
    let file = directory.appendingPathComponent("known_hosts", isDirectory: false)
    let manager = FileManager.default

    do {
      try manager.createDirectory(
        at: directory,
        withIntermediateDirectories: true,
        attributes: [.posixPermissions: 0o700]
      )
      try manager.setAttributes(
        [.posixPermissions: 0o700],
        ofItemAtPath: directory.path
      )

      var isDirectory: ObjCBool = false
      if manager.fileExists(atPath: file.path, isDirectory: &isDirectory) {
        guard !isDirectory.boolValue else {
          throw StoreError.unavailable
        }
      } else if !manager.createFile(
        atPath: file.path,
        contents: Data(),
        attributes: [.posixPermissions: 0o600]
      ) {
        throw StoreError.unavailable
      }

      try manager.setAttributes(
        [.posixPermissions: 0o600],
        ofItemAtPath: file.path
      )
      return file.path
    } catch {
      throw StoreError.unavailable
    }
  }
}
