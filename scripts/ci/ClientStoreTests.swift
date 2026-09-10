import Foundation
import Security
import XCTest

/// Runs against the actual Simulator Keychain in the isolated XCTest runner.
/// Only generated test profiles are removed; no production storage is shared.
final class ClientStoreTests: XCTestCase {
  private var recordedIssue = false

  override func record(_ issue: XCTIssue) {
    recordedIssue = true
    appendValidation("result=failed source_line=\(issue.sourceCodeContext.location?.lineNumber ?? 0)")
    super.record(issue)
  }

  private func appendValidation(_ line: String) {
    guard let directory = ProcessInfo.processInfo.environment["MEETERM_IOS_ARTIFACT_DIR"] else { return }
    let path = URL(fileURLWithPath: directory).appendingPathComponent("ios-native-storage-validation.txt")
    let data = Data((line + "\n").utf8)
    if let handle = try? FileHandle(forWritingTo: path) {
      handle.seekToEndOfFile()
      handle.write(data)
      try? handle.close()
    } else {
      try? data.write(to: path, options: .atomic)
    }
  }

  func testInterruptedCredentialWriteIsCleanedWithoutDeletingLiveCredentials() throws {
    let id = UUID().uuidString.lowercased()
    let profile: [String: Any] = ["id": id, "name": "Journal fixture", "host": "fixture.invalid",
      "port": 22, "username": "fixture", "authMethod": "password"]
    defer { try? ClientStore.deleteProfile(id) }
    _ = try ClientStore.saveProfile(profile, credential: ["authMethod": "password", "password": "fixture-only"], keepCredential: false)
    let support = try XCTUnwrap(FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first)
    let file = support.appendingPathComponent("meeterm/client-v1.json")
    var state = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
    let profiles = try XCTUnwrap(state["profiles"] as? [[String: Any]])
    let live = try XCTUnwrap(profiles.first { $0["id"] as? String == id }?["credentialID"] as? String)
    let orphan = UUID().uuidString.lowercased()
    let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: "dev.meeterm.credentials.v1", kSecAttrAccount as String: orphan,
      kSecAttrSynchronizable as String: false]
    defer { SecItemDelete(query as CFDictionary) }
    var item = query
    item[kSecValueData as String] = Data("fixture-only".utf8)
    item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
    XCTAssertEqual(SecItemAdd(item as CFDictionary, nil), errSecSuccess)
    // Model a process interruption between Keychain add and metadata commit.
    state["pendingCredentialDeletes"] = [orphan, live]
    try JSONSerialization.data(withJSONObject: state).write(to: file, options: .atomic)
    _ = try ClientStore.profiles()
    XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, nil), errSecItemNotFound)
    XCTAssertEqual(try ClientStore.connectionOptions(id)["password"] as? String, "fixture-only")
    let recovered = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
    XCTAssertNil(recovered["pendingCredentialDeletes"])
    if !recordedIssue { appendValidation("case=interrupted_write_cleanup result=passed") }
  }

  func testCredentialRemainsNativeAndCannotFollowAnEndpointChange() throws {
    let id = UUID().uuidString.lowercased()
    let profile: [String: Any] = ["id": id, "name": "Fixture", "host": "fixture.invalid",
      "port": 22, "username": "fixture", "authMethod": "password"]
    defer { try? ClientStore.deleteProfile(id) }
    let saved = try ClientStore.saveProfile(profile,
      credential: ["authMethod": "password", "password": "  fixture-only  "], keepCredential: false)
    XCTAssertEqual(saved["credentialSaved"] as? Bool, true)
    XCTAssertNil(saved["password"])
    XCTAssertNil(saved["credentialID"])
    let listed = try XCTUnwrap(ClientStore.profiles().first { $0["id"] as? String == id })
    XCTAssertNil(listed["password"])
    XCTAssertNil(listed["credentialID"])
    let connection = try ClientStore.connectionOptions(id)
    XCTAssertEqual(connection["password"] as? String, "  fixture-only  ")
    var renamed = profile
    renamed["name"] = "Renamed fixture"
    let retained = try ClientStore.saveProfile(renamed, credential: nil, keepCredential: true)
    XCTAssertEqual(retained["credentialSaved"] as? Bool, true)
    renamed["host"] = "different.invalid"
    let changed = try ClientStore.saveProfile(renamed, credential: nil, keepCredential: true)
    XCTAssertEqual(changed["credentialSaved"] as? Bool, false)
    XCTAssertThrowsError(try ClientStore.connectionOptions(id))
    if !recordedIssue { appendValidation("case=credential_endpoint_binding result=passed") }
  }

  func testRemovingSavedCredentialAndProfile() throws {
    let id = UUID().uuidString.lowercased()
    let profile: [String: Any] = ["id": id, "name": "Fixture key", "host": "fixture.invalid",
      "port": 22, "username": "fixture", "authMethod": "publicKey"]
    defer { try? ClientStore.deleteProfile(id) }
    _ = try ClientStore.saveProfile(profile, credential: ["authMethod": "publicKey",
      "privateKey": "fixture-only-key", "passphrase": "fixture-only-passphrase"], keepCredential: false)
    XCTAssertEqual(try ClientStore.connectionOptions(id)["privateKey"] as? String, "fixture-only-key")
    let forgotten = try ClientStore.saveProfile(profile, credential: nil, keepCredential: false)
    XCTAssertEqual(forgotten["credentialSaved"] as? Bool, false)
    XCTAssertThrowsError(try ClientStore.connectionOptions(id))
    try ClientStore.deleteProfile(id)
    XCTAssertFalse(try ClientStore.profiles().contains { $0["id"] as? String == id })
    if !recordedIssue { appendValidation("case=remove_saved_credential result=passed") }
  }

  func testPreferencesRoundTripAndValidation() throws {
    let old = try ClientStore.preferences()
    defer { try? ClientStore.setPreferences(old) }
    let updated: [String: Any] = ["fontSize": 20, "theme": "light", "scrollbackLines": 20000, "automaticReconnect": false]
    try ClientStore.setPreferences(updated)
    let actual = try ClientStore.preferences()
    XCTAssertEqual(actual["fontSize"] as? Int, 20)
    XCTAssertEqual(actual["theme"] as? String, "light")
    XCTAssertEqual(actual["scrollbackLines"] as? Int, 20000)
    XCTAssertEqual(actual["automaticReconnect"] as? Bool, false)
    let invalidValues: [(String, Any)] = [("fontSize", 25), ("fontSize", true),
      ("scrollbackLines", 50001), ("theme", "bad"), ("automaticReconnect", 1)]
    for (field, value) in invalidValues {
      var invalid = updated
      invalid[field] = value
      XCTAssertThrowsError(try ClientStore.setPreferences(invalid))
    }
    if !recordedIssue { appendValidation("case=preferences_validation result=passed") }
  }
}
