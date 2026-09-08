import Foundation
import XCTest

/// Real SSH/tmux UI coverage for the hosted iOS Simulator job.
///
/// The fixture environment is supplied by scripts/ssh/ios-smoke.py.  This
/// test deliberately records only stage names and screenshots taken before
/// credentials are entered or after the form has closed.  XCTest's private
/// activity/result data stays in RUNNER_TEMP because `typeText` may retain
/// the strings it sends in an xcresult bundle.
final class MeetermSmokeUITests: XCTestCase {
  private let app = XCUIApplication(bundleIdentifier: "dev.meeterm.app")
  private let artifactDirectory = URL(
    fileURLWithPath: ProcessInfo.processInfo.environment["MEETERM_IOS_ARTIFACT_DIR"]
      ?? NSTemporaryDirectory(),
    isDirectory: true
  )
  private let validationPath = URL(
    fileURLWithPath: ProcessInfo.processInfo.environment["MEETERM_IOS_STAGE_PATH"]
      ?? NSTemporaryDirectory() + "/meeterm-ios-validation.txt"
  )

  private var markerPath: URL {
    URL(fileURLWithPath: requiredEnvironment("MEETERM_IOS_MARKER_PATH"))
  }

  private var markerValue: String {
    requiredEnvironment("MEETERM_IOS_MARKER_VALUE")
  }

  private var handoffValue: String {
    requiredEnvironment("MEETERM_IOS_HANDOFF_VALUE")
  }

  override func setUpWithError() throws {
    continueAfterFailure = false
    try? FileManager.default.createDirectory(
      at: artifactDirectory,
      withIntermediateDirectories: true
    )
    try? FileManager.default.removeItem(at: markerPath)
    record("test_started")

    app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
    app.launch()
    XCTAssertTrue(
      app.wait(for: .runningForeground, timeout: 60),
      "The meeterm app did not reach the foreground."
    )
    record("app_launched")
  }

