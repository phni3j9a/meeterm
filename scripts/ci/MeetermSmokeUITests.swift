import Foundation
import UIKit
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
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-failure.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-form-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-connection-state.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-host-trust-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("host-trust-timeout.png")
    )
    record("test_started")

    app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
    app.launch()
    XCTAssertTrue(
      app.wait(for: .runningForeground, timeout: 60),
      "The meeterm app did not reach the foreground."
    )
    record("app_launched")
  }

  override func tearDownWithError() throws {
    UIPasteboard.general.string = nil
  }

  override func record(_ issue: XCTIssue) {
    // XCTest keeps the complete issue, including the text entered with
    // typeText, in its result bundle. Emit only a fixed diagnostic category to
    // the uploaded observability directory; super.record keeps the raw result
    // under RUNNER_TEMP for XCTest's normal reporting.
    let description = issue.compactDescription
    let category: String
    if description.localizedCaseInsensitiveContains("multiple matching") {
      category = "multiple_matching"
    } else if description.localizedCaseInsensitiveContains("no matches found") {
      category = "no_matches_found"
    } else if description.localizedCaseInsensitiveContains("hit point")
      || description.localizedCaseInsensitiveContains("hittable") {
      category = "hittability"
    } else {
      category = "unknown"
    }
    let line = "source_line=\(issue.sourceCodeContext.location?.lineNumber ?? 0)\ncategory=\(category)\n"
    let data = Data(line.utf8)
    let path = artifactDirectory.appendingPathComponent("ios-ui-failure.txt")
    if let handle = try? FileHandle(forWritingTo: path) {
      handle.seekToEndOfFile()
      handle.write(data)
      try? handle.close()
    } else {
      try? data.write(to: path, options: .atomic)
    }
    super.record(issue)
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
    let submit = app.buttons["ssh-submit"]
    record("submit_connect_exists")
    XCTAssertTrue(submit.waitForExistence(timeout: 10), "The Connect action is unavailable.")
    record("submit_connect_hittable")
    XCTAssertTrue(waitForHittable(submit, timeout: 10), "The Connect action is not hittable.")
    record("submit_connect_tap")
    submit.tap()

    record("await_connection_form_dismissed")
    if !waitForConnectionFormDismissal(timeout: 10) {
      record("form_not_dismissed")
      writeConnectionFormDiagnostics()
      XCTFail("The SSH connection form did not dismiss after Connect.")
      return
    }
    record("connection_form_dismissed")
    recordConnectionState()

    record("await_host_trust_prompt")
    let alert = app.alerts.firstMatch
    guard alert.waitForExistence(timeout: 60) else {
      record("host_trust_timeout")
      writeHostTrustTimeoutDiagnostics()
      XCTFail("The SSH host trust prompt did not appear.")
      return
    }
    let expectedFingerprint = requiredEnvironment("MEETERM_SSH_FINGERPRINT")
    record("verify_host_fingerprint")
    XCTAssertTrue(
      alert.staticTexts.allElementsBoundByIndex.contains {
        $0.label.contains(expectedFingerprint)
      },
      "The host trust prompt did not display the fixture fingerprint."
    )
    record("guard_host_trust_capture")
    guard safeForPostFormScreenshot() else {
      record("host_trust_capture_blocked")
      XCTFail("The host trust screenshot was blocked because the form or app is not safe.")
      return
    }
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
    enterTerminalCommand(markerCommand, stage: "send_terminal_input")
    XCTAssertTrue(
      waitForMarkerLines([markerValue]),
      "Native terminal input did not reach the fixture pane."
    )

    record("send_handoff_variable")
    let handoffCommand = "export MEETERM_IOS_HANDOFF='\(handoffValue)'; printf '%s\\n' \"$MEETERM_IOS_HANDOFF\" >> \(shellQuote(markerPath.path))"
    enterTerminalCommand(handoffCommand, stage: "send_handoff_variable")
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
    enterTerminalCommand(resumeCommand, stage: "verify_remote_shell_after_reconnect")
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
    if label == "Port" { record("fill_port_focus") }
    field.tap()
    if clearExistingCharacters > 0 {
      // The port uses iOS's number-pad, whose delete key is not exposed
      // consistently through app.keys. Send the key to the focused field so
      // replacement does not depend on the keyboard's accessibility label.
      if label == "Port" { record("fill_port_clear") }
      field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: clearExistingCharacters))
    }
    if label == "Port" { record("fill_port_type") }
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
    if buttons.count > 0 {
      for index in 0..<buttons.count {
        let candidate = buttons.element(boundBy: index)
        if candidate.exists && candidate.isHittable { return candidate }
      }
      return buttons.element(boundBy: buttons.count - 1)
    }
    let descendant = app.descendants(matching: .any)[label]
    return descendant
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
    // React Native's tab role need not be exposed as an XCTest button.
    let query = app.descendants(matching: .any).matching(predicate)
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

  private func waitForConnectionFormDismissal(timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if !app.buttons["ssh-submit"].exists { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return !app.buttons["ssh-submit"].exists
  }

  private func connectionFormIsGone() -> Bool {
    let host = app.descendants(matching: .any)["Host"]
    let privateKey = app.descendants(matching: .any)["Private OpenSSH key"]
    return !app.buttons["ssh-submit"].exists && !host.exists && !privateKey.exists
  }

  private func safeForPostFormScreenshot() -> Bool {
    app.state == .runningForeground && connectionFormIsGone()
  }

  private func writeConnectionFormDiagnostics() {
    // Query only the exact, fixed validation strings from ConnectionForm. The
    // uploaded file contains flags, never the entered host, username, key, or
    // any XCTest description.
    let validations: [(String, String)] = [
      ("host", "空白を含まないホスト名か IP アドレスを入力してください。"),
      ("port", "1〜65535 の数字を入力してください。"),
      ("username", "SSH のユーザー名を入力してください。空白は使えません。"),
      ("private_key", "BEGIN と END の行を含む OpenSSH 形式の秘密鍵を貼り付けてください。"),
    ]
    let lines = validations.map { name, message in
      "\(name)_validation_error_visible=\(app.staticTexts[message].exists ? 1 : 0)"
    } + [
      "host_field_visible=\(app.descendants(matching: .any)["Host"].exists ? 1 : 0)",
      "private_key_field_visible=\(app.descendants(matching: .any)["Private OpenSSH key"].exists ? 1 : 0)",
      "submit_visible=\(app.buttons["ssh-submit"].exists ? 1 : 0)",
      "app_foreground=\(app.state == .runningForeground ? 1 : 0)",
      "form_dismissed=0",
    ]
    writeFixedArtifact("ios-ui-form-diagnostics.txt", lines: lines)
  }

  private func recordConnectionState() {
    let observation = connectionStateObservation()
    writeFixedArtifact(
      "ios-ui-connection-state.txt",
      lines: [
        "connection_state=\(observation.key)",
        "connection_state_label_present=\(observation.present ? 1 : 0)",
      ]
    )
  }

  private func connectionStateObservation() -> (key: String, present: Bool) {
    let states: [(String, String)] = [
      ("connecting", "Connecting…"),
      ("verify_host_key", "Verify host key"),
      ("authenticating", "Authenticating…"),
      ("opening_terminal", "Opening terminal…"),
      ("opening_workspace", "Opening workspace…"),
      ("restoring_terminals", "Restoring terminals…"),
      ("connected", "Connected"),
      ("reconnecting", "Reconnecting…"),
      ("disconnecting", "Disconnecting…"),
      ("connection_failed", "Connection failed"),
      ("not_connected", "Not connected"),
    ]
    guard let observed = states.first(where: { _, label in app.staticTexts[label].exists })?.0 else {
      return ("unavailable", false)
    }
    return (observed, true)
  }

  private func writeHostTrustTimeoutDiagnostics() {
    let observation = connectionStateObservation()
    writeFixedArtifact(
      "ios-ui-host-trust-diagnostics.txt",
      lines: [
        "app_foreground=\(app.state == .runningForeground ? 1 : 0)",
        "connection_state=\(observation.key)",
        "connection_state_label_present=\(observation.present ? 1 : 0)",
        "submit_visible=\(app.buttons["ssh-submit"].exists ? 1 : 0)",
        "host_field_visible=\(app.descendants(matching: .any)["Host"].exists ? 1 : 0)",
        "private_key_field_visible=\(app.descendants(matching: .any)["Private OpenSSH key"].exists ? 1 : 0)",
        "safe_for_post_form_screenshot=\(safeForPostFormScreenshot() ? 1 : 0)",
      ]
    )
    if safeForPostFormScreenshot() {
      capture("host-trust-timeout")
    }
  }

  private func writeFixedArtifact(_ name: String, lines: [String]) {
    let contents = lines.joined(separator: "\n") + "\n"
    try? Data(contents.utf8).write(
      to: artifactDirectory.appendingPathComponent(name),
      options: .atomic
    )
  }

  private func waitForHittable(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if element.exists && element.isHittable { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return element.exists && element.isHittable
  }

  private func enterTerminalCommand(_ value: String, stage: String) {
    // Tap real keyboard keys so letter commits and Enter are exercised even
    // though the native preedit-only UITextView is hidden from accessibility.
    // Paste the remainder through the native toolbar, then require the remote
    // marker to prove that all three input paths reached the selected shell.
    let prefix = String(value.prefix { $0.isASCII && $0.isLetter })
    XCTAssertFalse(prefix.isEmpty, "The fixture command needs an ASCII word prefix.")
    record("\(stage)_keyboard_letters")
    for character in prefix {
      let key = app.keys[String(character)]
      XCTAssertTrue(key.waitForExistence(timeout: 10), "The terminal keyboard letter is unavailable.")
      key.tap()
    }

    UIPasteboard.general.string = String(value.dropFirst(prefix.count))
    defer { UIPasteboard.general.string = nil }
    record("\(stage)_paste_set")
    let paste = button("Paste")
    record("\(stage)_paste_exists")
    XCTAssertTrue(paste.waitForExistence(timeout: 10), "The terminal Paste action is unavailable.")
    record("\(stage)_paste_hittable")
    XCTAssertTrue(waitForHittable(paste, timeout: 10), "The terminal Paste action is not hittable.")
    record("\(stage)_paste_tap")
    paste.tap()
    allowFixturePasteIfRequested(stage: stage)
    UIPasteboard.general.string = nil

    record("\(stage)_keyboard_return")
    let enter = app.keys.matching(
      NSPredicate(format: "label ==[c] %@ OR identifier ==[c] %@", "return", "return")
    ).firstMatch
    XCTAssertTrue(enter.waitForExistence(timeout: 10), "The terminal Return key is unavailable.")
    enter.tap()
    record("\(stage)_await_remote_marker")
  }

  private func allowFixturePasteIfRequested(stage: String) {
    // The disposable test runner owns the clipboard content. iOS may ask
    // before the app's explicit native Paste action can read another app's
    // pasteboard; keep that value until this user-facing permission resolves.
    let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    let deadline = Date().addingTimeInterval(5)
    while Date() < deadline {
      for owner in [app, springboard] {
        let allow = owner.alerts.buttons["Allow Paste"]
        if allow.exists && allow.isHittable {
          record("\(stage)_allow_fixture_paste")
          allow.tap()
          return
        }
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
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
