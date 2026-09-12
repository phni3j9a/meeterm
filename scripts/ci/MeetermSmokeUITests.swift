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
  private enum HostTrustFailurePhase: String {
    case buttonMissing = "button_missing"
    case buttonNotHittable = "button_not_hittable"
    case alertNotDismissed = "alert_not_dismissed"
    case connectedTimeout = "connected_timeout"
  }

  private enum HostSelectionCopyResult {
    case passed
    case rejected
    case timedOut
  }

  private let testStartedAt = ProcessInfo.processInfo.systemUptime
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

  private var selectionCopyRequestPath: URL {
    URL(fileURLWithPath: markerPath.path + ".selection-copy-request")
  }

  private var selectionCopyResultPath: URL {
    URL(fileURLWithPath: markerPath.path + ".selection-copy-result")
  }

  private var selectionCopyRequestToken: String {
    markerValue + "-selection-copy-request\n"
  }

  private var selectionCopyPassedToken: String {
    markerValue + "-selection-copy-passed\n"
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
    let setupEpochMilliseconds = Int(Date().timeIntervalSince1970 * 1000)
    let setupElapsedMilliseconds = Int((ProcessInfo.processInfo.systemUptime - testStartedAt) * 1000)
    writeFixedArtifact(
      "ios-ui-clock.txt",
      lines: [
        "setup_epoch_ms=\(setupEpochMilliseconds)",
        "setup_elapsed_ms=\(setupElapsedMilliseconds)",
      ]
    )
    try? FileManager.default.removeItem(at: markerPath)
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-failure.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-form-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-short-field-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-terminal-keyboard-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("terminal-keyboard-failure.png")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-connection-state.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-host-trust-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-host-trust-response-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("host-trust-timeout.png")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("host-trust-response-failure.png")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-timing.txt")
    )
    for name in [
      "ios-ui-standard-validation.txt",
      "ios-ui-ssh-validation.txt",
      "standard-home.png",
      "standard-servers.png",
      "standard-connection.png",
      "standard-password.png",
      "standard-workspaces.png",
      "standard-terminal.png",
      "standard-settings.png",
      "standard-workspace-name.png",
      "standard-terminal-name.png",
      "standard-handoff.png",
      "standard-herdr-connection.png",
      "standard-herdr-groups.png",
      "standard-herdr-terminal.png",
      "standard-herdr-workspaces.png",
    ] {
      try? FileManager.default.removeItem(at: artifactDirectory.appendingPathComponent(name))
    }
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
    record("teardown_started")
    UIPasteboard.general.string = nil
    record("teardown_complete")
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
    try runRealSshWorkflow(namesOnly: false)
  }

  func testRealSshNameOperations() throws {
    try runRealSshWorkflow(namesOnly: true)
  }

  private func runRealSshWorkflow(namesOnly: Bool) throws {
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
    fillTextField(label: "Port", value: port)
    record("fill_username")
    fillTextField(label: "Username", value: username)
    if namesOnly {
      record("names_public_key_auth")
      XCTAssertTrue(
        revealAuthenticationControl(input("Private OpenSSH key"), stage: "names_key"),
        "Private-key authentication is unavailable."
      )
    } else {
      verifyPasswordForm()
      configureSavedFixtureProfile()
    }
    record("fill_private_key")
    fillPrivateKey(key)
    record("pre_submit_public_fields")
    let expectedProfileName = namesOnly ? "" : "Daily fixture"
    let profileNameMatches = namesOnly
      ? waitForShortFieldValue(input("Server name"), expected: expectedProfileName, timeout: 5)
      : shortFieldValue(input("Server name")) == expectedProfileName
    if !namesOnly {
      // A public-field check catches focus leaking back into the profile editor
      // without publishing any field contents after secret entry.
      appendFixedArtifact("ios-ui-form-diagnostics.txt", lines: [
        "profile_name_unchanged_after_key=\(profileNameMatches ? 1 : 0)",
      ])
      XCTAssertTrue(profileNameMatches, "The saved-server name changed during private-key entry.")
    }
    let hostMatches = waitForShortFieldValue(input("Host"), expected: host, timeout: 5)
    let portMatches = waitForShortFieldValue(input("Port"), expected: port, timeout: 5)
    let usernameMatches = waitForShortFieldValue(input("Username"), expected: username, timeout: 5)
    let publicFieldsVerified = hostMatches && portMatches && usernameMatches && profileNameMatches
    var preSubmitLines = [
      "pre_submit_host_matches=\(hostMatches ? 1 : 0)",
      "pre_submit_port_matches=\(portMatches ? 1 : 0)",
      "pre_submit_username_matches=\(usernameMatches ? 1 : 0)",
      "pre_submit_public_fields_verified=\(publicFieldsVerified ? 1 : 0)",
    ]
    if namesOnly {
      preSubmitLines.append("names_profile_name_blank=\(profileNameMatches ? 1 : 0)")
    }
    appendFixedArtifact("ios-ui-form-diagnostics.txt", lines: preSubmitLines)
    guard publicFieldsVerified else {
      record("pre_submit_public_fields_failed")
      XCTFail("The connection form public fields changed before Connect.")
      return
    }
    record("pre_submit_public_fields_verified")

    record("submit_connect")
    let submit = app.buttons["ssh-submit"]
    record("submit_connect_exists")
    XCTAssertTrue(submit.waitForExistence(timeout: 10), "The Connect action is unavailable.")
    record("submit_connect_hittable")
    XCTAssertTrue(waitForHittable(submit, timeout: 10), "The Connect action is not hittable.")
    record("submit_connect_tap")
    submit.tap()

    record("await_connection_form_dismissed")
    // Credential persistence now completes before the form closes. Keep a
    // bounded wait for the asynchronous native save and connection command.
    if !waitForConnectionFormDismissal(timeout: 30) {
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
    let trust = alert.buttons["Trust and connect"]
    record("await_host_trust_button")
    guard trust.waitForExistence(timeout: 10) else {
      record("host_trust_button_missing")
      writePostTrustDiagnostics(phase: .buttonMissing, alert: alert, trust: trust)
      XCTFail("The SSH host trust action is unavailable.")
      return
    }
    record("await_host_trust_button_hittable")
    guard waitForHittable(trust, timeout: 10) else {
      record("host_trust_button_not_hittable")
      writePostTrustDiagnostics(phase: .buttonNotHittable, alert: alert, trust: trust)
      XCTFail("The SSH host trust action is not hittable.")
      return
    }
    record("trust_host_key")
    trust.tap()
    record("trust_host_key_tapped")
    record("await_host_trust_dismissed")
    guard waitForDisappearance(alert, timeout: 10) else {
      record("host_trust_not_dismissed")
      writePostTrustDiagnostics(phase: .alertNotDismissed, alert: alert, trust: trust)
      XCTFail("The SSH host trust prompt did not dismiss after tapping Trust and connect.")
      return
    }
    record("host_trust_dismissed")

    record("await_connected")
    let connected = app.staticTexts["Connected"]
    guard connected.waitForExistence(timeout: 90) else {
      record("connected_timeout_after_host_trust")
      writePostTrustDiagnostics(phase: .connectedTimeout, alert: alert, trust: trust)
      XCTFail("The native SSH/tmux connection did not reach Connected.")
      return
    }

    record("await_workspaces")
    let workspaceLabels = waitForWorkspaceLabels(minimum: 2)
    XCTAssertEqual(workspaceLabels.count, 2, "The fixture must expose two workspace rows, excluding their options buttons.")
    record("capture_workspaces")
    capture("workspaces")

    if namesOnly {
      if button("Back to workspaces").exists {
        button("Back to workspaces").tap()
      }
      record("names_started")
      verifyNameOperations()
      writeFixedArtifact("ios-ui-names-validation.txt", lines: ["case=names result=passed"])
      record("names_complete")
      app.terminate()
      return
    }

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
    let markerReached = waitForMarkerLines([markerValue])
    XCTAssertTrue(
      markerReached,
      "Native terminal input did not reach the fixture pane."
    )
    if markerReached {
      record("capture_terminal_input")
      capture("terminal-input")
    }

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

    try verifyDailyUse(firstWorkspace: firstWorkspace, pane: switchedPane)

    record("verify_app_foreground")
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 5), "The app left the foreground during the smoke.")
    // End the mobile side before the shell-level handoff check. The fixture
    // then proves that ordinary tmux attach can continue the same session.
    record("terminate_app_for_handoff")
    app.terminate()
    try verifyFoundationRelaunch()
  }

  /// Opens each production screen from public deterministic state. This is a
  /// presentation contract only: no saved metadata or remote session is
  /// created while preparing these screenshots.
  func testStandardSeededScreensAndFoundation() throws {
    let screens = [
      "home", "servers", "connection", "password", "workspaces", "terminal",
      "settings", "workspace-name", "terminal-name", "handoff",
      "herdr-connection", "herdr-groups", "herdr-terminal", "herdr-workspaces",
    ]
    for screen in screens {
      record("standard_screen_\(screen)_open")
      guard let url = URL(string: "meeterm://smoke?screen=\(screen)") else {
        XCTFail("The standard smoke URL could not be created.")
        return
      }
      app.open(url)
      XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30), "The app left the foreground while opening \(screen).")
      guard waitForStandardScreen(screen) else {
        XCTFail("The standard smoke screen did not open: \(screen).")
        return
      }
      // NameForm and the connection forms can make the Simulator offer its
      // one-time QuickPath prompt. Dismiss only that exact prompt before the
      // safe, public screenshot; never tap a generic Continue action.
      dismissQuickPathTutorialIfPresent(stage: "standard_\(screen)")
      record("standard_screen_\(screen)_ready")
      capture("standard-\(screen)")
      record("standard_screen_\(screen)_captured")
    }

    // Keep the existing foundation check as a genuinely fresh process after
    // the seeded screen pass. The foundation uses the Rust poc-main fixture.
    app.terminate()
    try verifyFoundationRelaunch()
    writeFixedArtifact("ios-ui-standard-validation.txt", lines: ["case=standard result=passed"])
    record("standard_complete")
  }

  /// A bounded real SSH round trip. The private key is entered through the
  /// existing production form, host-key verification remains explicit, and
  /// one command proves native terminal input reached the fixture shell.
  func testShortSshInputAndDisconnect() throws {
    record("ssh_open_connection_form")
    openConnectionForm()
    let host = requiredEnvironment("MEETERM_SSH_HOST")
    let port = requiredEnvironment("MEETERM_SSH_PORT")
    let username = requiredEnvironment("MEETERM_SSH_USERNAME")
    fillTextField(label: "Host", value: host)
    fillTextField(label: "Port", value: port)
    fillTextField(label: "Username", value: username)
    let key = try readPrivateKey()
    XCTAssertTrue(
      revealAuthenticationControl(input("Private OpenSSH key"), stage: "ssh_key"),
      "Private-key authentication is unavailable."
    )
    fillPrivateKey(key)

    record("ssh_submit_connect")
    let submit = app.buttons["ssh-submit"]
    XCTAssertTrue(submit.waitForExistence(timeout: 10), "The Connect action is unavailable.")
    XCTAssertTrue(waitForHittable(submit, timeout: 10), "The Connect action is not hittable.")
    submit.tap()
    guard waitForConnectionFormDismissal(timeout: 30) else {
      writeConnectionFormDiagnostics()
      XCTFail("The SSH connection form did not dismiss.")
      return
    }
    record("ssh_connection_form_dismissed")

    guard acceptFixtureHostKey() else { return }
    let workspaces = waitForWorkspaceLabels(minimum: 1)
    guard let firstWorkspace = workspaces.first else {
      XCTFail("The fixture did not expose a workspace.")
      return
    }
    record("ssh_open_first_workspace")
    let workspaceButton = button(firstWorkspace)
    if workspaceButton.waitForExistence(timeout: 15) {
      XCTAssertTrue(waitForHittable(workspaceButton, timeout: 10), "The first workspace is not hittable.")
      workspaceButton.tap()
    }
    XCTAssertTrue(waitForTerminal(), "The first fixture workspace did not open a terminal.")

    record("ssh_native_input")
    let terminal = try terminalElement()
    terminal.tap()
    let markerCommand = "printf '%s\\n' '\(markerValue)' > \(shellQuote(markerPath.path))"
    enterTerminalCommand(markerCommand, stage: "ssh_native_input")
    XCTAssertTrue(waitForMarkerLines([markerValue]), "Native terminal input did not reach the fixture shell.")
    record("ssh_remote_ack")
    capture("ssh-terminal-input")

    record("ssh_disconnect")
    tapConnectionAction("Disconnect")
    XCTAssertTrue(app.staticTexts["Not connected"].waitForExistence(timeout: 60), "The app did not complete the explicit disconnect.")
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 5), "The app left the foreground during the SSH check.")
    capture("ssh-disconnected")
    writeFixedArtifact("ios-ui-ssh-validation.txt", lines: ["case=ssh result=passed"])
    record("ssh_complete")
    app.terminate()
  }

  private func waitForStandardScreen(_ screen: String) -> Bool {
    switch screen {
    case "home":
      let title = app.staticTexts["ワークスペース"]
      let profile = app.buttons.matching(
        NSPredicate(format: "label == %@", "Connect saved server Smoke server")
      ).firstMatch
      return title.waitForExistence(timeout: 30)
        && waitForHittable(profile, timeout: 30)
    case "servers":
      let title = app.staticTexts["保存済みサーバー"]
      let profile = app.buttons.matching(
        NSPredicate(format: "identifier == %@", "server-profile-smoke-profile")
      ).firstMatch
      return title.waitForExistence(timeout: 30)
        && waitForHittable(profile, timeout: 30)
    case "connection":
      return app.staticTexts["サーバーに接続"].waitForExistence(timeout: 30)
        && waitForHittable(input("Host"), timeout: 30)
    case "password":
      guard app.staticTexts["サーバーに接続"].waitForExistence(timeout: 30) else { return false }
      // The password field is deliberately empty but can be below the fold
      // on the iPhone simulator. Reuse the bounded, secret-free reveal path
      // used by the focused form test before calling the screen ready.
      return revealAuthenticationControl(app.secureTextFields["SSH password"], stage: "standard_password")
    case "workspaces":
      guard waitForWorkspaceLabels(minimum: 2).count >= 2 else { return false }
      let firstRow = app.buttons.matching(
        NSPredicate(format: "identifier == %@", "workspace-row-@smoke-main")
      ).firstMatch
      return waitForHittable(firstRow, timeout: 30)
    case "terminal":
      let terminal = app.otherElements["Terminal"]
      return app.staticTexts["Connected"].waitForExistence(timeout: 30)
        && waitForHittable(terminal, timeout: 30)
    case "settings":
      return app.staticTexts["ターミナル設定"].waitForExistence(timeout: 30)
        && waitForHittable(app.buttons["settings-submit"], timeout: 30)
    case "workspace-name":
      return app.staticTexts["ワークスペースの名前"].waitForExistence(timeout: 30)
        && waitForHittable(input("Workspace or terminal name"), timeout: 30)
    case "terminal-name":
      return app.staticTexts["ターミナルの名前"].waitForExistence(timeout: 30)
        && waitForHittable(input("Workspace or terminal name"), timeout: 30)
    case "handoff":
      return app.staticTexts["PC で続きを"].waitForExistence(timeout: 30)
        && waitForHittable(button("Disconnect"), timeout: 30)
    case "herdr-connection":
      let backend = button("herdr backend")
      let runtime = input("Herdr session name")
      return app.staticTexts["サーバーに接続"].waitForExistence(timeout: 30)
        && waitForHittable(backend, timeout: 30)
        && waitForSelected(backend)
        && runtime.waitForExistence(timeout: 30)
        && waitForShortFieldValue(runtime, expected: "dev", timeout: 30)
    case "herdr-groups":
      let title = app.staticTexts["Groupを切り替える"]
      let development = button("Group Development")
      let tests = button("Group Tests & review")
      return title.waitForExistence(timeout: 30)
        && development.waitForExistence(timeout: 30)
        && tests.waitForExistence(timeout: 30)
    case "herdr-terminal":
      let groupPicker = button("Switch terminal group")
      let terminal = app.otherElements["Terminal"]
      return waitForHittable(groupPicker, timeout: 30)
        && terminal.waitForExistence(timeout: 30)
        && app.staticTexts["Claude Code"].waitForExistence(timeout: 30)
        && app.staticTexts["作業中"].waitForExistence(timeout: 30)
    case "herdr-workspaces":
      let total = app.staticTexts.matching(
        NSPredicate(format: "label CONTAINS %@ AND label CONTAINS %@", "すべて", "2")
      ).firstMatch
      let mainCount = app.staticTexts["4 ターミナル"]
      let toolsCount = app.staticTexts["1 ターミナル"]
      guard waitForWorkspaceLabels(minimum: 2).count >= 2 else { return false }
      return total.waitForExistence(timeout: 30)
        && mainCount.waitForExistence(timeout: 30)
        && toolsCount.waitForExistence(timeout: 30)
    default:
      return false
    }
  }

  private func acceptFixtureHostKey() -> Bool {
    record("ssh_await_host_trust_prompt")
    let alert = app.alerts.firstMatch
    guard alert.waitForExistence(timeout: 60) else {
      writeHostTrustTimeoutDiagnostics()
      XCTFail("The SSH host trust prompt did not appear.")
      return false
    }
    let expectedFingerprint = requiredEnvironment("MEETERM_SSH_FINGERPRINT")
    XCTAssertTrue(
      alert.staticTexts.allElementsBoundByIndex.contains { $0.label.contains(expectedFingerprint) },
      "The host trust prompt did not display the fixture fingerprint."
    )
    let trust = alert.buttons["Trust and connect"]
    guard trust.waitForExistence(timeout: 10), waitForHittable(trust, timeout: 10) else {
      writePostTrustDiagnostics(phase: .buttonNotHittable, alert: alert, trust: trust)
      XCTFail("The host trust action is unavailable.")
      return false
    }
    record("ssh_trust_host_key")
    trust.tap()
    guard waitForDisappearance(alert, timeout: 10) else {
      writePostTrustDiagnostics(phase: .alertNotDismissed, alert: alert, trust: trust)
      XCTFail("The host trust prompt did not dismiss.")
      return false
    }
    let connected = app.staticTexts["Connected"]
    guard connected.waitForExistence(timeout: 90) else {
      writePostTrustDiagnostics(phase: .connectedTimeout, alert: alert, trust: trust)
      XCTFail("The native SSH/tmux connection did not reach Connected.")
      return false
    }
    record("ssh_connected")
    return true
  }

  func testConnectionFormControlsWithoutSecrets() throws {
    // Focused form coverage never starts the SSH fixture and never reads or
    // enters a private key/password. The values below exercise only public
    // field and control wiring before cancelling the dirty form.
    record("forms_open_connection")
    openConnectionForm()

    record("forms_fill_public_fields")
    fillTextField(label: "Host", value: "127.0.0.1")
    fillTextField(label: "Port", value: "22")
    fillTextField(label: "Username", value: "fixture")
    // This is a safe focused-suite frame: only the fixed public form values
    // have been entered, and the keyboard path is still visible.
    capture("forms-keyboard")

    // Reuse the full test's credential-free selector check. It reveals and
    // taps the empty password field so this focused suite covers the keyboard
    // path, then switches back without entering any secret.
    verifyPasswordForm()

    record("forms_profile_controls")
    let profileName = input("Server name")
    XCTAssertTrue(revealAuthenticationControl(profileName, stage: "forms_profile_name"))
    fillTextField(label: "Server name", value: "Focused form")
    let saveServer = app.switches["save-server-profile"]
    XCTAssertTrue(revealAuthenticationControl(saveServer, stage: "forms_save_server"))
    if saveServer.value as? String == "1" { saveServer.tap() }
    XCTAssertEqual(saveServer.value as? String, "0", "The save-server control could not be disabled.")
    saveServer.tap()
    XCTAssertEqual(saveServer.value as? String, "1", "The save-server control could not be re-enabled.")
    let saveCredentials = app.switches["save-credentials"]
    XCTAssertTrue(revealAuthenticationControl(saveCredentials, stage: "forms_save_credentials"))
    if saveCredentials.value as? String == "1" { saveCredentials.tap() }
    XCTAssertEqual(saveCredentials.value as? String, "0", "Credential saving was enabled unexpectedly.")
    // This frame contains only public fields and control state; it is useful
    // for visual review but is deliberately not a machine acceptance gate.
    capture("forms-controls")

    record("forms_cancel")
    let cancel = button("Cancel")
    XCTAssertTrue(cancel.waitForExistence(timeout: 10), "The form Cancel action is unavailable.")
    XCTAssertTrue(waitForHittable(cancel, timeout: 10), "The form Cancel action is not hittable.")
    cancel.tap()
    let discard = app.alerts.firstMatch
    XCTAssertTrue(discard.waitForExistence(timeout: 10), "The dirty form did not ask for confirmation.")
    let discardButton = discard.buttons["破棄"]
    XCTAssertTrue(discardButton.waitForExistence(timeout: 5), "The form discard action is unavailable.")
    discardButton.tap()
    XCTAssertTrue(waitForConnectionFormDismissal(timeout: 10), "The form did not dismiss after cancellation.")
    XCTAssertTrue(connectionFormIsGone(), "The cancelled connection form remains exposed.")
    record("forms_complete")
    writeFixedArtifact("ios-ui-forms-validation.txt", lines: ["case=forms result=passed"])
  }

  private func configureSavedFixtureProfile() {
    record("daily_save_credential_opt_in")
    let name = input("Server name")
    XCTAssertTrue(revealAuthenticationControl(name, stage: "profile_name"))
    fillTextField(label: "Server name", value: "Daily fixture")
    let save = app.switches["save-credentials"]
    XCTAssertTrue(revealAuthenticationControl(save, stage: "save_credentials"))
    if save.value as? String != "1" { save.tap() }
    XCTAssertEqual(save.value as? String, "1", "Credential saving was not selected.")
    let key = input("Private OpenSSH key")
    XCTAssertTrue(revealAuthenticationControl(key, stage: "return_to_key"))
  }

  private func verifyDailyUse(firstWorkspace: String, pane: String) throws {
    record("daily_cold_restart")
    app.terminate()
    record("daily_cold_launch")
    app.launch()
    record("daily_cold_launch_wait")
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30))
    record("daily_cold_servers_wait")
    // Cold launch has two button controls with this label. Keep a typed,
    // lazily evaluated query so launch-time absence cannot widen the lookup
    // to descendants and later resolve as multiple matches.
    let servers = app.buttons.matching(
      NSPredicate(
        format: "identifier == %@ OR label == %@",
        "Saved servers", "Saved servers"
      )
    ).firstMatch
    XCTAssertTrue(servers.waitForExistence(timeout: 20))
    record("daily_cold_servers_tap")
    servers.tap()
    record("daily_cold_profile_wait")
    let saved = app.buttons.matching(
      NSPredicate(
        format: "identifier BEGINSWITH %@ AND label == %@",
        "server-profile-", "Connect saved server Daily fixture"
      )
    ).firstMatch
    XCTAssertTrue(saved.waitForExistence(timeout: 20), "The saved profile did not survive process restart.")
    record("daily_cold_profile_capture")
    capture("daily-servers")
    record("daily_cold_profile_connect")
    saved.tap()
    record("daily_cold_connected_wait")
    XCTAssertTrue(app.staticTexts["Connected"].waitForExistence(timeout: 90), "The saved native credential could not reconnect.")
    XCTAssertFalse(input("Private OpenSSH key").exists, "Saved credentials must not be returned to the form.")
    XCTAssertTrue(button(firstWorkspace).waitForExistence(timeout: 20))
    button(firstWorkspace).tap()
    XCTAssertTrue(waitForTerminal())
    XCTAssertTrue(button(pane).waitForExistence(timeout: 20))
    button(pane).tap()
    XCTAssertTrue(waitForSelected(button(pane)))
    try terminalElement().tap()
    let dailyPath = markerPath.appendingPathExtension("daily")
    try? FileManager.default.removeItem(at: dailyPath)
    enterTerminalCommand("test \"$MEETERM_IOS_HANDOFF\" = '\(handoffValue)' && printf 'daily\\n' > \(shellQuote(dailyPath.path))", stage: "daily_resumed_shell")
    let marker = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
      (try? String(contentsOf: dailyPath, encoding: .utf8)) == "daily\n"
    }, object: nil)
    XCTAssertEqual(XCTWaiter.wait(for: [marker], timeout: 20), .completed)

    record("daily_selection")
    enterTerminalCommand("i=0; while [ $i -lt 80 ]; do printf 'COPY 日本語 selection %s\\n' \"$i\"; i=$((i+1)); done", stage: "daily_selection_content")
    if button("Hide keyboard").exists { button("Hide keyboard").tap() }
    let surface = try terminalElement()
    let start = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.65))
    let end = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.7, dy: 0.8))
    record("daily_selection_gesture")
    start.press(forDuration: 0.7, thenDragTo: end)
    record("daily_selection_copy_control_wait")
    let copy = button("Copy selection")
    XCTAssertTrue(copy.waitForExistence(timeout: 10))
    record("daily_selection_copy_control_ready")
    capture("daily-selection")
    record("daily_selection_clipboard_cleared")
    UIPasteboard.general.string = nil
    record("daily_selection_copy_tap")
    copy.tap()
    record("daily_selection_copy_tapped")
    XCTAssertFalse(button("Copy selection").exists)
    record("daily_selection_copy_control_cleared")
    record("daily_selection_copy_request")
    do {
      try Data(selectionCopyRequestToken.utf8).write(to: selectionCopyRequestPath, options: .atomic)
    } catch {
      XCTFail("The host clipboard validation request could not be written.")
      return
    }
    record("daily_selection_copy_result_wait")
    switch waitForSelectionCopyResult(timeout: 20) {
    case .passed:
      record("daily_selection_copy_result_verified")
    case .rejected:
      record("daily_selection_copy_result_rejected")
      XCTFail("The copied native terminal selection did not match the public fixture text.")
      return
    case .timedOut:
      record("daily_selection_copy_result_timeout")
      XCTFail("The host clipboard validation result did not arrive.")
      return
    }
    capture("daily-selection-cleared")
    record("daily_selection_clipboard_cleanup")
    UIPasteboard.general.string = nil

    record("daily_settings")
    button("Back to workspaces").tap()
    button("Terminal settings").tap()
    fillTextField(label: "Terminal font size", value: "18")
    fillTextField(label: "Scrollback lines", value: "20000")
    let theme = button("terminal-theme")
    theme.tap()
    app.buttons["ライト"].tap()
    capture("daily-settings")
    button("settings-submit").tap()
    XCTAssertTrue(button(firstWorkspace).waitForExistence(timeout: 15))
    button(firstWorkspace).tap()
    XCTAssertTrue(waitForTerminal())
    capture("daily-terminal-light")
    button("Back to workspaces").tap()
    button("Terminal settings").tap()
    XCTAssertEqual(shortFieldValue(input("Terminal font size")), "18")
    XCTAssertEqual(shortFieldValue(input("Scrollback lines")), "20000")
    fillTextField(label: "Terminal font size", value: "15")
    button("terminal-theme").tap()
    app.buttons["ダーク"].tap()
    button("settings-submit").tap()

    verifyNameOperations()
    record("daily_complete")
  }

  private func verifyNameOperations() {
    record("daily_create_workspace")
    let createWorkspace = button("Create workspace")
    record("daily_create_workspace_action_wait")
    XCTAssertTrue(createWorkspace.waitForExistence(timeout: 15))
    record("daily_create_workspace_action_ready")
    createWorkspace.tap()
    record("daily_create_workspace_action_tapped")
    record("daily_create_workspace_form")
    fillTextField(label: "Workspace or terminal name", value: "daily-smoke")
    record("daily_create_workspace_form_capture_guard")
    if safeForPostFormScreenshot() {
      capture("daily-workspace-create-form")
      record("daily_create_workspace_form_captured")
    } else {
      record("daily_create_workspace_form_capture_blocked")
    }
    let nameSubmit = button("name-submit")
    record("daily_create_workspace_submit_wait")
    XCTAssertTrue(nameSubmit.waitForExistence(timeout: 10))
    record("daily_create_workspace_submit_hittable_wait")
    XCTAssertTrue(waitForHittable(nameSubmit, timeout: 10))
    record("daily_create_workspace_submit_hittable")
    nameSubmit.tap()
    record("daily_create_workspace_submit_tapped")
    let createdWorkspace = button("Workspace daily-smoke")
    record("daily_create_workspace_row_wait")
    guard createdWorkspace.waitForExistence(timeout: 20) else {
      record("daily_create_workspace_row_timeout")
      writeDailyWorkspaceCreateDiagnostics(
        nameField: input("Workspace or terminal name"),
        submit: nameSubmit,
        expectedRow: createdWorkspace
      )
      if safeForPostFormScreenshot() {
        capture("daily-workspace-create-failure")
      } else {
        record("daily_create_workspace_failure_capture_blocked")
      }
      XCTFail("The created workspace row did not appear.")
      return
    }
    record("daily_create_workspace_row_ready")
    button("Workspace options daily-smoke").tap()
    button("名前を変更").tap()
    fillTextField(label: "Workspace or terminal name", value: "daily-renamed")
    button("name-submit").tap()
    XCTAssertTrue(button("Workspace daily-renamed").waitForExistence(timeout: 20))
    button("Workspace daily-renamed").tap()
    XCTAssertTrue(waitForTerminal())
    XCTAssertTrue(button("Create terminal").waitForExistence(timeout: 15))
    button("Create terminal").tap()
    XCTAssertGreaterThanOrEqual(waitForPaneLabels(minimum: 2).count, 2)
    button("Terminal menu").tap()
    button("Rename terminal").tap()
    fillTextField(label: "Workspace or terminal name", value: "daily-pane")
    button("name-submit").tap()
    XCTAssertTrue(waitForTerminal())
    capture("daily-created-pane")
    button("Terminal menu").tap()
    button("Refresh terminal").tap()
    XCTAssertTrue(waitForTerminal())
    button("Terminal menu").tap()
    button("Close terminal").tap()
    let closePane = app.alerts.firstMatch
    XCTAssertTrue(closePane.waitForExistence(timeout: 10))
    closePane.buttons["終了"].tap()
    let paneRemoved = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
      let tabs = self.app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'Terminal %'"))
      return Set(tabs.allElementsBoundByIndex.map { $0.label }).count == 1
    }, object: nil)
    XCTAssertEqual(XCTWaiter.wait(for: [paneRemoved], timeout: 20), .completed)
    button("Back to workspaces").tap()
    button("Workspace options daily-renamed").tap()
    button("終了").tap()
    let confirm = app.alerts.firstMatch
    XCTAssertTrue(confirm.waitForExistence(timeout: 10))
    confirm.buttons["終了"].tap()
    let removed = XCTNSPredicateExpectation(predicate: NSPredicate(format: "exists == NO"), object: button("Workspace daily-renamed"))
    XCTAssertEqual(XCTWaiter.wait(for: [removed], timeout: 20), .completed)
  }

  private func verifyPasswordForm() {
    record("password_form")
    let passwordChoice = app.descendants(matching: .any).matching(identifier: "ssh-auth-password").firstMatch
    XCTAssertTrue(passwordChoice.waitForExistence(timeout: 10), "Password authentication is unavailable.")
    XCTAssertTrue(revealAuthenticationControl(passwordChoice, stage: "password_choice"), "Password authentication cannot be selected.")
    passwordChoice.tap()
    let password = app.secureTextFields["SSH password"]
    XCTAssertTrue(password.waitForExistence(timeout: 10), "The password field is not a secure text field.")
    XCTAssertTrue(revealAuthenticationControl(password, stage: "password_field"), "The password field is not hittable.")
    password.tap()
    // Capture only the empty password field, before real credentials are entered.
    capture("password-form-keyboard")
    let keyChoice = app.descendants(matching: .any).matching(identifier: "ssh-auth-public-key").firstMatch
    XCTAssertTrue(keyChoice.waitForExistence(timeout: 10), "Private key authentication is unavailable.")
    XCTAssertTrue(revealAuthenticationControl(keyChoice, stage: "key_choice"), "Private key authentication cannot be selected.")
    keyChoice.tap()
    XCTAssertFalse(password.exists, "The unselected password field is still exposed.")
    record("authentication_selector_verified")
  }

  private func effectiveScrollViewport(_ scroll: XCUIElement) -> CGRect? {
    guard scroll.exists else { return nil }
    let viewport = scroll.frame.intersection(app.frame)
    guard !viewport.isNull, viewport.width > 0, viewport.height > 0 else { return nil }
    let keyboard = app.keyboards.firstMatch
    guard keyboard.exists, keyboard.frame.intersects(viewport) else { return viewport }
    let effectiveBottom = min(viewport.maxY, keyboard.frame.minY)
    guard effectiveBottom > viewport.minY else { return nil }
    return CGRect(
      x: viewport.minX,
      y: viewport.minY,
      width: viewport.width,
      height: effectiveBottom - viewport.minY
    )
  }

  private func authenticationControlIsFullyVisible(
    _ element: XCUIElement,
    in scroll: XCUIElement
  ) -> Bool {
    guard element.exists, let viewport = effectiveScrollViewport(scroll) else { return false }
    let frame = element.frame
    return frame.width > 0 && frame.height > 0 && viewport.contains(frame)
  }

  private func waitForVisibleAuthenticationControl(
    _ element: XCUIElement,
    in scroll: XCUIElement,
    timeout: TimeInterval
  ) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      let matches = element.exists && element.isEnabled && element.isHittable
        && authenticationControlIsFullyVisible(element, in: scroll)
      if matches { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return false
  }

  private func recordAuthenticationControlGeometry(
    _ element: XCUIElement,
    scroll: XCUIElement,
    stage: String,
    attempt: Int,
    phase: String
  ) {
    let keyboard = app.keyboards.firstMatch
    let controlExists = element.exists
    let controlFrame: CGRect? = controlExists ? element.frame : nil
    let controlEnabled = controlExists ? element.isEnabled : false
    let controlHittable = controlExists ? element.isHittable : false
    let scrollExists = scroll.exists
    let scrollFrame: CGRect? = scrollExists ? scroll.frame : nil
    let keyboardExists = keyboard.exists
    let keyboardFrame: CGRect? = keyboardExists ? keyboard.frame : nil
    var effectiveViewport: CGRect?
    if let scrollFrame = scrollFrame {
      let viewport = scrollFrame.intersection(app.frame)
      if !viewport.isNull, viewport.width > 0, viewport.height > 0 {
        if let keyboardFrame = keyboardFrame, keyboardFrame.intersects(viewport) {
          let effectiveBottom = min(viewport.maxY, keyboardFrame.minY)
          if effectiveBottom > viewport.minY {
            effectiveViewport = CGRect(
              x: viewport.minX,
              y: viewport.minY,
              width: viewport.width,
              height: effectiveBottom - viewport.minY
            )
          }
        } else {
          effectiveViewport = viewport
        }
      }
    }
    let controlFullyVisible = controlFrame.map { frame in
      frame.width > 0 && frame.height > 0 && effectiveViewport?.contains(frame) == true
    } ?? false
    appendFixedArtifact("ios-ui-auth-control-diagnostics.txt", lines: [
      "stage=\(stage)",
      "attempt=\(attempt)",
      "phase=\(phase)",
      "control_exists=\(controlExists ? 1 : 0)",
      "control_frame=\(controlFrame.map { String(describing: $0) } ?? "unavailable")",
      "control_enabled=\(controlEnabled ? 1 : 0)",
      "control_hittable=\(controlHittable ? 1 : 0)",
      "scroll_exists=\(scrollExists ? 1 : 0)",
      "scroll_frame=\(scrollFrame.map { String(describing: $0) } ?? "unavailable")",
      "effective_viewport=\(effectiveViewport.map { String(describing: $0) } ?? "unavailable")",
      "control_fully_visible=\(controlFullyVisible ? 1 : 0)",
      "keyboard_exists=\(keyboardExists ? 1 : 0)",
      "keyboard_frame=\(keyboardFrame.map { String(describing: $0) } ?? "unavailable")",
    ])
  }

  /// Used only before any credential is entered. A full-frame swipe can
  /// start on the IME when the sheet's scroll frame extends below it.
  private func revealAuthenticationControl(
    _ element: XCUIElement,
    stage: String
  ) -> Bool {
    let scroll = app.scrollViews.containing(.textField, identifier: "ssh-host").firstMatch
    for attempt in 0..<5 {
      if waitForVisibleAuthenticationControl(element, in: scroll, timeout: 1) { return true }
      recordAuthenticationControlGeometry(
        element,
        scroll: scroll,
        stage: stage,
        attempt: attempt,
        phase: "before_scroll"
      )
      guard scroll.exists else { break }
      let appFrame = app.frame
      guard let viewport = effectiveScrollViewport(scroll) else { break }
      guard viewport.height >= 80, viewport.width >= 16 else { break }
      guard element.exists else { break }
      // Recompute the direction from the current frame on every attempt. A
      // previous drag can pass the target, especially with the keyboard open.
      let distance = max(-viewport.height * 0.5,
        min(viewport.height * 0.5, viewport.midY - element.frame.midY))
      guard abs(distance) > 1 else { break }
      let startY = viewport.midY - distance / 2
      let origin = app.coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
      let start = origin.withOffset(CGVector(
        dx: viewport.minX + 8 - appFrame.minX,
        dy: startY - appFrame.minY
      ))
      let end = origin.withOffset(CGVector(
        dx: viewport.minX + 8 - appFrame.minX,
        dy: startY + distance - appFrame.minY
      ))
      record("\(stage)_scroll_visible_viewport")
      // Release only after holding still, rather than ending with a fling.
      start.press(forDuration: 0.05, thenDragTo: end,
        withVelocity: .slow, thenHoldForDuration: 0.2)
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
      recordAuthenticationControlGeometry(
        element,
        scroll: scroll,
        stage: stage,
        attempt: attempt,
        phase: "after_scroll"
      )
    }
    if waitForVisibleAuthenticationControl(element, in: scroll, timeout: 2) { return true }
    recordAuthenticationControlGeometry(
      element,
      scroll: scroll,
      stage: stage,
      attempt: 5,
      phase: "final"
    )
    // All callers run before the private key or passphrase is entered.
    capture("password-form-\(stage)-unavailable")
    return false
  }

  private func verifyFoundationRelaunch() throws {
    // Target the installed app explicitly. A host-side simctl openurl can
    // stop at SpringBoard's "Open in meeterm?" dialog instead of delivering
    // the URL. This is a fresh launch after the real SSH flow has completed.
    record("foundation_launch")
    let launchEpoch = Date().timeIntervalSince1970
    app.launch()
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 60), "The fresh app did not reach the foreground.")
    app.open(URL(string: "meeterm://foundation?foundation=1")!)
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 60), "The foundation app did not reach the foreground.")
    XCTAssertTrue(app.staticTexts["Native foundation preview"].waitForExistence(timeout: 60), "The foundation URL did not open the preview.")
    XCTAssertTrue(waitForTerminal(), "The foundation native terminal is unavailable.")

    record("foundation_survival_start")
    let survivalStartEpoch = Date().timeIntervalSince1970
    let leftForeground = XCTNSPredicateExpectation(
      predicate: NSPredicate { _, _ in self.app.state != .runningForeground },
      object: app
    )
    leftForeground.isInverted = true
    XCTAssertEqual(XCTWaiter.wait(for: [leftForeground], timeout: 10), .completed,
      "The foundation app left the foreground during the no-crash observation.")
    let survivalEndEpoch = Date().timeIntervalSince1970
    // The host requires this fresh launch's native-ready and first-frame logs
    // at least five seconds before observation ends. The first five seconds
    // allow the native surface to finish its initial draw after the UI appears.
    let observation = [
      "launch_epoch": launchEpoch,
      "survival_start_epoch": survivalStartEpoch,
      "survival_end_epoch": survivalEndEpoch,
    ]
    let data = try JSONSerialization.data(withJSONObject: observation, options: [.sortedKeys])
    try data.write(to: artifactDirectory.appendingPathComponent("ios-foundation-observation.json"), options: .atomic)
    record("capture_foundation")
    capture("terminal")
    record("foundation_verified")
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
    // Keep the stage-only contract stable; timing contains only elapsed
    // milliseconds and the same fixed stage names, never XCTest descriptions.
    let elapsed = Int((ProcessInfo.processInfo.systemUptime - testStartedAt) * 1000)
    let timingPath = artifactDirectory.appendingPathComponent("ios-ui-timing.txt")
    for (path, line) in [(validationPath, stage + "\n"), (timingPath, "\(elapsed) \(stage)\n")] {
      let data = Data(line.utf8)
      if FileManager.default.fileExists(atPath: path.path) {
        if let handle = try? FileHandle(forWritingTo: path) {
          handle.seekToEndOfFile()
          handle.write(data)
          try? handle.close()
        }
      } else {
        try? data.write(to: path, options: .atomic)
      }
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

  private func fillTextField(label: String, value: String) {
    // Only non-secret short fields use readback. Never inspect or publish the
    // private-key editor's value through this helper.
    guard ["Host", "Port", "Username", "Server name", "Terminal font size", "Scrollback lines", "Workspace or terminal name"].contains(label) else {
      XCTFail("The short-field helper received an unsupported field.")
      return
    }
    let field = input(label)
    XCTAssertTrue(field.waitForExistence(timeout: 10), "The \(label) field is unavailable.")
    let stage = "fill_" + label.lowercased().replacingOccurrences(of: " ", with: "_")
    for attempt in 0..<2 {
      record("\(stage)_focus")
      guard waitForHittable(field, timeout: 10) else {
        XCTFail("The short field is not hittable.")
        return
      }
      record("\(stage)_hittable")
      // These fixture values fit on one line. Tapping its trailing edge puts
      // the caret after the current value before deleting it on a retry.
      field.coordinate(withNormalizedOffset: CGVector(dx: 0.95, dy: 0.5)).tap()
      record("\(stage)_tapped")
      guard let observed = shortFieldValue(field), observed.utf16.count <= 256 else {
        writeShortFieldDiagnostics(
          label: label,
          field: field,
          expected: value,
          phase: "initial_value_unavailable",
          attempt: attempt
        )
        XCTFail("The short field returned an unexpected value length.")
        return
      }
      if !observed.isEmpty {
        record("\(stage)_clear")
        if label == "Workspace or terminal name" {
          // Tmux can supply a long generated pane title. Selecting it all and
          // sending one delete avoids queuing one controlled-input update per
          // UTF-16 code unit through React Native.
          record("\(stage)_clear_select_all")
          let selectAllCandidates = [app.menuItems["Select All"], app.buttons["Select All"]]
          var selectAll = selectAllCandidates.first(where: { $0.exists && $0.isHittable })
          if selectAll == nil {
            record("\(stage)_clear_select_all_reveal")
            field.press(forDuration: 1.0)
            selectAll = waitForHittableElement(selectAllCandidates, timeout: 5)
          } else {
            record("\(stage)_clear_select_all_existing")
          }
          guard let selectAll = selectAll else {
            record("\(stage)_clear_select_all_unavailable")
            writeShortFieldDiagnostics(
              label: label,
              field: field,
              expected: "",
              phase: "clear_select_all_unavailable",
              attempt: attempt
            )
            captureNameFieldFailureIfSafe()
            XCTFail("The name field Select All action is unavailable.")
            return
          }
          record("\(stage)_clear_select_all_ready")
          selectAll.tap()
          record("\(stage)_clear_delete")
          field.typeText(XCUIKeyboardKey.delete.rawValue)
        } else {
          field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: observed.utf16.count))
        }
        record("\(stage)_clear_wait")
        let clearWaitResult = shortFieldWaitResult(field, expected: "", timeout: 5)
        guard clearWaitResult == .completed else {
          record("\(stage)_clear_wait_\(waiterResultKey(clearWaitResult))")
          record("\(stage)_clear_failed")
          writeShortFieldDiagnostics(
            label: label,
            field: field,
            expected: "",
            phase: "clear_mismatch",
            attempt: attempt
          )
          if label == "Workspace or terminal name" {
            captureNameFieldFailureIfSafe()
          }
          XCTFail("The short field could not be cleared.")
          return
        }
        record("\(stage)_clear_verified")
      }
      record("\(stage)_type")
      if attempt == 0 && label != "Username" {
        field.typeText(value)
      } else {
        if attempt == 0 { record("\(stage)_initial_paced") }
        // Settle each prefix so a fast synthetic burst cannot repeatedly
        // outrun the controlled React Native field.
        var prefix = ""
        for character in value {
          prefix.append(character)
          field.typeText(String(character))
          if !waitForShortFieldValue(field, expected: prefix, timeout: 5) {
            writeShortFieldDiagnostics(
              label: label,
              field: field,
              expected: prefix,
              phase: attempt == 0 ? "initial_prefix_mismatch" : "retry_prefix_mismatch",
              attempt: attempt
            )
            break
          }
        }
      }
      record("\(stage)_readback")
      let readbackMatched = waitForShortFieldValue(field, expected: value, timeout: 5)
      if readbackMatched {
        record("\(stage)_verified")
        return
      }
      writeShortFieldDiagnostics(
        label: label,
        field: field,
        expected: value,
        phase: "readback_mismatch",
        attempt: attempt
      )
      if attempt == 0 { record("\(stage)_retry") }
    }
    XCTFail("The short field did not retain the expected input after one retry.")
  }

  private func shortFieldValue(_ field: XCUIElement) -> String? {
    guard let value = field.value as? String else { return nil }
    guard !value.isEmpty else { return "" }
    return value == field.placeholderValue ? "" : value
  }

  private func waitForShortFieldValue(_ field: XCUIElement, expected: String, timeout: TimeInterval) -> Bool {
    shortFieldWaitResult(field, expected: expected, timeout: timeout) == .completed
  }

  private func shortFieldWaitResult(
    _ field: XCUIElement,
    expected: String,
    timeout: TimeInterval
  ) -> XCTWaiter.Result {
    if !expected.isEmpty {
      let predicate = NSPredicate { _, _ in self.shortFieldValue(field) == expected }
      let matched = XCTNSPredicateExpectation(predicate: predicate, object: field)
      return XCTWaiter.wait(for: [matched], timeout: timeout)
    }
    // Use immediate readback for empty fields before bounded polling; non-empty
    // fields retain the existing XCTest expectation.
    let deadline = Date().addingTimeInterval(max(0, timeout))
    if shortFieldValue(field) == expected { return .completed }
    while true {
      let remaining = deadline.timeIntervalSinceNow
      guard remaining > 0 else { break }
      RunLoop.current.run(until: Date().addingTimeInterval(min(0.25, remaining)))
      guard Date() < deadline else { break }
      if shortFieldValue(field) == expected {
        return .completed
      }
    }
    return .timedOut
  }

  private func waiterResultKey(_ result: XCTWaiter.Result) -> String {
    switch result {
    case .completed: return "completed"
    case .timedOut: return "timed_out"
    case .incorrectOrder: return "incorrect_order"
    case .invertedFulfillment: return "inverted_fulfillment"
    case .interrupted: return "interrupted"
    @unknown default: return "unknown"
    }
  }

  private func writeShortFieldDiagnostics(
    label: String,
    field: XCUIElement,
    expected: String,
    phase: String,
    attempt: Int
  ) {
    let observed = shortFieldValue(field)
    let valueAvailable = observed == nil ? 0 : 1
    let valueEmpty = observed.map { $0.isEmpty ? 1 : 0 } ?? -1
    let valueLength = observed?.utf16.count ?? -1
    let expectedPrefix = observed.map { expected.hasPrefix($0) ? 1 : 0 } ?? -1
    let caseInsensitiveMatch = observed.map {
      $0.caseInsensitiveCompare(expected) == .orderedSame ? 1 : 0
    } ?? -1
    let keyboard = app.keyboards.firstMatch
    appendFixedArtifact(
      "ios-ui-short-field-diagnostics.txt",
      lines: [
        "field=\(label.lowercased())",
        "phase=\(phase)",
        "attempt=\(attempt)",
        "field_exists=\(field.exists ? 1 : 0)",
        "field_hittable=\(field.isHittable ? 1 : 0)",
        "value_available=\(valueAvailable)",
        "value_empty=\(valueEmpty)",
        "value_length=\(valueLength)",
        "expected_length=\(expected.utf16.count)",
        "observed_is_expected_prefix=\(expectedPrefix)",
        "case_insensitive_match=\(caseInsensitiveMatch)",
        "keyboard_exists=\(keyboard.exists ? 1 : 0)",
        "keyboard_hittable=\(keyboard.isHittable ? 1 : 0)",
      ]
    )
  }

  private func captureNameFieldFailureIfSafe() {
    if safeForPostFormScreenshot() {
      capture("daily-name-field-failure")
    } else {
      record("daily_name_field_failure_capture_blocked")
    }
  }

  private func writeDailyWorkspaceCreateDiagnostics(
    nameField: XCUIElement,
    submit: XCUIElement,
    expectedRow: XCUIElement
  ) {
    let observation = connectionStateObservation()
    let appForeground = app.state == .runningForeground
    let formGone = connectionFormIsGone()
    writeFixedArtifact(
      "ios-ui-daily-workspace-create-diagnostics.txt",
      lines: [
        "app_foreground=\(appForeground ? 1 : 0)",
        "connection_form_gone=\(formGone ? 1 : 0)",
        "safe_for_post_form_screenshot=\(appForeground && formGone ? 1 : 0)",
        "name_field_exists=\(nameField.exists ? 1 : 0)",
        "submit_exists=\(submit.exists ? 1 : 0)",
        "submit_hittable=\(submit.exists && submit.isHittable ? 1 : 0)",
        "expected_workspace_row_exists=\(expectedRow.exists ? 1 : 0)",
        "connection_state=\(observation.key)",
        "connection_state_label_present=\(observation.present ? 1 : 0)",
      ]
    )
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
    let predicate = NSPredicate(
      format: "identifier BEGINSWITH %@ AND label BEGINSWITH %@",
      "workspace-row-", "Workspace "
    )
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
      ("profile_name", "名前は制御文字を含まない80文字以内で入力してください。"),
      ("host", "空白を含まないホスト名か IP アドレスを入力してください。"),
      ("port", "1〜65535 の数字を入力してください。"),
      ("username", "SSH のユーザー名を入力してください。空白は使えません。"),
      ("private_key", "BEGIN と END の行を含む OpenSSH 形式の秘密鍵を貼り付けてください。"),
      ("password", "SSH パスワードを入力してください。"),
      ("submission_rejected", "保存または接続を開始できませんでした。接続先を確認して、認証情報を入力し直してください。"),
      ("submission_failed", "保存または接続を開始できませんでした。認証情報を入力し直して、もう一度試してください。"),
    ]
    let lines = validations.map { name, message in
      "\(name)_validation_error_visible=\(app.staticTexts[message].exists ? 1 : 0)"
    } + [
      "host_field_visible=\(app.descendants(matching: .any)["Host"].exists ? 1 : 0)",
      "private_key_field_visible=\(app.descendants(matching: .any)["Private OpenSSH key"].exists ? 1 : 0)",
      "submit_visible=\(app.buttons["ssh-submit"].exists ? 1 : 0)",
      "submit_enabled=\(app.buttons["ssh-submit"].isEnabled ? 1 : 0)",
      "activity_indicator_visible=\(app.activityIndicators.firstMatch.exists ? 1 : 0)",
      "profile_name_matches=\(shortFieldValue(input("Server name")) == "Daily fixture" ? 1 : 0)",
      "app_foreground=\(app.state == .runningForeground ? 1 : 0)",
      "form_dismissed=0",
    ]
    appendFixedArtifact("ios-ui-form-diagnostics.txt", lines: lines)
  }

  private func recordConnectionState() {
    let observation = connectionStateObservation()
    writeConnectionStateArtifact(observation)
  }

  private func writeConnectionStateArtifact(_ observation: (key: String, present: Bool)) {
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
    writeConnectionStateArtifact(observation)
    writeFixedArtifact(
      "ios-ui-host-trust-diagnostics.txt",
      lines: [
        "phase=prompt_timeout",
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

  private func writePostTrustDiagnostics(
    phase: HostTrustFailurePhase,
    alert: XCUIElement,
    trust: XCUIElement
  ) {
    // Persist only fixed state keys and booleans. Never serialize the alert,
    // whose body contains the fixture endpoint and host-key fingerprint.
    let observation = connectionStateObservation()
    let alertExists = alert.exists
    let trustExists = trust.exists
    let trustHittable = trustExists && trust.isHittable
    let appForeground = app.state == .runningForeground
    let hostResponseError = app.staticTexts[
      "ホスト鍵への回答を送れませんでした。接続をやり直してください。"
    ].exists
    let screenshotSafe = appForeground && connectionFormIsGone()
    writeConnectionStateArtifact(observation)
    writeFixedArtifact(
      "ios-ui-host-trust-response-diagnostics.txt",
      lines: [
        "phase=\(phase.rawValue)",
        "app_foreground=\(appForeground ? 1 : 0)",
        "connection_state=\(observation.key)",
        "connection_state_label_present=\(observation.present ? 1 : 0)",
        "trust_alert_exists=\(alertExists ? 1 : 0)",
        "trust_button_exists=\(trustExists ? 1 : 0)",
        "trust_button_hittable=\(trustHittable ? 1 : 0)",
        "host_response_error_visible=\(hostResponseError ? 1 : 0)",
        "safe_for_post_form_screenshot=\(screenshotSafe ? 1 : 0)",
      ]
    )
    if screenshotSafe {
      capture("host-trust-response-failure")
    }
  }

  private func writeTerminalKeyboardDiagnostics(
    stage: String,
    character: Character,
    requestedKey: XCUIElement
  ) {
    let terminal = app.otherElements["Terminal"]
    let keyboard = app.keyboards.firstMatch
    let paste = button("Paste")
    let hideKeyboard = button("Hide keyboard")
    let uppercaseKey = app.keys[String(character).uppercased()]
    let sameLabelButton = app.buttons.matching(
      NSPredicate(format: "label ==[c] %@", String(character))
    ).firstMatch
    let appForeground = app.state == .runningForeground
    let formGone = connectionFormIsGone()
    let terminalExists = terminal.exists
    writeFixedArtifact(
      "ios-ui-terminal-keyboard-diagnostics.txt",
      lines: [
        "stage=\(stage)",
        "app_foreground=\(appForeground ? 1 : 0)",
        "connection_form_gone=\(formGone ? 1 : 0)",
        "terminal_exists=\(terminalExists ? 1 : 0)",
        "terminal_hittable=\(terminalExists && terminal.isHittable ? 1 : 0)",
        "keyboard_exists=\(keyboard.exists ? 1 : 0)",
        "keyboard_hittable=\(keyboard.exists && keyboard.isHittable ? 1 : 0)",
        "paste_exists=\(paste.exists ? 1 : 0)",
        "paste_hittable=\(paste.exists && paste.isHittable ? 1 : 0)",
        "hide_keyboard_exists=\(hideKeyboard.exists ? 1 : 0)",
        "hide_keyboard_hittable=\(hideKeyboard.exists && hideKeyboard.isHittable ? 1 : 0)",
        "requested_key_exists=\(requestedKey.exists ? 1 : 0)",
        "requested_key_hittable=\(requestedKey.exists && requestedKey.isHittable ? 1 : 0)",
        "uppercase_key_exists=\(uppercaseKey.exists ? 1 : 0)",
        "uppercase_key_hittable=\(uppercaseKey.exists && uppercaseKey.isHittable ? 1 : 0)",
        "same_label_button_exists=\(sameLabelButton.exists ? 1 : 0)",
        "same_label_button_hittable=\(sameLabelButton.exists && sameLabelButton.isHittable ? 1 : 0)",
      ]
    )
    if appForeground && formGone && terminalExists {
      capture("terminal-keyboard-failure")
    }
  }

  private func writeFixedArtifact(_ name: String, lines: [String]) {
    let contents = lines.joined(separator: "\n") + "\n"
    try? Data(contents.utf8).write(
      to: artifactDirectory.appendingPathComponent(name),
      options: .atomic
    )
  }

  private func appendFixedArtifact(_ name: String, lines: [String]) {
    let data = Data((lines.joined(separator: "\n") + "\n").utf8)
    let path = artifactDirectory.appendingPathComponent(name)
    if let handle = try? FileHandle(forWritingTo: path) {
      handle.seekToEndOfFile()
      handle.write(data)
      try? handle.close()
    } else {
      try? data.write(to: path, options: .atomic)
    }
  }

  private func waitForHittable(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if element.exists && element.isHittable { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return element.exists && element.isHittable
  }

  private func waitForHittableElement(
    _ elements: [XCUIElement],
    timeout: TimeInterval
  ) -> XCUIElement? {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      for element in elements where element.exists && element.isHittable {
        return element
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return elements.first(where: { $0.exists && $0.isHittable })
  }

  private func waitForDisappearance(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if !element.exists { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return !element.exists
  }

  private func enterTerminalCommand(_ value: String, stage: String) {
    dismissQuickPathTutorialIfPresent(stage: stage)
    // Tap real keyboard keys so letter commits and Enter are exercised even
    // though the native preedit-only UITextView is hidden from accessibility.
    // Paste the remainder through the native toolbar, then require the remote
    // marker to prove that all three input paths reached the selected shell.
    let prefix = String(value.prefix { $0.isASCII && $0.isLetter })
    XCTAssertFalse(prefix.isEmpty, "The fixture command needs an ASCII word prefix.")
    record("\(stage)_keyboard_letters")
    for character in prefix {
      let key = app.keys[String(character)]
      guard key.waitForExistence(timeout: 10) else {
        record("\(stage)_keyboard_letter_unavailable")
        writeTerminalKeyboardDiagnostics(stage: stage, character: character, requestedKey: key)
        XCTFail("The terminal keyboard letter is unavailable.")
        return
      }
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
    let enabled = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "enabled == YES"), object: paste
    )
    XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 10), .completed, "The terminal Paste action is disabled.")
    record("\(stage)_paste_tap")
    paste.tap()
    record("\(stage)_paste_tapped")
    // UIPasteControl loads the item provider asynchronously. Wait for the
    // native action's completion before clearing its source or sending Enter.
    let finished = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "value == %@", "Ready"), object: paste
    )
    XCTAssertEqual(XCTWaiter.wait(for: [finished], timeout: 10), .completed, "The native paste did not finish.")
    record("\(stage)_paste_finished")
    UIPasteboard.general.string = nil

    record("\(stage)_keyboard_return")
    // The keyboard's action control may be exposed as a button rather than a
    // key. Keep the query inside the keyboard but accept either native role.
    let keyboard = app.keyboards.firstMatch
    let enter = keyboard.descendants(matching: .any).matching(
      NSPredicate(format: "label ==[c] %@ OR identifier ==[c] %@", "return", "return")
    ).firstMatch
    guard enter.waitForExistence(timeout: 10), waitForHittable(enter, timeout: 10) else {
      record("\(stage)_keyboard_return_unavailable")
      if safeForPostFormScreenshot() { capture("terminal-return-unavailable") }
      XCTFail("The terminal Return action is unavailable.")
      return
    }
    enter.tap()
    record("\(stage)_await_remote_marker")
  }

  private func dismissQuickPathTutorialIfPresent(stage: String) {
    let prompt = app.staticTexts[
      "Speed up your typing by sliding your finger across the letters to compose a word."
    ]
    guard prompt.exists else { return }
    record("\(stage)_quickpath_prompt")

    let continueButton = app.buttons["Continue"]
    guard continueButton.waitForExistence(timeout: 10),
          waitForHittable(continueButton, timeout: 10) else {
      record("\(stage)_quickpath_continue_unavailable")
      XCTFail("The QuickPath tutorial Continue action is unavailable.")
      return
    }
    record("\(stage)_quickpath_continue")
    continueButton.tap()
    guard waitForDisappearance(prompt, timeout: 10) else {
      record("\(stage)_quickpath_not_dismissed")
      XCTFail("The QuickPath tutorial did not dismiss.")
      return
    }
    record("\(stage)_quickpath_dismissed")
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

  private func waitForSelectionCopyResult(timeout: TimeInterval) -> HostSelectionCopyResult {
    let deadline = Date().addingTimeInterval(timeout)
    let expected = Data(selectionCopyPassedToken.utf8)
    while Date() < deadline {
      if let observed = try? Data(contentsOf: selectionCopyResultPath) {
        return observed == expected ? .passed : .rejected
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return .timedOut
  }

  private func shellQuote(_ value: String) -> String {
    "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
  }
}