  func testRealSshWorkspacePaneInputDisconnectReconnectAndHandoff() throws {
    record("open_connection_form")
    openConnectionForm()
    record("focus_connection_form_keyboard")
    let connectionHostField = input("Host")
    XCTAssertTrue(connectionHostField.waitForExistence(timeout: 10), "The Host field is unavailable.")
    connectionHostField.tap()
    record("capture_connection_form_keyboard")
    capture("connection-form-keyboard")

    let host = requiredEnvironment("MEETERM_SSH_HOST")
    let port = requiredEnvironment("MEETERM_SSH_PORT")
    let username = requiredEnvironment("MEETERM_SSH_USERNAME")
    let key = try readPrivateKey()

    record("fill_host")
    fillTextField(label: "Host", value: host)
    record("fill_port")
    fillTextField(label: "Port", value: port, clearExistingCharacters: 2)
    record("fill_username")
    fillTextField(label: "Username", value: username)
    record("fill_private_key")
    fillPrivateKey(key)

    record("submit_connect")
    let submit = button("Connect")
    XCTAssertTrue(submit.waitForExistence(timeout: 10), "The Connect action is unavailable.")
    submit.tap()

    record("await_host_trust_prompt")
    let alert = app.alerts.firstMatch
    XCTAssertTrue(alert.waitForExistence(timeout: 60), "The SSH host trust prompt did not appear.")
    let expectedFingerprint = requiredEnvironment("MEETERM_SSH_FINGERPRINT")
    record("verify_host_fingerprint")
    XCTAssertTrue(
      alert.label.contains(expectedFingerprint),
      "The host trust prompt did not display the fixture fingerprint."
    )
    record("capture_host_trust")
    capture("host-trust")
    record("trust_host_key")
    alert.buttons["Trust and connect"].tap()

    record("await_connected")
    let connected = app.staticTexts["Connected"]
    XCTAssertTrue(
      connected.waitForExistence(timeout: 90),
      "The native SSH/tmux connection did not reach Connected."
    )

    record("await_workspaces")
    let workspaceLabels = waitForWorkspaceLabels(minimum: 2)
    XCTAssertGreaterThanOrEqual(workspaceLabels.count, 2, "The fixture workspaces were not discovered.")
    record("capture_workspaces")
    capture("workspaces")

    // The app may keep the workspace list visible or may open a workspace
    // automatically. Support both product presentations through the same
    // stable Workspace <tmux window name> accessibility contract.
    let firstWorkspace = workspaceLabels[0]
    let secondWorkspace = workspaceLabels[1]
    record("open_first_workspace")
    if button(firstWorkspace).exists {
      button(firstWorkspace).tap()
    }
    XCTAssertTrue(waitForTerminal(), "The first workspace did not open a terminal.")

    record("open_second_workspace_picker")
    if !button(secondWorkspace).exists {
      let switchWorkspace = button("Switch workspace")
      XCTAssertTrue(switchWorkspace.waitForExistence(timeout: 15), "The workspace switcher is unavailable.")
      switchWorkspace.tap()
    }
    record("select_second_workspace")
    XCTAssertTrue(button(secondWorkspace).waitForExistence(timeout: 20), "The second workspace is unavailable in the picker.")
    button(secondWorkspace).tap()
    XCTAssertTrue(waitForTerminal(), "The second workspace did not open a terminal.")
    record("capture_workspace_switched")
    capture("workspace-switched")
    record("restore_first_workspace")
    if button("Switch workspace").exists {
      button("Switch workspace").tap()
      XCTAssertTrue(button(firstWorkspace).waitForExistence(timeout: 20), "The first workspace is unavailable in the picker.")
      button(firstWorkspace).tap()
      XCTAssertTrue(waitForTerminal(), "The first workspace could not be restored.")
    }

    let paneLabels = waitForPaneLabels(minimum: 2)
    XCTAssertGreaterThanOrEqual(paneLabels.count, 2, "The fixture panes were not discovered.")
    let initialPane = paneLabels[0]
    let switchedPane = paneLabels.first(where: { $0 != initialPane }) ?? paneLabels[1]
    record("select_second_pane")
    button(switchedPane).tap()
    XCTAssertTrue(waitForSelected(button(switchedPane)), "The second pane was not selected.")
    record("capture_pane_switched")
    capture("pane-switched")

    record("focus_terminal_keyboard")
    let terminal = try terminalElement()
    terminal.tap()
    record("capture_terminal_keyboard")
    capture("terminal-keyboard")

    record("send_terminal_input")
    let markerCommand = "printf '%s\\n' '\(markerValue)' > \(shellQuote(markerPath.path))"
    typeTerminal(markerCommand)
    XCTAssertTrue(
      waitForMarkerLines([markerValue]),
      "Native terminal input did not reach the fixture pane."
    )

    record("send_handoff_variable")
    let handoffCommand = "export MEETERM_IOS_HANDOFF='\(handoffValue)'; printf '%s\\n' \"$MEETERM_IOS_HANDOFF\" >> \(shellQuote(markerPath.path))"
    typeTerminal(handoffCommand)
    XCTAssertTrue(
      waitForMarkerLines([markerValue, handoffValue]),
      "The fixture shell did not retain the handoff variable."
    )

    record("disconnect")
    tapConnectionAction("Disconnect")
    XCTAssertTrue(
      waitForDisconnected(),
      "The app did not expose the disconnected state after Disconnect."
    )
    record("capture_disconnected")
    capture("disconnected")

    record("reconnect")
    tapConnectionAction("Reconnect")
    XCTAssertTrue(
      connected.waitForExistence(timeout: 90),
      "The app did not reconnect to the fixture."
    )

    record("close_reconnect_sheet")
    let reconnectSheetClose = button("Close sheet")
    if reconnectSheetClose.waitForExistence(timeout: 5) {
      reconnectSheetClose.tap()
    }

    // Re-open the workspace if the reconnect presentation returned to the
    // workspace list, then require the exact pane identity to still exist.
    record("restore_workspace_after_reconnect")
    if button(firstWorkspace).exists {
      button(firstWorkspace).tap()
      XCTAssertTrue(waitForTerminal(), "The workspace did not reopen after reconnect.")
    }
    record("restore_pane_after_reconnect")
    let resumedPane = button(switchedPane)
    XCTAssertTrue(resumedPane.waitForExistence(timeout: 30), "The selected pane identity changed after reconnect.")
    resumedPane.tap()
    XCTAssertTrue(waitForSelected(resumedPane), "The selected pane was not restored after reconnect.")

    record("verify_remote_shell_after_reconnect")
    let resumedTerminal = try terminalElement()
    resumedTerminal.tap()
    let resumeCommand = "test \"$MEETERM_IOS_HANDOFF\" = '\(handoffValue)' && printf '%s\\n' '\(handoffValue)' >> \(shellQuote(markerPath.path))"
    typeTerminal(resumeCommand)
    XCTAssertTrue(
      waitForMarkerLines([markerValue, handoffValue, handoffValue]),
      "The reconnect did not resume the original remote shell."
    )
    record("capture_reconnected")
    capture("reconnected")

    record("verify_app_foreground")
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 5), "The app left the foreground during the smoke.")
    // End the mobile side before the shell-level handoff check. The fixture
    // then proves that ordinary tmux attach can continue the same session.
    record("terminate_app_for_handoff")
    app.terminate()
  }

  private func requiredEnvironment(_ name: String) -> String {
    guard let value = ProcessInfo.processInfo.environment[name], !value.isEmpty else {
      XCTFail("Missing iOS smoke environment value: \(name)")
      return ""
    }
    return value
  }

  private func record(_ stage: String) {
    let line = stage + "\n"
    guard let data = line.data(using: .utf8) else { return }
    if FileManager.default.fileExists(atPath: validationPath.path) {
      if let handle = try? FileHandle(forWritingTo: validationPath) {
        handle.seekToEndOfFile()
        handle.write(data)
        try? handle.close()
      }
    } else {
      try? data.write(to: validationPath, options: .atomic)
    }
  }

  private func capture(_ name: String) {
    let data = XCUIScreen.main.screenshot().pngRepresentation
    try? data.write(
      to: artifactDirectory.appendingPathComponent(name + ".png"),
      options: .atomic
    )
  }

  private func openConnectionForm() {
    if button("Connect").waitForExistence(timeout: 20) {
      button("Connect").tap()
    } else if button("Server connection").waitForExistence(timeout: 10) {
      button("Server connection").tap()
      XCTAssertTrue(button("Connect").waitForExistence(timeout: 10), "The connection menu did not open.")
      button("Connect").tap()
    } else {
      XCTFail("The connection entry point is unavailable.")
      return
    }
    XCTAssertTrue(input("Host").waitForExistence(timeout: 15), "The SSH connection form did not open.")
    record("connection_form_opened")
  }

  private func fillTextField(label: String, value: String, clearExistingCharacters: Int = 0) {
    let field = input(label)
    XCTAssertTrue(field.waitForExistence(timeout: 10), "The \(label) field is unavailable.")
    field.tap()
    if clearExistingCharacters > 0 {
      for _ in 0..<clearExistingCharacters {
        app.keys["delete"].tap()
      }
    }
    field.typeText(value)
  }

  private func fillPrivateKey(_ value: String) {
    let field = input("Private OpenSSH key")
    for _ in 0..<8 where !field.isHittable {
      app.scrollViews.firstMatch.swipeUp()
    }
    XCTAssertTrue(field.waitForExistence(timeout: 15), "The private-key field is unavailable.")
    field.tap()
    let lines = value.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false)
    for (index, line) in lines.enumerated() {
      if !line.isEmpty {
        field.typeText(String(line))
      }
      if index < lines.count - 1 {
        if app.keys["return"].exists {
          app.keys["return"].tap()
        } else {
          field.typeText("\n")
        }
      }
    }
  }

  private func readPrivateKey() throws -> String {
    let path = requiredEnvironment("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
    let data = try Data(contentsOf: URL(fileURLWithPath: path))
    let key = String(decoding: data, as: UTF8.self)
      .trimmingCharacters(in: .whitespacesAndNewlines)
    XCTAssertTrue(
      key.hasPrefix("-----BEGIN OPENSSH PRIVATE KEY-----") && key.hasSuffix("-----END OPENSSH PRIVATE KEY-----"),
      "The fixture private key is not an OpenSSH key."
    )
    return key
  }

  private func input(_ label: String) -> XCUIElement {
    let candidates = [
      app.textFields[label],
      app.textViews[label],
      app.secureTextFields[label],
      app.descendants(matching: .any)[label],
    ]
    return candidates.first(where: { $0.exists }) ?? app.descendants(matching: .any)[label]
  }

  private func button(_ label: String) -> XCUIElement {
    // React Native exposes these contracts as accessibility labels. Match
    // either label or identifier so the test remains stable across iOS
    // versions that populate XCUIElementQuery's subscript differently.
    let buttons = app.buttons.matching(
      NSPredicate(format: "identifier == %@ OR label == %@", label, label)
    )
    if buttons.count > 0 { return buttons.element(boundBy: buttons.count - 1) }
    return app.descendants(matching: .any)[label]
  }

  private func terminalElement() throws -> XCUIElement {
    let terminal = app.otherElements["Terminal"]
    XCTAssertTrue(terminal.waitForExistence(timeout: 30), "The native terminal view is unavailable.")
    return terminal
  }

  private func waitForTerminal() -> Bool {
    app.otherElements["Terminal"].waitForExistence(timeout: 30)
  }

  private func waitForWorkspaceLabels(minimum: Int) -> [String] {
    let predicate = NSPredicate(format: "label BEGINSWITH 'Workspace '")
    let query = app.buttons.matching(predicate)
    let deadline = Date().addingTimeInterval(90)
    while Date() < deadline {
      let labels = (0..<query.count).compactMap { index -> String? in
        let label = query.element(boundBy: index).label
        return label.isEmpty ? nil : label
      }
      if Set(labels).count >= minimum {
        return Array(Set(labels)).sorted()
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return (0..<query.count).compactMap { index in
      let label = query.element(boundBy: index).label
      return label.isEmpty ? nil : label
    }
  }

  private func waitForPaneLabels(minimum: Int) -> [String] {
    let predicate = NSPredicate(format: "label BEGINSWITH 'Terminal %'")
    let query = app.buttons.matching(predicate)
    let deadline = Date().addingTimeInterval(60)
    while Date() < deadline {
      let labels = (0..<query.count).compactMap { index -> String? in
        let label = query.element(boundBy: index).label
        return label.isEmpty ? nil : label
      }
      if Set(labels).count >= minimum {
        return Array(Set(labels)).sorted()
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return (0..<query.count).compactMap { index in
      let label = query.element(boundBy: index).label
      return label.isEmpty ? nil : label
    }
  }

  private func waitForSelected(_ element: XCUIElement) -> Bool {
    let deadline = Date().addingTimeInterval(30)
    while Date() < deadline {
      if element.isSelected { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return element.isSelected
  }

  private func typeTerminal(_ value: String) {
    app.typeText(value + "\n")
  }

  private func waitForDisconnected() -> Bool {
    let deadline = Date().addingTimeInterval(60)
    while Date() < deadline {
      if app.staticTexts["Not connected"].exists || button("Reconnect").exists {
        return true
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return false
  }

  private func tapConnectionAction(_ label: String) {
    if button(label).waitForExistence(timeout: 30) {
      button(label).tap()
      return
    }
    if button("Server connection").waitForExistence(timeout: 10) {
      button("Server connection").tap()
      XCTAssertTrue(button(label).waitForExistence(timeout: 10), "The \(label) action is unavailable in the connection menu.")
      button(label).tap()
      return
    }
    if button("Terminal menu").waitForExistence(timeout: 10) {
      button("Terminal menu").tap()
      XCTAssertTrue(button(label).waitForExistence(timeout: 10), "The \(label) action is unavailable in the terminal menu.")
      button(label).tap()
      return
    }
    XCTFail("The \(label) action is unavailable.")
  }

  private func waitForMarkerLines(_ expected: [String]) -> Bool {
    let deadline = Date().addingTimeInterval(30)
    while Date() < deadline {
      if let contents = try? String(contentsOf: markerPath, encoding: .utf8) {
        let lines = contents.split(whereSeparator: { $0.isNewline }).map(String.init)
        if lines == expected { return true }
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return false
  }

  private func shellQuote(_ value: String) -> String {
    "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
  }
}
