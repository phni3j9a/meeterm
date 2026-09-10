import XCTest
import Security

final class RunnerKeychainTests: XCTestCase {
    func testRunnerKeychainAccess() {
        let account = "probe-\(UUID().uuidString)"
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "dev.meeterm.runner-probe",
            kSecAttrAccount as String: account,
            kSecAttrSynchronizable as String: false,
        ]
        var item = query
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        item[kSecValueData as String] = Data("public-fixture-only".utf8)

        let addStatus = SecItemAdd(item as CFDictionary, nil)
        let deleteStatus = SecItemDelete(query as CFDictionary)
        print("PROBE runner_keychain_add=\(addStatus)")
        print("PROBE runner_keychain_delete=\(deleteStatus)")
        XCTAssertEqual(addStatus, errSecSuccess)
        XCTAssertEqual(deleteStatus, errSecSuccess)
    }
}
