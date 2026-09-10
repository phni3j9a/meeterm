import Foundation
import Security

/// Saved credentials stay in this device's Keychain. The ordinary file contains
/// only profile metadata, preferences and opaque Keychain record IDs.
enum ClientStore {
  private static let lock = NSLock()
  private static let service = "dev.meeterm.credentials.v1"
  private static let maxProfiles = 100

  private static func guarded<T>(_ body: () throws -> T) throws -> T {
    lock.lock()
    defer { lock.unlock() }
    do { return try body() }
    catch {
      throw NSError(domain: "dev.meeterm.storage", code: 1, userInfo: [NSLocalizedDescriptionKey:
        "Saved server storage is unavailable or its values are invalid. Check the details or enter credentials again."])
    }
  }

  private enum Failure: Error { case invalid, unavailable }

  private static func fileURL() throws -> URL {
    guard let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
      throw Failure.unavailable
    }
    let directory = support.appendingPathComponent("meeterm", isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
    return directory.appendingPathComponent("client-v1.json")
  }

  private static func read() throws -> [String: Any] {
    let url = try fileURL()
    if !FileManager.default.fileExists(atPath: url.path) { return ["version": 1, "profiles": [[String: Any]]()] }
    let info = try FileManager.default.attributesOfItem(atPath: url.path)
    guard let length = info[.size] as? NSNumber, length.intValue <= 16 * 1024 * 1024 else { throw Failure.invalid }
    let data = try Data(contentsOf: url)
    guard var state = try JSONSerialization.jsonObject(with: data) as? [String: Any],
          integer(state["version"]) == 1, let profiles = state["profiles"] as? [[String: Any]],
          profiles.count <= maxProfiles else { throw Failure.invalid }
    if let pending = state["pendingCredentialDeletes"] as? [String] {
      guard pending.count <= maxProfiles else { throw Failure.invalid }
      let live = Set(profiles.compactMap { $0["credentialID"] as? String })
      for id in pending where !live.contains(id) { try removeCredential(id) }
      state.removeValue(forKey: "pendingCredentialDeletes")
      try write(state)
    }
    return state
  }

  private static func write(_ state: [String: Any]) throws {
    let url = try fileURL()
    let data = try JSONSerialization.data(withJSONObject: state, options: [.sortedKeys])
    try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    // The directory is already app-private. A permissions adjustment after the
    // atomic commit must not turn success into an apparent rollback.
    try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
  }

  private static func query(_ id: String) -> [String: Any] {
    [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
     kSecAttrAccount as String: id, kSecAttrSynchronizable as String: false]
  }

  private static func hasCredential(_ profile: [String: Any]) -> Bool {
    guard let id = profile["credentialID"] as? String else { return false }
    // Attributes-only lookup. Do not decrypt secrets to render the server list.
    var lookup = query(id)
    lookup[kSecReturnAttributes as String] = true
    return SecItemCopyMatching(lookup as CFDictionary, nil) == errSecSuccess
  }

  private static func addCredential(_ credential: [String: Any], profile: [String: Any], id: String) throws {
    var item = query(id)
    item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
    item[kSecValueData as String] = try JSONSerialization.data(withJSONObject:
      ["identity": identity(profile), "credential": credential])
    guard SecItemAdd(item as CFDictionary, nil) == errSecSuccess else { throw Failure.unavailable }
  }

  private static func removeCredential(_ id: String?) throws {
    guard let id else { return }
    let result = SecItemDelete(query(id) as CFDictionary)
    guard result == errSecSuccess || result == errSecItemNotFound else { throw Failure.unavailable }
  }

  private static func credential(_ profile: [String: Any]) throws -> [String: Any] {
    guard let id = profile["credentialID"] as? String else { throw Failure.unavailable }
    var lookup = query(id)
    lookup[kSecReturnData as String] = true
    lookup[kSecMatchLimit as String] = kSecMatchLimitOne
    var result: CFTypeRef?
    guard SecItemCopyMatching(lookup as CFDictionary, &result) == errSecSuccess,
          let data = result as? Data,
          let record = try JSONSerialization.jsonObject(with: data) as? [String: Any],
          let storedIdentity = record["identity"] as? [String], storedIdentity == identity(profile),
          let secret = record["credential"] as? [String: Any] else { throw Failure.unavailable }
    return try validateCredential(secret, profile: profile)
  }

  private static func identity(_ profile: [String: Any]) -> [String] {
    ["id", "host", "port", "username", "authMethod"].map { String(describing: profile[$0] ?? "") }
  }

  private static func record(_ profile: [String: Any]) -> [String: Any] {
    var result = profile
    result.removeValue(forKey: "credentialID")
    result["credentialSaved"] = hasCredential(profile)
    return result
  }

  static func profiles() throws -> [[String: Any]] {
    try guarded { (try read()["profiles"] as! [[String: Any]]).map(record) }
  }

