import Foundation
import Security
import XCTest
@testable import MeetermTerminal

/// Runs in the entitled app host against the production native storage module.
/// Only generated test profiles are removed; no secrets or raw errors are logged.
final class ClientStoreTests: XCTestCase {
  private var recordedIssue = false
  private var currentCase = "unknown"
  private var currentStage = "start"

  override func record(_ issue: XCTIssue) {
    recordedIssue = true
    appendValidation(
      "result=failed source_line=\(issue.sourceCodeContext.location?.lineNumber ?? 0) " +
        "case=\(currentCase) stage=\(currentStage)"
    )
    appendDiagnostic(issue.associatedError)
    super.record(issue)
  }

  private func beginCase(_ name: String) {
    currentCase = name
    currentStage = "start"
  }

  private func stage(_ name: String) {
    currentStage = name
  }

  private func appendDiagnostic(_ error: Error?) {
    guard let error else { return }
    let nsError = error as NSError
    let category = nsError.userInfo[ClientStore.diagnosticCategoryKey] as? NSNumber
    let code = nsError.userInfo[ClientStore.diagnosticCodeKey] as? NSNumber
    if let category, let code, (0...6).contains(category.intValue) {
      appendValidation(
        "case=\(currentCase) stage=\(currentStage) " +
          "diagnostic_category=\(category.intValue) diagnostic_code=\(code.intValue)"
      )
    } else {
      appendValidation(
        "case=\(currentCase) stage=\(currentStage) diagnostic_category=0 diagnostic_code=0"
      )
    }
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
    beginCase("interrupted_write_cleanup")
    let id = UUID().uuidString.lowercased()
    let profile: [String: Any] = ["id": id, "name": "Journal fixture", "host": "fixture.invalid",
      "port": 22, "username": "fixture", "authMethod": "password"]
    defer { try? ClientStore.deleteProfile(id) }
    stage("save_profile")
    _ = try ClientStore.saveProfile(profile, credential: ["authMethod": "password", "password": "fixture-only"], keepCredential: false)
    stage("read_metadata")
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
    stage("add_orphan_credential")
    XCTAssertEqual(SecItemAdd(item as CFDictionary, nil), errSecSuccess)
    // Model a process interruption between Keychain add and metadata commit.
    state["pendingCredentialDeletes"] = [orphan, live]
    stage("write_pending_journal")
    try JSONSerialization.data(withJSONObject: state).write(to: file, options: .atomic)
    stage("recover_profiles")
    _ = try ClientStore.profiles()
    stage("read_recovered_credential")
    XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, nil), errSecItemNotFound)
    XCTAssertEqual(try ClientStore.connectionOptions(id)["password"] as? String, "fixture-only")
    let recovered = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any])
    XCTAssertNil(recovered["pendingCredentialDeletes"])
    if !recordedIssue { appendValidation("case=interrupted_write_cleanup result=passed") }
  }

  func testCredentialRemainsNativeAndCannotFollowAnEndpointChange() throws {
    beginCase("credential_endpoint_binding")
    let id = UUID().uuidString.lowercased()
    let profile: [String: Any] = ["id": id, "name": "Fixture", "host": "fixture.invalid",
      "port": 22, "username": "fixture", "authMethod": "password"]
    defer { try? ClientStore.deleteProfile(id) }
    stage("save_initial_profile")
    let saved = try ClientStore.saveProfile(profile,
      credential: ["authMethod": "password", "password": "  fixture-only  "], keepCredential: false)
    XCTAssertEqual(saved["credentialSaved"] as? Bool, true)
    XCTAssertNil(saved["password"])
    XCTAssertNil(saved["credentialID"])
    stage("list_profile")
    let listed = try XCTUnwrap(ClientStore.profiles().first { $0["id"] as? String == id })
    XCTAssertNil(listed["password"])
    XCTAssertNil(listed["credentialID"])
    stage("read_connection_options")
    let connection = try ClientStore.connectionOptions(id)
    XCTAssertEqual(connection["password"] as? String, "  fixture-only  ")
    var renamed = profile
    renamed["name"] = "Renamed fixture"
    stage("rename_profile")
    let retained = try ClientStore.saveProfile(renamed, credential: nil, keepCredential: true)
    XCTAssertEqual(retained["credentialSaved"] as? Bool, true)
    renamed["host"] = "different.invalid"
    stage("reject_endpoint_change")
    let changed = try ClientStore.saveProfile(renamed, credential: nil, keepCredential: true)
    XCTAssertEqual(changed["credentialSaved"] as? Bool, false)
    XCTAssertThrowsError(try ClientStore.connectionOptions(id))
    if !recordedIssue { appendValidation("case=credential_endpoint_binding result=passed") }
  }

  func testRemovingSavedCredentialAndProfile() throws {
    beginCase("remove_saved_credential")
    let id = UUID().uuidString.lowercased()
    let profile: [String: Any] = ["id": id, "name": "Fixture key", "host": "fixture.invalid",
      "port": 22, "username": "fixture", "authMethod": "publicKey"]
    defer { try? ClientStore.deleteProfile(id) }
    stage("save_key_profile")
    _ = try ClientStore.saveProfile(profile, credential: ["authMethod": "publicKey",
      "privateKey": "fixture-only-key", "passphrase": "fixture-only-passphrase"], keepCredential: false)
    stage("read_key_credential")
    XCTAssertEqual(try ClientStore.connectionOptions(id)["privateKey"] as? String, "fixture-only-key")
    stage("forget_credential")
    let forgotten = try ClientStore.saveProfile(profile, credential: nil, keepCredential: false)
    XCTAssertEqual(forgotten["credentialSaved"] as? Bool, false)
    XCTAssertThrowsError(try ClientStore.connectionOptions(id))
    stage("delete_profile")
    try ClientStore.deleteProfile(id)
    stage("list_after_delete")
    XCTAssertFalse(try ClientStore.profiles().contains { $0["id"] as? String == id })
    if !recordedIssue { appendValidation("case=remove_saved_credential result=passed") }
  }

  func testPreferencesRoundTripAndValidation() throws {
    beginCase("preferences_validation")
    stage("read_preferences")
    let old = try ClientStore.preferences()
    defer { try? ClientStore.setPreferences(old) }
    let updated: [String: Any] = ["fontSize": 20, "theme": "light", "scrollbackLines": 20000, "automaticReconnect": false]
    stage("write_preferences")
    try ClientStore.setPreferences(updated)
    stage("read_updated_preferences")
    let actual = try ClientStore.preferences()
    XCTAssertEqual(actual["fontSize"] as? Int, 20)
    XCTAssertEqual(actual["theme"] as? String, "light")
    XCTAssertEqual(actual["scrollbackLines"] as? Int, 20000)
    XCTAssertEqual(actual["automaticReconnect"] as? Bool, false)
    let invalidValues: [(String, Any)] = [("fontSize", 25), ("fontSize", true),
      ("scrollbackLines", 50001), ("theme", "bad"), ("automaticReconnect", 1)]
    stage("validate_invalid_preferences")
    for (field, value) in invalidValues {
      var invalid = updated
      invalid[field] = value
      XCTAssertThrowsError(try ClientStore.setPreferences(invalid))
    }
    if !recordedIssue { appendValidation("case=preferences_validation result=passed") }
  }
}