  static func saveProfile(_ values: [String: Any], credential: [String: Any]?, keepCredential: Bool) throws -> [String: Any] {
    try guarded {
      var state = try read()
      var profiles = state["profiles"] as! [[String: Any]]
      var profile = try validateProfile(values)
      let id = profile["id"] as! String
      let index = profiles.firstIndex { $0["id"] as? String == id }
      guard index != nil || profiles.count < maxProfiles else { throw Failure.invalid }
      let previous = index.map { profiles[$0] }
      let oldCredentialID = previous?["credentialID"] as? String
      var newCredentialID: String?
      if let credential {
        let normalized = try validateCredential(credential, profile: profile)
        let newID = UUID().uuidString.lowercased()
        // Journal the unused record before creating it. If the process dies or
        // the metadata commit fails, the next read can remove it without ever
        // losing track of a secret. Live references are excluded from cleanup.
        var preparation = state
        preparation["pendingCredentialDeletes"] = [newID]
        try write(preparation)
        try addCredential(normalized, profile: profile, id: newID)
        newCredentialID = newID
        profile["credentialID"] = newCredentialID
      } else if keepCredential, let previous, identity(previous) == identity(profile) {
        profile["credentialID"] = oldCredentialID
      }
      if let index { profiles[index] = profile } else { profiles.append(profile) }
      state["profiles"] = profiles
      // Keep the old reference on disk until its deletion succeeds. An
      // explicit forget must never strand an unreferenced Keychain record.
      if oldCredentialID != profile["credentialID"] as? String { try removeCredential(oldCredentialID) }
      // On failure, the on-disk journal owns cleanup. Do not delete the new
      // record here: a filesystem error must never remove a live reference.
      try write(state)
      return record(profile)
    }
  }

  static func deleteProfile(_ id: String) throws {
    try guarded {
      var state = try read()
      var profiles = state["profiles"] as! [[String: Any]]
      guard let index = profiles.firstIndex(where: { $0["id"] as? String == id }) else { return }
      try removeCredential(profiles[index]["credentialID"] as? String)
      profiles.remove(at: index)
      state["profiles"] = profiles
      try write(state)
    }
  }

  /// Consumed by the native connection function; never returned to JavaScript.
  static func connectionOptions(_ id: String) throws -> [String: Any] {
    try guarded {
      let profiles = try read()["profiles"] as! [[String: Any]]
      guard let profile = profiles.first(where: { $0["id"] as? String == id }) else { throw Failure.invalid }
      var result = profile
      result.removeValue(forKey: "credentialID")
      result.merge(try credential(profile)) { _, secret in secret }
      return result
    }
  }

  static let defaultPreferences: [String: Any] = ["fontSize": 15, "theme": "system", "scrollbackLines": 10000, "automaticReconnect": true]

  static func preferences() throws -> [String: Any] {
    try guarded {
      guard let values = try read()["preferences"] as? [String: Any] else { return defaultPreferences }
      return try validatePreferences(values)
    }
  }

  static func setPreferences(_ values: [String: Any]) throws {
    try guarded {
      let preferences = try validatePreferences(values)
      var state = try read()
      state["preferences"] = preferences
      try write(state)
    }
  }

  static func validatePreferences(_ values: [String: Any]) throws -> [String: Any] {
    guard let size = integer(values["fontSize"]), (10...24).contains(size),
          let lines = integer(values["scrollbackLines"]), (1000...50000).contains(lines),
          let theme = values["theme"] as? String, ["system", "light", "dark"].contains(theme),
          let automatic = values["automaticReconnect"] as? NSNumber,
          CFGetTypeID(automatic) == CFBooleanGetTypeID() else { throw Failure.invalid }
    return ["fontSize": size, "theme": theme, "scrollbackLines": lines, "automaticReconnect": automatic.boolValue]
  }

  private static func validateProfile(_ values: [String: Any]) throws -> [String: Any] {
    let requested = values["id"] as? String ?? ""
    let id = requested.isEmpty ? UUID().uuidString.lowercased() : requested
    guard UUID(uuidString: id) != nil,
          let port = integer(values["port"]), (1...65535).contains(port),
          let method = values["authMethod"] as? String, ["publicKey", "password"].contains(method) else { throw Failure.invalid }
    var profile: [String: Any] = ["id": id, "port": port, "authMethod": method]
    for field in ["name", "host", "username"] {
      guard let string = values[field] as? String else { throw Failure.invalid }
      let value = string.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !value.isEmpty, value.utf16.count <= 256,
            !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw Failure.invalid }
      profile[field] = value
    }
    return profile
  }

  private static func validateCredential(_ values: [String: Any], profile: [String: Any]) throws -> [String: Any] {
    guard let method = values["authMethod"] as? String, method == profile["authMethod"] as? String else { throw Failure.invalid }
    var result: [String: Any] = ["authMethod": method]
    for field in method == "password" ? ["password"] : ["privateKey", "passphrase"] {
      guard let value = values[field] as? String, value.utf16.count <= 65536,
            !value.utf8.contains(0), field == "passphrase" || !value.isEmpty else { throw Failure.invalid }
      result[field] = value
    }
    return result
  }

  private static func integer(_ value: Any?) -> Int? {
    guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(),
          number.doubleValue.isFinite, Double(number.intValue) == number.doubleValue else { return nil }
    return number.intValue
  }
}
