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

  private enum RuntimePickerFailurePhase: String {
    case pickerMissing = "picker_missing"
    case runtimeMissing = "runtime_missing"
    case runtimeNotHittable = "runtime_not_hittable"
    case connectedTimeout = "connected_timeout"
  }

  private enum HostSelectionCopyResult {
    case passed
    case rejected
    case timedOut
  }

  private let testStartedAt = ProcessInfo.processInfo.systemUptime
  private let app = XCUIApplication(bundleIdentifier: "dev.meeterm.app")
  private var publicPresentationObservation = false
  private var observesNativeInputDiagnostics = false
  private var observesInitialConnectionEntry = false
  private var preCredentialObservationAllowed = false
  private var preCredentialFailureRecorded = false
  private var recordingPresentationFailure = false
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

  private var transportLossMarkerPath: URL {
    URL(fileURLWithPath: requiredEnvironment("MEETERM_IOS_TRANSPORT_LOSS_MARKER_PATH"))
  }

  private var transportLossPreValue: String {
    requiredEnvironment("MEETERM_IOS_TRANSPORT_LOSS_PRE_VALUE")
  }

  private var transportLossPostValue: String {
    requiredEnvironment("MEETERM_IOS_TRANSPORT_LOSS_POST_VALUE")
  }

  override func setUpWithError() throws {
    continueAfterFailure = false
    preCredentialObservationAllowed = false
    preCredentialFailureRecorded = false
    publicPresentationObservation = name.contains("testStandardSeededScreensAndFoundation")
      || name.contains("testPolishStatesAndNavigation")
      || name.contains("testPolishNavigationAndFoundation")
    observesInitialConnectionEntry = name.contains("testShortSshInputAndDisconnect")
    observesNativeInputDiagnostics = publicPresentationObservation || observesInitialConnectionEntry
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
    if observesInitialConnectionEntry {
      try? FileManager.default.removeItem(at: transportLossMarkerPath)
      try? FileManager.default.removeItem(
        at: artifactDirectory.appendingPathComponent("ios-ui-transport-loss-observation.txt")
      )
    }
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
      at: artifactDirectory.appendingPathComponent("ios-ui-terminal-paste-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("terminal-paste-failure.png")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ios-ui-ssh-entry-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ssh-entry-initial.png")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("ssh-entry-failure.png")
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
      at: artifactDirectory.appendingPathComponent("ios-ui-runtime-picker-diagnostics.txt")
    )
    try? FileManager.default.removeItem(
      at: artifactDirectory.appendingPathComponent("runtime-picker-failure.png")
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
      "ios-ui-polish-validation.txt",
      "ios-ui-polish-navigation-validation.txt",
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
      "standard-runtime-picker.png",
      "standard-runtime-partial-error.png",
      "standard-runtime-empty.png",
      "standard-runtime-create.png",
      "standard-herdr-connection.png",
      "standard-herdr-groups.png",
      "standard-herdr-terminal.png",
      "standard-herdr-workspaces.png",
      "standard-recovery-progress.png",
      "standard-recovery-exhausted.png",
      "standard-recovery-mismatch.png",
      "standard-herdr-recovery-confirm.png",
      "standard-layout-restore-unconfirmed.png",
      "standard-runtime-layout-restore-unconfirmed.png",
      "standard-connection-error.png",
      "standard-session-switcher-current.png",
      "standard-session-switcher-loading.png",
      "standard-session-switcher-partial-error.png",
      "standard-session-switcher-stopped-herdr.png",
      "standard-session-switcher-credentials.png",
      "standard-session-switcher-host-key.png",
      "standard-session-switcher-create.png",
      "standard-session-switcher-pending.png",
      "standard-session-switcher-failure.png",
      "standard-session-switcher-long-names.png",
      "standard-session-switcher-current-dark.png",
      "standard-session-switcher-loading-dark.png",
      "standard-session-switcher-partial-error-dark.png",
      "standard-session-switcher-stopped-herdr-dark.png",
      "standard-session-switcher-credentials-dark.png",
      "standard-session-switcher-host-key-dark.png",
      "standard-session-switcher-create-dark.png",
      "standard-session-switcher-pending-dark.png",
      "standard-session-switcher-failure-dark.png",
      "standard-session-switcher-long-names-dark.png",
      "polish-welcome.png",
      "polish-empty.png",
      "polish-search-empty.png",
      "polish-disconnected.png",
      "polish-reconnecting.png",
      "polish-connection-error.png",
      "polish-long-workspaces.png",
      "polish-terminal-keyboard.png",
      "polish-edge-back.png",
      "public-presentation-failure.png",
      "ios-public-presentation-diagnostics.txt",
    ] {
      try? FileManager.default.removeItem(at: artifactDirectory.appendingPathComponent(name))
    }
    record("test_started")

    app.launchArguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
    if observesNativeInputDiagnostics {
      app.launchArguments += ["-meeterm-ui-observation"]
    }
    app.launch()
    if observesInitialConnectionEntry {
      // Arm only the explicit permission before the foreground wait. The
      // initial element queries and app-scoped screenshot happen below, after
      // the original wait, so a failed wait cannot abort diagnostics first.
      beginPreCredentialConnectionEntryObservation()
    }
    let reachedForeground = app.wait(for: .runningForeground, timeout: 60)
    if observesInitialConnectionEntry {
      recordPreCredentialConnectionEntryInitial(reachedForeground: reachedForeground)
    }
    XCTAssertTrue(reachedForeground, "The meeterm app did not reach the foreground.")
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
    if publicPresentationObservation, !recordingPresentationFailure,
       (issue.sourceCodeContext.location?.lineNumber ?? 0) > 0 {
      // Only these public presentation tests visit credential-free seeded screens.
      // Never capture arbitrary failures in the real SSH/forms suites.
      recordingPresentationFailure = true
      writePublicPresentationDiagnostics()
      recordingPresentationFailure = false
    }
    super.record(issue)
  }

  private func writePublicPresentationDiagnostics() {
    let terminal = visibleTerminalElement() ?? terminalQuery().firstMatch
    let keyboard = app.keyboards.firstMatch
    let hide = app.buttons.matching(NSPredicate(format: "label == %@", "Hide keyboard")).firstMatch
    let foreground = app.state == .runningForeground
    var lines = ["app_foreground=\(foreground ? 1 : 0)"]
    for (name, element) in [("terminal", terminal), ("keyboard", keyboard), ("hide_keyboard", hide)] {
      let exists = element.exists
      lines.append("\(name)_exists=\(exists ? 1 : 0)")
      lines.append("\(name)_hittable=\(exists && element.isHittable ? 1 : 0)")
      if exists, !element.frame.isNull, !element.frame.isInfinite {
        let frame = element.frame
        lines.append("\(name)_frame=\(Int(frame.minX)),\(Int(frame.minY)),\(Int(frame.width)),\(Int(frame.height))")
      }
    }
    writeFixedArtifact("ios-public-presentation-diagnostics.txt", lines: lines)
    if foreground { capture("public-presentation-failure") }
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

    guard selectFixtureTmuxRuntimeAndWaitForConnected(stage: "real_ssh_initial_runtime") else {
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

    let paneTabs = waitForTerminalTabs(minimum: 2)
    XCTAssertGreaterThanOrEqual(paneTabs.count, 2, "The fixture panes were not discovered.")
    let initialPane = paneTabs[0]
    let switchedPane = paneTabs.first(where: { $0.identifier != initialPane.identifier }) ?? paneTabs[1]
    let switchedPaneIdentifier = switchedPane.identifier
    record("select_second_pane")
    switchedPane.tap()
    XCTAssertTrue(waitForSelected(terminalTab(identifier: switchedPaneIdentifier)), "The second pane was not selected.")
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
    guard selectFixtureTmuxRuntimeAndWaitForConnected(stage: "real_ssh_manual_reconnect") else {
      return
    }

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
    let resumedPane = terminalTab(identifier: switchedPaneIdentifier)
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

    try verifyDailyUse(firstWorkspace: firstWorkspace, paneIdentifier: switchedPaneIdentifier)

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
    let sessionSwitcherScreens = [
      "session-switcher-current", "session-switcher-loading",
      "session-switcher-partial-error", "session-switcher-stopped-herdr",
      "session-switcher-credentials", "session-switcher-host-key",
      "session-switcher-create", "session-switcher-pending",
      "session-switcher-failure", "session-switcher-long-names",
    ]
    let screens = [
      "home", "servers", "connection", "password", "workspaces", "terminal",
      "settings", "workspace-name", "terminal-name", "handoff",
      "runtime-picker", "runtime-partial-error", "runtime-empty", "runtime-create",
      "herdr-connection", "herdr-groups", "herdr-terminal", "herdr-workspaces",
      "recovery-progress", "recovery-exhausted", "recovery-mismatch", "herdr-recovery-confirm",
      "layout-restore-unconfirmed", "runtime-layout-restore-unconfirmed",
      "connection-error",
    ] + sessionSwitcherScreens + sessionSwitcherScreens.map { "\($0)-dark" }
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
      if screen == "session-switcher-host-key" || screen == "session-switcher-host-key-dark" {
        let prompt = app.alerts["Trust this SSH host?"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10), "The seeded host-key prompt did not remain visible for review.")
        prompt.buttons["Cancel"].tap()
      }
    }

    // Keep the existing foundation check as a genuinely fresh process after
    // the seeded screen pass. The foundation uses the Rust poc-main fixture.
    app.terminate()
    try verifyFoundationRelaunch()
    writeFixedArtifact("ios-ui-standard-validation.txt", lines: ["case=standard result=passed"])
    record("standard_complete")
  }

  /// Additional states and native navigation, separate from the standard
  /// seeded-screen gate so both scopes retain their own execution budget.
  func testPolishStatesAndNavigation() throws {
    for screen in ["welcome", "empty", "search-empty", "disconnected", "reconnecting", "connection-error", "long-workspaces"] {
      record("polish_screen_\(screen)_open")
      app.open(try XCTUnwrap(URL(string: "meeterm://smoke?screen=\(screen)")))
      XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30))
      guard waitForStandardScreen(screen) else {
        XCTFail("The polish screen did not open: \(screen).")
        return
      }
      dismissQuickPathTutorialIfPresent(stage: "polish_\(screen)")
      capture("polish-\(screen)")
      record("polish_screen_\(screen)_captured")
    }
    try verifyPolishNavigation()
    app.terminate()
    try verifyFoundationRelaunch()
    writeFixedArtifact("ios-ui-polish-validation.txt", lines: ["case=polish result=passed"])
    record("polish_complete")
  }

  /// Focused navigation diagnostic independent of the seven polish states.
  /// The shared helper owns the search, native keyboard, settings, picker,
  /// Back, edge-Back, and search-preservation assertions. This entry adds its
  /// own validation record and keeps the fresh native foundation requirement.
  func testPolishNavigationAndFoundation() throws {
    try verifyPolishNavigation()
    app.terminate()
    try verifyFoundationRelaunch()
    writeFixedArtifact(
      "ios-ui-polish-navigation-validation.txt",
      lines: ["case=polish-navigation result=passed"]
    )
    record("polish_navigation_suite_complete")
  }

  /// Real presentation interactions using the existing native poc-main handle.
  /// No remote input, connection, or workspace creation is claimed by this pass.
  private func verifyPolishNavigation() throws {
    record("polish_navigation_open")
    app.open(try XCTUnwrap(URL(string: "meeterm://smoke?screen=workspaces")))
    XCTAssertTrue(waitForStandardScreen("workspaces"))
    button("Search workspaces").tap()
    let search = input("Search workspaces")
    XCTAssertTrue(waitForHittable(search, timeout: 10))
    search.tap()
    search.typeText("Main")
    let workspace = button("Workspace Main workspace")
    XCTAssertTrue(waitForHittable(workspace, timeout: 10))
    workspace.tap()
    XCTAssertTrue(waitForTerminal(), "The workspace did not push the native terminal screen.")

    record("polish_navigation_keyboard")
    let terminal = try terminalElement()
    terminal.tap()
    XCTAssertTrue(app.keyboards.firstMatch.waitForExistence(timeout: 10))
    let hideKeyboard = button("Hide keyboard")
    XCTAssertTrue(waitForHittable(hideKeyboard, timeout: 10))
    capture("polish-terminal-keyboard")
    hideKeyboard.tap()
    XCTAssertTrue(waitForDisappearance(app.keyboards.firstMatch, timeout: 10))

    record("polish_navigation_settings")
    button("Terminal menu").tap()
    let settings = button("Terminal settings")
    XCTAssertTrue(waitForHittable(settings, timeout: 10))
    settings.tap()
    XCTAssertTrue(waitForStandardScreen("settings"))
    button("Cancel").tap()
    XCTAssertTrue(waitForTerminal(), "Closing Settings did not restore the native terminal.")

    record("polish_navigation_picker")
    button("Switch workspace").tap()
    let close = button("Close sheet")
    XCTAssertTrue(waitForHittable(close, timeout: 10))
    close.tap()
    XCTAssertTrue(waitForTerminal())
    button("Back to workspaces").tap()
    XCTAssertTrue(waitForShortFieldValue(search, expected: "Main", timeout: 10), "Back lost the workspace search.")

    record("polish_navigation_edge_back")
    XCTAssertTrue(waitForHittable(workspace, timeout: 10))
    workspace.tap()
    XCTAssertTrue(waitForTerminal())
    let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.01, dy: 0.45))
    let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.90, dy: 0.45))
    start.press(forDuration: 0.1, thenDragTo: end)
    XCTAssertTrue(waitForShortFieldValue(search, expected: "Main", timeout: 10), "The native edge-back gesture did not restore search.")
    XCTAssertTrue(waitForHittable(workspace, timeout: 10))
    capture("polish-edge-back")
    record("polish_navigation_complete")
  }

  /// A bounded real SSH round trip. The private key is entered through the
  /// existing production form, host-key verification remains explicit, and
  /// one command proves native terminal input reached the fixture shell.
  func testShortSshInputAndDisconnect() throws {
    record("ssh_open_connection_form")
    openConnectionForm(observeInitialEntry: true)
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

    record("ssh_recovery_binding_before_background")
    let terminal = try terminalElement()
    let terminalTabs = waitForTerminalTabs(minimum: 1)
    guard let selectedTerminalTab = terminalTabs.first(where: { $0.isSelected }) else {
      XCTFail("The fixture did not expose a selected terminal tab before backgrounding.")
      return
    }
    let selectedPaneIdentifier = selectedTerminalTab.identifier
    XCTAssertFalse(selectedPaneIdentifier.isEmpty, "The selected terminal pane identity is unavailable.")

    // This is the real foreground/background lifecycle path. The app is
    // suspended and activated without terminating it, so a changed native
    // binding or a newly opened picker cannot be hidden by a cold launch.
    record("ssh_background")
    XCUIDevice.shared.press(.home)
    guard waitForBackground(timeout: 30) else {
      XCTFail("The app did not reach a background state after pressing Home.")
      return
    }
    record("ssh_background_same_process")

    record("ssh_foreground")
    app.activate()
    guard app.wait(for: .runningForeground, timeout: 30) else {
      XCTFail("The app did not return to the foreground.")
      return
    }
    guard waitForAuthoritativeReady(paneIdentifier: selectedPaneIdentifier, timeout: 90) else {
      XCTFail("Foreground recovery did not restore the same live runtime and terminal pane.")
      return
    }
    record("ssh_authoritative_ready")

    record("ssh_resumed_native_input")
    let resumedTerminal = try terminalElement()
    XCTAssertEqual(
      resumedTerminal.identifier,
      terminal.identifier,
      "Foreground recovery replaced the native terminal surface binding."
    )
    resumedTerminal.tap()
    // The short-suite marker starts from the file removed in setUp. Append so
    // a duplicate remote execution becomes two lines and fails the exact-once
    // host-side assertion instead of silently overwriting the first marker.
    let markerCommand = "printf '%s\\n' '\(markerValue)' >> \(shellQuote(markerPath.path))"
    enterTerminalCommand(markerCommand, stage: "ssh_resumed_native_input")
    XCTAssertTrue(
      waitForMarkerLines([markerValue]),
      "Foreground-recovered native terminal input did not reach the fixture shell exactly once."
    )
    record("ssh_resumed_remote_ack")
    capture("ssh-terminal-input")

    // This is a separate, deterministic transport-loss branch. The fixture
    // controller stops only its sshd process tree; the ordinary tmux server,
    // selected window, pane, shell, and host key remain in place. No XCTest
    // input is sent between the stop acknowledgement and the start
    // acknowledgement.
    let nativeHandleBeforeLoss = nativeTerminalHandleObservation()
    let terminalIdentifierBeforeLoss = nativeTerminalSurfaceElement().identifier
    guard let preLossCommand = transportLossMarkerCommand(
      value: transportLossPreValue,
      paneIdentifier: selectedPaneIdentifier,
      append: false
    ) else {
      XCTFail("The selected fixture pane identity is unavailable for transport-loss evidence.")
      return
    }
    resumedTerminal.tap()
    record("ssh_transport_loss_pre_marker")
    enterTerminalCommand(preLossCommand, stage: "ssh_transport_loss_pre_marker")
    guard waitForTransportLossMarkerLines(
      values: [transportLossPreValue], paneIdentifier: selectedPaneIdentifier, timeout: 30
    ) else {
      XCTFail("The pre-loss fixture marker did not reach the selected pane exactly once.")
      return
    }

    var transportStopped = false
    defer {
      // A failed stale-screen assertion must not leave the disposable fixture
      // stopped while XCTest is unwinding. This is one bounded cleanup action,
      // not a retry of a product recovery assertion.
      if transportStopped {
        _ = requestFixtureTransport("start")
        transportStopped = false
      }
    }
    guard requestFixtureTransport("stop") else { return }
    transportStopped = true
    record("ssh_transport_loss_injected")
    guard waitForTransportLossStale(
      paneIdentifier: selectedPaneIdentifier,
      terminalIdentifier: terminalIdentifierBeforeLoss,
      timeout: 90
    ) else {
      XCTFail("The app did not retain the selected terminal as cached read-only output after transport loss.")
      return
    }
    let staleHandle = nativeTerminalHandleObservation()
    let staleTerminalIdentifierSame = nativeTerminalSurfaceElement().identifier == terminalIdentifierBeforeLoss
    let stalePane = terminalTab(identifier: selectedPaneIdentifier)
    let stalePaneIdentifierSame = stalePane.exists
      && stalePane.identifier == selectedPaneIdentifier
      && stalePane.isSelected
    let staleNativeHandleSame = staleHandle == nativeHandleBeforeLoss
    record("ssh_transport_loss_stale_read_only")

    guard requestFixtureTransport("start") else { return }
    transportStopped = false
    record("ssh_transport_loss_restored")
    guard waitForAuthoritativeReady(paneIdentifier: selectedPaneIdentifier, timeout: 90) else {
      XCTFail("Transport recovery did not restore the selected runtime without reopening the picker.")
      return
    }
    let recoveredTerminal = try terminalElement()
    let recoveredHandle = nativeTerminalHandleObservation()
    let recoveredTerminalIdentifierSame = nativeTerminalSurfaceElement().identifier == terminalIdentifierBeforeLoss
    let recoveredPane = terminalTab(identifier: selectedPaneIdentifier)
    let recoveredPaneIdentifierSame = recoveredPane.exists
      && recoveredPane.identifier == selectedPaneIdentifier
      && recoveredPane.isSelected
    let recoveredNativeHandleSame = recoveredHandle == nativeHandleBeforeLoss
    let nativeHandleSame = staleNativeHandleSame && recoveredNativeHandleSame
    let selectedPaneIdentifierSame = stalePaneIdentifierSame && recoveredPaneIdentifierSame
    writeFixedArtifact(
      "ios-ui-transport-loss-observation.txt",
      lines: [
        "native_terminal_identifier_same=\(recoveredTerminalIdentifierSame && staleTerminalIdentifierSame ? "yes" : "no")",
        "native_handle_same=\(nativeHandleSame ? "yes" : "no")",
        "selected_pane_identifier_same=\(selectedPaneIdentifierSame ? "yes" : "no")",
        "cached_read_only_surface=yes",
        "picker_visible_during_loss=no",
        "input_during_loss=none",
      ]
    )
    XCTAssertTrue(staleTerminalIdentifierSame, "Transport loss replaced the retained terminal surface binding.")
    XCTAssertTrue(staleNativeHandleSame, "Transport loss changed the retained native terminal handle.")
    XCTAssertTrue(recoveredTerminalIdentifierSame, "Transport recovery replaced the native terminal surface binding.")
    XCTAssertTrue(recoveredNativeHandleSame, "Transport recovery changed the native terminal handle.")
    XCTAssertTrue(stalePaneIdentifierSame, "Transport loss changed the selected terminal pane.")
    XCTAssertTrue(recoveredPaneIdentifierSame, "Transport recovery changed the selected terminal pane.")
    record("ssh_transport_loss_authoritative_ready")

    guard let postLossCommand = transportLossMarkerCommand(
      value: transportLossPostValue,
      paneIdentifier: selectedPaneIdentifier,
      append: true
    ) else {
      XCTFail("The selected fixture pane identity is unavailable for the post-loss marker.")
      return
    }
    recoveredTerminal.tap()
    enterTerminalCommand(postLossCommand, stage: "ssh_transport_loss_post_marker")
    guard waitForTransportLossMarkerLines(
      values: [transportLossPreValue, transportLossPostValue],
      paneIdentifier: selectedPaneIdentifier,
      timeout: 30
    ) else {
      XCTFail("The post-loss fixture marker did not reach the same pane exactly once.")
      return
    }
    record("ssh_transport_loss_remote_ack")

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
    let screen = screen.hasSuffix("-dark") ? String(screen.dropLast(5)) : screen
    switch screen {
    case "welcome":
      return app.staticTexts["Your workspace. Anywhere."].waitForExistence(timeout: 30)
        && waitForHittable(button("Connect"), timeout: 30)
    case "empty":
      return app.staticTexts["A fresh workspace starts here."].waitForExistence(timeout: 30)
        && button("Create workspace").waitForExistence(timeout: 30)
    case "search-empty":
      return app.staticTexts["No matching workspaces"].waitForExistence(timeout: 30)
        && waitForHittable(button("Clear workspace search"), timeout: 30)
    case "disconnected":
      return app.staticTexts["Disconnected"].waitForExistence(timeout: 30)
        && waitForHittable(button("Reconnect"), timeout: 30)
    case "reconnecting":
      return app.staticTexts.matching(NSPredicate(format: "label == %@", "Reconnecting…")).firstMatch.waitForExistence(timeout: 30)
        && waitForHittable(button("Cancel connection"), timeout: 30)
    case "connection-error":
      let authentication = "Authentication failed. Check your username and the password or private key for your chosen sign-in method."
      let warning = "The old connection's desktop layout restore could not be confirmed."
      return app.staticTexts.matching(NSPredicate(format: "label == %@", "Connection failed")).firstMatch.waitForExistence(timeout: 30)
        && waitForHittable(button("Reconnect"), timeout: 30)
        && app.staticTexts[authentication].waitForExistence(timeout: 30)
        && app.staticTexts[warning].waitForExistence(timeout: 30)
        && waitForHittable(button("Dismiss desktop layout warning"), timeout: 30)
    case "long-workspaces":
      return waitForHittable(buttonStarting(with: "Workspace Production infrastructure — migration and release preparation"), timeout: 30)
        && waitForHittable(buttonStarting(with: "Workspace Research / terminal typography and international text"), timeout: 30)
    case "home":
      let title = app.staticTexts["Workspaces"]
      let profile = app.buttons.matching(
        NSPredicate(format: "label == %@", "Connect saved server Smoke server")
      ).firstMatch
      return title.waitForExistence(timeout: 30)
        && waitForHittable(profile, timeout: 30)
    case "servers":
      let title = app.staticTexts["Saved servers"]
      let profile = app.buttons.matching(
        NSPredicate(format: "identifier == %@", "server-profile-smoke-profile")
      ).firstMatch
      return title.waitForExistence(timeout: 30)
        && waitForHittable(profile, timeout: 30)
    case "connection":
      return app.staticTexts["Connect to server"].waitForExistence(timeout: 30)
        && waitForHittable(input("Host"), timeout: 30)
    case "password":
      guard app.staticTexts["Connect to server"].waitForExistence(timeout: 30) else { return false }
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
      guard connectedElement().waitForExistence(timeout: 30),
            let terminal = waitForTerminalElement(timeout: 30) else { return false }
      return waitForHittable(terminal, timeout: 30)
    case "settings":
      return app.staticTexts["Settings"].waitForExistence(timeout: 30)
        && waitForHittable(app.buttons["settings-submit"], timeout: 30)
    case "workspace-name":
      return app.staticTexts["Rename workspace"].waitForExistence(timeout: 30)
        && waitForHittable(input("Workspace or terminal name"), timeout: 30)
    case "terminal-name":
      return app.staticTexts["Rename terminal"].waitForExistence(timeout: 30)
        && waitForHittable(input("Workspace or terminal name"), timeout: 30)
    case "handoff":
      return app.staticTexts["Continue on your computer"].waitForExistence(timeout: 30)
        && waitForHittable(button("Disconnect"), timeout: 30)
    case "runtime-picker":
      return app.staticTexts["Choose a session"].waitForExistence(timeout: 30)
        && waitForHittable(button("tmux runtime meeterm"), timeout: 30)
        && waitForHittable(button("Herdr runtime default"), timeout: 30)
        && button("Herdr runtime paused").waitForExistence(timeout: 30)
    case "runtime-partial-error":
      return app.staticTexts["Herdr is not available over SSH. Open Herdr on your computer or check its installation."].waitForExistence(timeout: 30)
        && waitForHittable(button("tmux runtime meeterm"), timeout: 30)
    case "runtime-empty":
      let paused = button("Herdr runtime paused")
      return app.staticTexts["No tmux sessions found."].waitForExistence(timeout: 30)
        && app.staticTexts["No running runtime is available yet. Stopped Herdr sessions need to be opened on your computer."].waitForExistence(timeout: 30)
        && paused.waitForExistence(timeout: 30)
        // A session row is one accessibility button on iOS, so its visible
        // state text is grouped into that row. Verify the actionable contract
        // on the parent instead of looking for a child StaticText that the
        // accessibility tree intentionally does not expose.
        && !paused.isEnabled
    case "runtime-create":
      return app.staticTexts["New tmux session"].waitForExistence(timeout: 30)
        && input("New tmux session name").waitForExistence(timeout: 30)
        && waitForHittable(app.buttons["switcher-create-submit"], timeout: 30)
    case "session-switcher-current":
      let currentSession = button("tmux session meeterm on Smoke server (fixture@fixture.invalid:22)")
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && currentSession.waitForExistence(timeout: 30)
        && currentSession.isEnabled
        && waitForHittable(app.buttons["switcher-new-tmux"], timeout: 30)
        && waitForHittable(app.buttons["switcher-manage-servers"], timeout: 30)
        && waitForHittable(app.buttons["switcher-disconnect"], timeout: 30)
    case "session-switcher-loading":
      let tmux = button("tmux session meeterm on Smoke server (fixture@fixture.invalid:22)")
      let herdr = button("Herdr session default on Smoke server (fixture@fixture.invalid:22)")
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && app.staticTexts["Choose a server, then select one of its running sessions."].waitForExistence(timeout: 30)
        && tmux.waitForExistence(timeout: 30)
        && !tmux.isEnabled
        && herdr.waitForExistence(timeout: 30)
        && !herdr.isEnabled
    case "session-switcher-partial-error":
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && app.staticTexts["Herdr is not available over SSH. Open Herdr on your computer or check its installation."].waitForExistence(timeout: 30)
        && waitForHittable(button("tmux session meeterm on Smoke server (fixture@fixture.invalid:22)"), timeout: 30)
    case "session-switcher-stopped-herdr":
      let paused = button("Herdr session paused on Smoke server (fixture@fixture.invalid:22)")
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && waitForHittable(button("tmux session meeterm on Smoke server (fixture@fixture.invalid:22)"), timeout: 30)
        && paused.waitForExistence(timeout: 30)
        && !paused.isEnabled
    case "session-switcher-credentials":
      return app.staticTexts["Credentials"].waitForExistence(timeout: 30)
        && app.secureTextFields["SSH password"].waitForExistence(timeout: 30)
        && waitForHittable(button("Back to sessions"), timeout: 30)
    case "session-switcher-host-key":
      let prompt = app.alerts["Trust this SSH host?"]
      return app.staticTexts["Verify the SSH host key for fixture.invalid:22 in the confirmation prompt."].waitForExistence(timeout: 30)
        && prompt.waitForExistence(timeout: 30)
        && prompt.buttons["Trust and continue"].exists
        && prompt.buttons["Cancel"].exists
    case "session-switcher-create":
      return app.staticTexts["New tmux session"].waitForExistence(timeout: 30)
        && app.staticTexts["On Smoke server · fixture@fixture.invalid:22"].waitForExistence(timeout: 30)
        && input("New tmux session name").waitForExistence(timeout: 30)
        && waitForHittable(app.buttons["switcher-create-submit"], timeout: 30)
    case "session-switcher-pending":
      let pending = button("tmux session release-prep on Smoke server (fixture@fixture.invalid:22)")
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && pending.waitForExistence(timeout: 30)
        && !pending.isEnabled
    case "session-switcher-failure":
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && app.staticTexts["The selected session could not be opened. Refresh and try another session."].waitForExistence(timeout: 30)
        && app.staticTexts["The old connection's desktop layout restore could not be confirmed."].waitForExistence(timeout: 30)
        && waitForHittable(button("Dismiss desktop layout warning"), timeout: 30)
    case "session-switcher-long-names":
      // Row children are grouped into the row's accessibility label on iOS,
      // so the long server/session strings live on the aggregated buttons.
      return app.staticTexts["Switch session"].waitForExistence(timeout: 30)
        && button("Current server 東京・多地域インフラ移行とリリース準備用の作業サーバー").waitForExistence(timeout: 30)
        && button("tmux session 日本語ログ確認と多地域デプロイ前の長時間リリース準備セッション on 東京・多地域インフラ移行とリリース準備用の作業サーバー (release-operator@release-workspace-east-2.fixture.invalid:22)").waitForExistence(timeout: 30)
        && button("Server Production mirror · 日本語ログと運用監視用サーバー").waitForExistence(timeout: 30)
    case "layout-restore-unconfirmed":
      let warning = "The old connection's desktop layout restore could not be confirmed."
      return app.staticTexts["Disconnected"].waitForExistence(timeout: 30)
        && app.staticTexts[warning].waitForExistence(timeout: 30)
        && waitForHittable(button("Dismiss desktop layout warning"), timeout: 30)
    case "runtime-layout-restore-unconfirmed":
      let warning = "The old connection's desktop layout restore could not be confirmed."
      return app.staticTexts["Choose a session"].waitForExistence(timeout: 30)
        && app.staticTexts[warning].waitForExistence(timeout: 30)
        && waitForHittable(button("Dismiss desktop layout warning"), timeout: 30)
    case "herdr-connection":
      // The Last used badge is visible presentation inside the explicitly
      // labelled session button. Screen readiness therefore uses the same
      // accessible row that VoiceOver and the selection flow receive; the
      // downloaded screenshot covers the badge itself.
      return app.staticTexts["Choose a session"].waitForExistence(timeout: 30)
        && waitForHittable(button("Herdr runtime default"), timeout: 30)
    case "herdr-groups":
      let title = app.staticTexts["Switch group"]
      let development = buttonStarting(with: "Group Development")
      let tests = buttonStarting(with: "Group Tests & review")
      return title.waitForExistence(timeout: 30)
        && development.waitForExistence(timeout: 30)
        && tests.waitForExistence(timeout: 30)
        && development.label.contains("Agent status: working")
        && tests.label.contains("Agent status: finished")
    case "herdr-terminal":
      let groupPicker = buttonStarting(with: "Switch terminal group")
      guard let terminal = waitForTerminalElement(timeout: 30) else { return false }
      let selectedAgentLine = app.descendants(matching: .any).matching(
        NSPredicate(format: "identifier == %@", "selected-agent-line")
      ).firstMatch
      let selectedAgentLineReady = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
        selectedAgentLine.exists
          && selectedAgentLine.label.contains("Claude Code")
          && selectedAgentLine.label.contains("Agent status: working")
      }, object: nil)
      // Representative-view boundary: the initial screenshot must show the
      // selected owner/status and the native group/terminal surface. The
      // remaining seeded statuses may be horizontally offscreen, so their
      // coverage belongs to deterministic App/component tests and screenshot
      // review rather than this one-shot readiness check. This one screenshot
      // does not visually prove all five statuses.
      return waitForHittable(groupPicker, timeout: 30)
        && terminalHasValidFrame(terminal)
        && XCTWaiter.wait(for: [selectedAgentLineReady], timeout: 30) == .completed
    case "herdr-workspaces":
      let total = app.staticTexts.matching(
        NSPredicate(format: "label CONTAINS %@ AND label CONTAINS %@", "All", "2")
      ).firstMatch
      // WorkspaceRow is one accessible button with an explicit label. Its
      // count/Agent Text children are grouped into that element on iOS, so
      // querying them as independent staticTexts cannot establish readiness.
      // Require both production rows and their visible tap targets; rollup
      // marks and pane subtitles remain part of screenshot review.
      let labels = waitForWorkspaceLabels(minimum: 2)
      guard labels.contains(where: { $0.hasPrefix("Workspace Main workspace") }),
            labels.contains(where: { $0.hasPrefix("Workspace Tools workspace") }) else { return false }
      guard labels.contains(where: { $0.contains("Agent status: blocked") }),
            labels.contains(where: { $0.contains("Agent status: idle") }) else { return false }
      let main = app.buttons.matching(
        NSPredicate(format: "identifier == %@", "workspace-row-@smoke-main")
      ).firstMatch
      let tools = app.buttons.matching(
        NSPredicate(format: "identifier == %@", "workspace-row-@smoke-tools")
      ).firstMatch
      return total.waitForExistence(timeout: 30)
        && waitForHittable(main, timeout: 30)
        && waitForHittable(tools, timeout: 30)
    case "recovery-progress":
      return waitForRecoveryScreen(
        title: "Verifying this workspace…",
        detail: "Checking the server, runtime, and terminal.",
        actions: []
      )
    case "recovery-exhausted":
      return waitForRecoveryScreen(
        title: "Still offline",
        detail: "Couldn’t reach Smoke server.",
        actions: ["recovery-retry", "recovery-change"]
      )
    case "recovery-mismatch":
      return waitForRecoveryScreen(
        title: "This runtime can’t be restored",
        detail: "The runtime named “meeterm” is not the same instance as before.",
        actions: ["recovery-retry", "recovery-change"]
      )
    case "herdr-recovery-confirm":
      return waitForRecoveryScreen(
        title: "Confirmation needed",
        detail: "Herdr can’t verify that “dev” is the same instance.",
        actions: ["recovery-review", "recovery-change"]
      )
    default:
      return false
    }
  }

  private func recoveryElement(_ identifier: String) -> XCUIElement {
    app.descendants(matching: .any).matching(
      NSPredicate(format: "identifier == %@", identifier)
    ).firstMatch
  }

  private func waitForVisible(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if element.exists && !element.frame.isNull && !element.frame.isInfinite
        && element.frame.width > 0 && element.frame.height > 0 {
        return true
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return element.exists && !element.frame.isNull && !element.frame.isInfinite
      && element.frame.width > 0 && element.frame.height > 0
  }

  private func waitForEnabledHittable(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if element.exists && element.isEnabled && element.isHittable { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return element.exists && element.isEnabled && element.isHittable
  }

  private func waitForRecoveryScreen(
    title: String,
    detail: String,
    actions: [String]
  ) -> Bool {
    let rail = recoveryElement("recovery-rail")
    let titleElement = recoveryElement("recovery-title")
    let detailElement = recoveryElement("recovery-detail")
    let metaElement = recoveryElement("recovery-meta")
    guard let terminal = waitForTerminalElement(timeout: 30) else { return false }
    guard waitForVisible(rail, timeout: 30),
          waitForVisible(titleElement, timeout: 30),
          waitForVisible(detailElement, timeout: 30),
          waitForVisible(metaElement, timeout: 30),
          waitForVisible(terminal, timeout: 30),
          app.staticTexts[title].waitForExistence(timeout: 30),
          app.staticTexts[detail].waitForExistence(timeout: 30),
          app.staticTexts["Last received output · Input paused"].waitForExistence(timeout: 30)
    else {
      return false
    }
    return actions.allSatisfy { identifier in
      waitForEnabledHittable(recoveryElement(identifier), timeout: 30)
    }
  }

  private func nativeTerminalHandleObservation() -> String {
    let surface = nativeTerminalSurfaceElement()
    XCTAssertTrue(waitForVisible(surface, timeout: 30), "The native terminal surface is unavailable.")
    guard let value = surface.value as? String,
          value.hasPrefix("native-handle-"),
          value.count > "native-handle-".count else {
      XCTFail("The test-only native terminal handle observation is unavailable.")
      return ""
    }
    return value
  }

  private func nativeTerminalSurfaceElement() -> XCUIElement {
    app.descendants(matching: .any).matching(
      NSPredicate(format: "identifier == %@", "native-terminal-surface")
    ).firstMatch
  }

  private func fixtureControlURLs() -> (request: URL, status: URL)? {
    guard let requestValue = ProcessInfo.processInfo.environment[
      "MEETERM_SSH_FIXTURE_CONTROL_REQUEST"
    ], !requestValue.isEmpty,
    let statusValue = ProcessInfo.processInfo.environment[
      "MEETERM_SSH_FIXTURE_CONTROL_STATUS"
    ], !statusValue.isEmpty else {
      XCTFail("The fixture transport control is unavailable.")
      return nil
    }
    let request = URL(fileURLWithPath: requestValue)
    let status = URL(fileURLWithPath: statusValue)
    let root = request.deletingLastPathComponent()
    guard request.path.hasPrefix("/"), status.path.hasPrefix("/"),
          request.lastPathComponent == "sshd-control-request",
          status.lastPathComponent == "sshd-control-status",
          root.path == status.deletingLastPathComponent().path,
          root.lastPathComponent.hasPrefix("meeterm-ssh-fixture-") else {
      XCTFail("The fixture transport control path is invalid.")
      return nil
    }
    guard let attributes = try? FileManager.default.attributesOfItem(atPath: root.path),
          let permissions = attributes[.posixPermissions] as? NSNumber,
          permissions.intValue & 0o077 == 0 else {
      XCTFail("The fixture transport control directory is not private.")
      return nil
    }
    return (request, status)
  }

  private func requestFixtureTransport(_ action: String) -> Bool {
    guard action == "stop" || action == "start" else {
      XCTFail("The fixture transport action is invalid.")
      return false
    }
    guard let paths = fixtureControlURLs() else { return false }
    let token = "ios-transport-loss-" + UUID().uuidString
      .replacingOccurrences(of: "-", with: "")
      .lowercased()
    let requestContents = "\(token)\t\(action)\n"
    let temporary = paths.request.deletingLastPathComponent().appendingPathComponent(
      ".sshd-control-request-\(token)"
    )
    let fileManager = FileManager.default
    do {
      guard !fileManager.fileExists(atPath: paths.request.path) else {
        XCTFail("The fixture transport control is busy.")
        return false
      }
      try Data(requestContents.utf8).write(to: temporary, options: .atomic)
      try fileManager.setAttributes(
        [.posixPermissions: NSNumber(value: 0o600)],
        ofItemAtPath: temporary.path
      )
      try fileManager.moveItem(at: temporary, to: paths.request)
    } catch {
      try? fileManager.removeItem(at: temporary)
      XCTFail("The fixture transport request could not be written.")
      return false
    }

    let expected = "\(token)\tok\t\(action == "stop" ? "stopped" : "started")\n"
    let errorStatus = "\(token)\terror\n"
    let deadline = Date().addingTimeInterval(20)
    while Date() < deadline {
      if let observed = try? String(contentsOf: paths.status, encoding: .utf8) {
        if observed == expected { return true }
        if observed == errorStatus {
          XCTFail("The fixture transport action failed.")
          return false
        }
        if observed.hasPrefix(token + "\t") {
          XCTFail("The fixture transport returned an invalid status.")
          return false
        }
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    if (try? String(contentsOf: paths.request, encoding: .utf8)) == requestContents {
      try? fileManager.removeItem(at: paths.request)
    }
    XCTFail("The fixture transport action timed out.")
    return false
  }

  private func transportLossMarkerCommand(
    value: String,
    paneIdentifier: String,
    append: Bool
  ) -> String? {
    let validValue = value.range(
      of: #"^ios-ssh-loss-(pre|post)-[0-9a-f]{16}$"#,
      options: .regularExpression
    ) != nil
    guard validValue,
          !value.contains("'"), !value.contains("\n"), !value.contains("\r"),
          paneIdentifier.hasPrefix("terminal-tab-") else {
      return nil
    }
    let pane = String(paneIdentifier.dropFirst("terminal-tab-".count))
    guard pane.hasPrefix("%"), pane.dropFirst().allSatisfy({ $0.isNumber }) else {
      return nil
    }
    let redirect = append ? ">>" : ">"
    return "printf '%s:%s:%s\\n' '\(value)' '\(pane.dropFirst())' \"$$\" \(redirect) \(shellQuote(transportLossMarkerPath.path))"
  }

  private func waitForTransportLossMarkerLines(
    values: [String],
    paneIdentifier: String,
    timeout: TimeInterval
  ) -> Bool {
    let pane = String(paneIdentifier.dropFirst("terminal-tab-".count)).dropFirst()
    guard !pane.isEmpty, pane.allSatisfy({ $0.isNumber }) else { return false }
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if let contents = try? String(contentsOf: transportLossMarkerPath, encoding: .utf8) {
        let lines = contents.split(whereSeparator: { $0.isNewline }).map(String.init)
        let valid = lines.count == values.count && zip(lines, values).allSatisfy { line, value in
          let fields = line.split(separator: ":", omittingEmptySubsequences: false)
          return fields.count == 3
            && String(fields[0]) == value
            && String(fields[1]) == String(pane)
            && !fields[2].isEmpty
            && fields[2].allSatisfy({ $0.isNumber })
        }
        if valid { return true }
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return false
  }

  private func waitForTransportLossStale(
    paneIdentifier: String,
    terminalIdentifier: String,
    timeout: TimeInterval
  ) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    let pickerTitle = app.staticTexts.matching(
      NSPredicate(format: "label == %@ OR label == %@", "Choose a session", "Switch session")
    ).firstMatch
    let pickerRuntime = app.buttons.matching(
      NSPredicate(
        format: "identifier BEGINSWITH %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@",
        "runtime-row-", "tmux runtime ", "Herdr runtime ", "tmux session ", "Herdr session "
      )
    ).firstMatch
    let rail = recoveryElement("recovery-rail")
    let title = recoveryElement("recovery-title")
    let detail = recoveryElement("recovery-detail")
    let meta = recoveryElement("recovery-meta")
    let terminal = nativeTerminalSurfaceElement()
    while Date() < deadline {
      let pane = terminalTab(identifier: paneIdentifier)
      let cached = terminal.exists
        && terminal.label == "Terminal, cached output, read only"
        && !terminal.frame.isNull
        && !terminal.frame.isInfinite
        && terminal.frame.width > 0
        && terminal.frame.height > 0
      if app.state == .runningForeground
        && !pickerTitle.exists
        && !pickerRuntime.exists
        && rail.exists
        && title.exists
        && detail.exists
        && meta.exists
        && cached
        && pane.exists
        && pane.isSelected
        && terminal.identifier == terminalIdentifier {
        return true
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    let pane = terminalTab(identifier: paneIdentifier)
    return app.state == .runningForeground
      && !pickerTitle.exists
      && !pickerRuntime.exists
      && rail.exists
      && title.exists
      && detail.exists
      && meta.exists
      && terminal.exists
      && terminal.label == "Terminal, cached output, read only"
      && pane.exists
      && pane.isSelected
      && terminal.identifier == terminalIdentifier
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
    guard selectFixtureTmuxRuntimeAndWaitForConnected(stage: "ssh_initial_runtime") else {
      return false
    }
    record("ssh_connected")
    return true
  }

  private func selectFixtureTmuxRuntimeAndWaitForConnected(stage: String) -> Bool {
    let pickerTitle = app.staticTexts["Choose a session"]
    record("\(stage)_await_runtime_picker")
    guard pickerTitle.waitForExistence(timeout: 60) else {
      record("\(stage)_runtime_picker_missing")
      writeRuntimePickerDiagnostics(
        phase: .pickerMissing,
        stage: stage,
        pickerTitle: pickerTitle,
        runtime: button("tmux runtime meeterm"),
        connected: connectedElement()
      )
      XCTFail("The runtime picker did not appear after SSH authentication.")
      return false
    }

    let runtime = button("tmux runtime meeterm")
    record("\(stage)_await_tmux_runtime")
    guard runtime.waitForExistence(timeout: 30) else {
      record("\(stage)_tmux_runtime_missing")
      writeRuntimePickerDiagnostics(
        phase: .runtimeMissing,
        stage: stage,
        pickerTitle: pickerTitle,
        runtime: runtime,
        connected: connectedElement()
      )
      XCTFail("The fixture tmux runtime meeterm was not exposed by the runtime picker.")
      return false
    }
    guard waitForHittable(runtime, timeout: 30) else {
      record("\(stage)_tmux_runtime_not_hittable")
      writeRuntimePickerDiagnostics(
        phase: .runtimeNotHittable,
        stage: stage,
        pickerTitle: pickerTitle,
        runtime: runtime,
        connected: connectedElement()
      )
      XCTFail("The fixture tmux runtime meeterm was not hittable.")
      return false
    }

    record("\(stage)_tap_tmux_runtime")
    runtime.tap()
    record("\(stage)_tmux_runtime_tapped")
    let connected = connectedElement()
    record("\(stage)_await_connected")
    guard connected.waitForExistence(timeout: 90) else {
      record("\(stage)_connected_timeout")
      writeRuntimePickerDiagnostics(
        phase: .connectedTimeout,
        stage: stage,
        pickerTitle: pickerTitle,
        runtime: runtime,
        connected: connected
      )
      XCTFail("The native SSH/tmux connection did not reach Connected after selecting meeterm.")
      return false
    }
    record("\(stage)_connected")
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
    let discardButton = discard.buttons["Discard"]
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

  private func verifyDailyUse(firstWorkspace: String, paneIdentifier: String) throws {
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
    guard selectFixtureTmuxRuntimeAndWaitForConnected(stage: "real_ssh_cold_saved_profile") else {
      return
    }
    XCTAssertFalse(input("Private OpenSSH key").exists, "Saved credentials must not be returned to the form.")
    XCTAssertTrue(button(firstWorkspace).waitForExistence(timeout: 20))
    button(firstWorkspace).tap()
    XCTAssertTrue(waitForTerminal())
    let paneTab = terminalTab(identifier: paneIdentifier)
    XCTAssertTrue(paneTab.waitForExistence(timeout: 20))
    paneTab.tap()
    XCTAssertTrue(waitForSelected(paneTab))
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
    app.buttons["Light"].tap()
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
    app.buttons["Dark"].tap()
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
    button("Rename").tap()
    fillTextField(label: "Workspace or terminal name", value: "daily-renamed")
    button("name-submit").tap()
    XCTAssertTrue(button("Workspace daily-renamed").waitForExistence(timeout: 20))
    button("Workspace daily-renamed").tap()
    XCTAssertTrue(waitForTerminal())
    XCTAssertTrue(button("Create terminal").waitForExistence(timeout: 15))
    button("Create terminal").tap()
    XCTAssertGreaterThanOrEqual(waitForTerminalTabs(minimum: 2).count, 2)
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
    closePane.buttons["Close"].tap()
    let paneRemoved = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
      let tabs = self.visibleTerminalTabs()
      return Set(tabs.map { $0.identifier }).count == 1
    }, object: nil)
    XCTAssertEqual(XCTWaiter.wait(for: [paneRemoved], timeout: 20), .completed)
    button("Back to workspaces").tap()
    button("Workspace options daily-renamed").tap()
    button("Close").tap()
    let confirm = app.alerts.firstMatch
    XCTAssertTrue(confirm.waitForExistence(timeout: 10))
    confirm.buttons["Close"].tap()
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

  private func beginPreCredentialConnectionEntryObservation() {
    guard observesInitialConnectionEntry, !preCredentialObservationAllowed else { return }
    preCredentialObservationAllowed = true
  }

  private func recordPreCredentialConnectionEntryInitial(reachedForeground: Bool) {
    guard observesInitialConnectionEntry, preCredentialObservationAllowed else { return }
    guard reachedForeground, app.state == .runningForeground else {
      // Do not query XCUI elements or capture the whole screen when the app
      // did not reach the foreground; that could observe another app.
      appendFixedArtifact(
        "ios-ui-ssh-entry-diagnostics.txt",
        lines: [
          "phase=initial",
          "app_foreground=\(app.state == .runningForeground ? 1 : 0)",
        ]
      )
      writeFixedArtifact("ssh-entry-initial-unavailable.txt", lines: ["reason=foreground_unavailable"])
      preCredentialObservationAllowed = false
      return
    }
    writePreCredentialConnectionEntryDiagnostics(phase: "initial")
    capturePreCredential("ssh-entry-initial")
  }

  private func closePreCredentialConnectionEntryObservation() {
    preCredentialObservationAllowed = false
  }

  private func recordPreCredentialConnectionEntryFailure() {
    guard observesInitialConnectionEntry,
          preCredentialObservationAllowed,
          !preCredentialFailureRecorded else { return }
    preCredentialFailureRecorded = true
    writePreCredentialConnectionEntryDiagnostics(phase: "entry_failure")
    capturePreCredential("ssh-entry-failure")
    // Do not leave a failure diagnostic path armed for a later XCTest issue.
    preCredentialObservationAllowed = false
  }

  private func preCredentialElementFlags(_ name: String, _ element: XCUIElement) -> [String] {
    guard element.exists else {
      // Do not ask XCTest for frame or hittability of an absent element. Some
      // XCTest versions turn those follow-up queries into their own failure.
      return [
        "\(name)_exists=0",
        "\(name)_hittable=0",
        "\(name)_frame_available=0",
        "\(name)_frame_x=unavailable",
        "\(name)_frame_y=unavailable",
        "\(name)_frame_width=unavailable",
        "\(name)_frame_height=unavailable",
      ]
    }

    let frame = element.frame
    let frameValues = [frame.minX, frame.minY, frame.width, frame.height]
    let frameCoordinates = frameValues.map { value -> String in
      guard value.isFinite, value >= -1_000_000, value <= 1_000_000 else {
        return "unavailable"
      }
      return String(Int(value.rounded()))
    }
    let frameAvailable = !frame.isNull && !frame.isInfinite
      && frame.width > 0 && frame.height > 0
      && !frameCoordinates.contains("unavailable")
    return [
      "\(name)_exists=1",
      "\(name)_hittable=\(element.isHittable ? 1 : 0)",
      "\(name)_frame_available=\(frameAvailable ? 1 : 0)",
      "\(name)_frame_x=\(frameCoordinates[0])",
      "\(name)_frame_y=\(frameCoordinates[1])",
      "\(name)_frame_width=\(frameCoordinates[2])",
      "\(name)_frame_height=\(frameCoordinates[3])",
    ]
  }

  private func capturePreCredential(_ name: String) {
    let unavailableName = name + "-unavailable.txt"
    guard app.state == .runningForeground else {
      writeFixedArtifact(unavailableName, lines: ["reason=foreground_unavailable"])
      return
    }
    do {
      let data = app.screenshot().pngRepresentation
      try data.write(
        to: artifactDirectory.appendingPathComponent(name + ".png"),
        options: .atomic
      )
      try? FileManager.default.removeItem(at: artifactDirectory.appendingPathComponent(unavailableName))
    } catch {
      writeFixedArtifact(unavailableName, lines: ["reason=screenshot_write_failed"])
    }
  }

  private func writePreCredentialConnectionEntryDiagnostics(phase: String) {
    guard preCredentialObservationAllowed else { return }
    guard app.state == .runningForeground else {
      appendFixedArtifact(
        "ios-ui-ssh-entry-diagnostics.txt",
        lines: [
          "phase=\(phase)",
          "app_foreground=0",
        ]
      )
      return
    }
    let fixedLabel = { (label: String) in
      self.app.staticTexts.matching(NSPredicate(format: "label == %@", label)).firstMatch
    }
    let fixedButton = { (label: String) in
      self.app.buttons.matching(NSPredicate(format: "label == %@", label)).firstMatch
    }
    var lines = [
      "phase=\(phase)",
      "app_foreground=\(app.state == .runningForeground ? 1 : 0)",
    ]
    lines += preCredentialElementFlags("window", app.windows.firstMatch)
    lines += preCredentialElementFlags(
      "open_settings",
      app.buttons.matching(NSPredicate(format: "identifier == %@", "open-settings")).firstMatch
    )
    lines += preCredentialElementFlags("connect", fixedButton("Connect"))
    lines += preCredentialElementFlags("server_connection", fixedButton("Server connection"))
    lines += preCredentialElementFlags("host", input("Host"))

    let loadingLabels: [(String, String)] = [
      ("loading_servers", "Loading your servers…"),
      ("loading_workspaces", "Loading workspaces…"),
      ("connecting", "Connecting…"),
      ("authenticating", "Authenticating…"),
      ("opening_terminal", "Opening terminal…"),
      ("opening_workspace", "Opening workspace…"),
      ("restoring_terminals", "Restoring terminals…"),
      ("reconnecting", "Reconnecting…"),
    ]
    for (name, label) in loadingLabels {
      lines += preCredentialElementFlags(name, fixedLabel(label))
    }
    appendFixedArtifact("ios-ui-ssh-entry-diagnostics.txt", lines: lines)
  }

  private func openConnectionForm(observeInitialEntry: Bool = false) {
    if observeInitialEntry { beginPreCredentialConnectionEntryObservation() }
    if button("Connect").waitForExistence(timeout: 20) {
      button("Connect").tap()
    } else if button("Server connection").waitForExistence(timeout: 10) {
      button("Server connection").tap()
      guard button("Connect").waitForExistence(timeout: 10) else {
        recordPreCredentialConnectionEntryFailure()
        XCTFail("The connection menu did not open.")
        return
      }
      button("Connect").tap()
    } else {
      recordPreCredentialConnectionEntryFailure()
      XCTFail("The connection entry point is unavailable.")
      return
    }
    guard input("Host").waitForExistence(timeout: 15) else {
      recordPreCredentialConnectionEntryFailure()
      XCTFail("The SSH connection form did not open.")
      return
    }
    record("connection_form_opened")
  }

  private func fillTextField(label: String, value: String) {
    // Only non-secret short fields use readback. Never inspect or publish the
    // private-key editor's value through this helper.
    guard ["Host", "Port", "Username", "Server name", "Terminal font size", "Scrollback lines", "Workspace or terminal name"].contains(label) else {
      XCTFail("The short-field helper received an unsupported field.")
      return
    }
    closePreCredentialConnectionEntryObservation()
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
    // Defense in depth: the pre-credential observation must be closed before
    // any secret connection data enters the form.
    closePreCredentialConnectionEntryObservation()
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

  private func buttonStarting(with prefix: String) -> XCUIElement {
    let buttons = app.buttons.matching(
      NSPredicate(format: "identifier BEGINSWITH %@ OR label BEGINSWITH %@", prefix, prefix)
    )
    if buttons.count > 0 {
      for index in 0..<buttons.count {
        let candidate = buttons.element(boundBy: index)
        if candidate.exists && candidate.isHittable { return candidate }
      }
      return buttons.element(boundBy: buttons.count - 1)
    }
    return app.descendants(matching: .any).matching(
      NSPredicate(format: "identifier BEGINSWITH %@ OR label BEGINSWITH %@", prefix, prefix)
    ).firstMatch
  }

  private func terminalQuery() -> XCUIElementQuery {
    app.otherElements.matching(
      NSPredicate(
        format: "identifier == %@ OR label == %@ OR label == %@",
        "Terminal",
        "Terminal",
        "Terminal, cached output, read only"
      )
    )
  }

  private func connectedElement() -> XCUIElement {
    app.staticTexts.matching(
      NSPredicate(format: "identifier == %@ OR label == %@", "Connected", "Connected")
    ).firstMatch
  }

  private func terminalHasValidFrame(_ element: XCUIElement) -> Bool {
    guard element.exists else { return false }
    let frame = element.frame
    return !frame.isNull && !frame.isInfinite && frame.width > 0 && frame.height > 0
  }

  private func visibleTerminalElement() -> XCUIElement? {
    let visibleCandidates = terminalQuery().allElementsBoundByIndex.filter {
      terminalHasValidFrame($0)
    }
    return visibleCandidates.first(where: { $0.isHittable }) ?? visibleCandidates.first
  }

  private func waitForTerminalElement(timeout: TimeInterval) -> XCUIElement? {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if let terminal = visibleTerminalElement() { return terminal }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return visibleTerminalElement()
  }

  private func terminalElement() throws -> XCUIElement {
    guard let terminal = waitForTerminalElement(timeout: 30) else {
      XCTFail("The native terminal view is unavailable.")
      return terminalQuery().firstMatch
    }
    return terminal
  }

  private func waitForTerminal() -> Bool {
    waitForTerminalElement(timeout: 30) != nil
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

  private func terminalTabQuery() -> XCUIElementQuery {
    // Query the test ID, never the spoken label: a tab can share its display
    // name with another pane and "Terminal menu" is a separate control.
    app.descendants(matching: .any).matching(
      NSPredicate(format: "identifier BEGINSWITH %@", "terminal-tab-")
    )
  }

  private func visibleTerminalTabs() -> [XCUIElement] {
    var tabs: [String: XCUIElement] = [:]
    for element in terminalTabQuery().allElementsBoundByIndex {
      let identifier = element.identifier
      guard identifier.hasPrefix("terminal-tab-"), element.exists else { continue }
      if let current = tabs[identifier] {
        // React Native can expose a test ID through more than one wrapper in
        // the accessibility tree. Prefer the actionable/selected instance,
        // while retaining identity as the only deduplication key.
        if (element.isSelected && !current.isSelected) || (element.isHittable && !current.isHittable) {
          tabs[identifier] = element
        }
      } else {
        tabs[identifier] = element
      }
    }
    return tabs.values.sorted { $0.identifier < $1.identifier }
  }

  private func terminalTab(identifier: String) -> XCUIElement {
    visibleTerminalTabs().first(where: { $0.identifier == identifier })
      ?? terminalTabQuery().matching(
        NSPredicate(format: "identifier == %@", identifier)
      ).firstMatch
  }

  private func waitForTerminalTabs(minimum: Int) -> [XCUIElement] {
    let deadline = Date().addingTimeInterval(60)
    while Date() < deadline {
      let tabs = visibleTerminalTabs()
      if tabs.count >= minimum {
        return tabs
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return visibleTerminalTabs()
  }

  private func waitForSelected(_ element: XCUIElement) -> Bool {
    let deadline = Date().addingTimeInterval(30)
    while Date() < deadline {
      if element.isSelected { return true }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return element.isSelected
  }

  private func waitForBackground(timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if app.state == .runningBackground || app.state == .runningBackgroundSuspended {
        return true
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return app.state == .runningBackground || app.state == .runningBackgroundSuspended
  }

  private func waitForAuthoritativeReady(
    paneIdentifier: String,
    timeout: TimeInterval
  ) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    let pickerTitle = app.staticTexts.matching(
      NSPredicate(format: "label == %@ OR label == %@", "Choose a session", "Switch session")
    ).firstMatch
    let recoveryRail = recoveryElement("recovery-rail")
    let connected = connectedElement()
    let pickerRuntime = app.buttons.matching(
      NSPredicate(
        format: "identifier BEGINSWITH %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@ OR label BEGINSWITH %@",
        "runtime-row-", "tmux runtime ", "Herdr runtime ", "tmux session ", "Herdr session "
      )
    ).firstMatch
    while Date() < deadline {
      let selectedPane = terminalTab(identifier: paneIdentifier)
      if let terminal = visibleTerminalElement(),
         app.state == .runningForeground,
         connected.exists,
         !pickerTitle.exists,
         !pickerRuntime.exists,
         !recoveryRail.exists,
         selectedPane.exists,
         selectedPane.isSelected,
         terminalHasValidFrame(terminal) {
        return true
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    let selectedPane = terminalTab(identifier: paneIdentifier)
    guard let terminal = visibleTerminalElement() else { return false }
    return app.state == .runningForeground
      && connected.exists
      && !pickerTitle.exists
      && !pickerRuntime.exists
      && !recoveryRail.exists
      && selectedPane.exists
      && selectedPane.isSelected
      && terminalHasValidFrame(terminal)
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
      ("profile_name", "Use up to 80 characters, without control characters."),
      ("host", "Enter a hostname or IP address without spaces."),
      ("port", "Enter a port from 1 to 65535."),
      ("username", "Enter your SSH username without spaces."),
      ("private_key", "Paste an OpenSSH private key, including its BEGIN and END lines."),
      ("password", "Enter your SSH password."),
      ("submission_rejected", "Could not save or connect. Check the address and enter your credentials again."),
      ("submission_failed", "Could not save or connect. Enter your credentials again and retry."),
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
      ("discovering_runtimes", "Finding runtimes…"),
      ("awaiting_runtime_selection", "Choose a runtime"),
      ("attaching_runtime", "Opening runtime…"),
      ("creating_runtime", "Creating runtime…"),
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
      "Your host-key decision could not be sent. Connect again."
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

  private func writeRuntimePickerDiagnostics(
    phase: RuntimePickerFailurePhase,
    stage: String,
    pickerTitle: XCUIElement,
    runtime: XCUIElement,
    connected: XCUIElement
  ) {
    let observation = connectionStateObservation()
    let appForeground = app.state == .runningForeground
    // The Rust bridge already renders one bounded, non-secret connection
    // error. Match only known fixed product copy so diagnostics can identify
    // the native failure class without serializing the accessibility tree or
    // any credential-bearing XCTest description.
    let knownConnectionErrors: [(String, String)] = [
      ("network", "The SSH connection could not be established."),
      ("channel", "The SSH session channel could not be opened."),
      ("transport", "The SSH terminal transport stopped."),
      ("remote_closed", "The remote terminal closed the session."),
      ("tmux_failed", "The managed tmux session could not be opened."),
      ("tmux_protocol", "The tmux Control Mode stream was malformed."),
      ("tmux_runtime_missing", "The selected tmux session disappeared or its server was replaced. Choose a runtime again."),
      ("stale_connection", "The SSH connection was replaced."),
    ]
    let failureCodeHint = knownConnectionErrors.first { _, message in
      app.staticTexts[message].exists
    }?.0 ?? "unavailable"
    writeConnectionStateArtifact(observation)
    writeFixedArtifact(
      "ios-ui-runtime-picker-diagnostics.txt",
      lines: [
        "phase=\(phase.rawValue)",
        "stage=\(stage)",
        "app_foreground=\(appForeground ? 1 : 0)",
        "connection_state=\(observation.key)",
        "connection_state_label_present=\(observation.present ? 1 : 0)",
        "picker_title_exists=\(pickerTitle.exists ? 1 : 0)",
        "picker_title_hittable=\(pickerTitle.exists && pickerTitle.isHittable ? 1 : 0)",
        "tmux_runtime_exists=\(runtime.exists ? 1 : 0)",
        "tmux_runtime_enabled=\(runtime.exists && runtime.isEnabled ? 1 : 0)",
        "tmux_runtime_hittable=\(runtime.exists && runtime.isHittable ? 1 : 0)",
        "connected_exists=\(connected.exists ? 1 : 0)",
        "connection_error_code_hint=\(failureCodeHint)",
      ]
    )
    if safeForPostFormScreenshot() {
      capture("runtime-picker-failure")
    }
  }

  private func writeTerminalKeyboardDiagnostics(
    stage: String,
    character: Character,
    requestedKey: XCUIElement
  ) {
    let terminal = visibleTerminalElement() ?? terminalQuery().firstMatch
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

  private func writeTerminalPasteDiagnostics(phase: String, paste: XCUIElement) {
    // Only fixed states and booleans cross the artifact boundary. Never dump
    // accessibility descriptions, clipboard contents or the typed command.
    func state(_ element: XCUIElement) -> String {
      guard element.exists else { return "absent" }
      switch element.value as? String {
      case let value? where value == "Ready" || value.hasPrefix("Ready "): return "ready"
      case "Pasting": return "pasting"
      case nil: return "no_value"
      default: return "unexpected_value"
      }
    }
    let labelTarget = button("Paste")
    let keyboard = app.keyboards.firstMatch
    let terminal = visibleTerminalElement() ?? terminalQuery().firstMatch
    appendFixedArtifact(
      "ios-ui-terminal-paste-diagnostics.txt",
      lines: [
        "phase=\(phase)",
        "app_foreground=\(app.state == .runningForeground ? 1 : 0)",
        "connection_form_gone=\(connectionFormIsGone() ? 1 : 0)",
        "terminal_exists=\(terminal.exists ? 1 : 0)",
        "keyboard_exists=\(keyboard.exists ? 1 : 0)",
        "paste_exists=\(paste.exists ? 1 : 0)",
        "paste_hittable=\(paste.exists && paste.isHittable ? 1 : 0)",
        "paste_state=\(state(paste))",
        "label_target_is_native_control=\(labelTarget.exists && labelTarget.identifier == "terminal-paste" ? 1 : 0)",
        "label_target_state=\(state(labelTarget))",
      ]
    )
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
    // The completion value belongs to UIPasteControl, not an arbitrary child
    // or edit-menu action carrying the same visible "Paste" label.
    let paste = app.descendants(matching: .any).matching(identifier: "terminal-paste").firstMatch
    record("\(stage)_paste_exists")
    XCTAssertTrue(paste.waitForExistence(timeout: 10), "The terminal Paste action is unavailable.")
    record("\(stage)_paste_hittable")
    XCTAssertTrue(waitForHittable(paste, timeout: 10), "The terminal Paste action is not hittable.")
    let enabled = XCTNSPredicateExpectation(
      predicate: NSPredicate(format: "enabled == YES"), object: paste
    )
    XCTAssertEqual(XCTWaiter.wait(for: [enabled], timeout: 10), .completed, "The terminal Paste action is disabled.")
    let initialPasteState = paste.value as? String
    let initiallyReady = initialPasteState?.hasPrefix("Ready ") == true
    if !initiallyReady { writeTerminalPasteDiagnostics(phase: "before_tap", paste: paste) }
    XCTAssertTrue(initiallyReady, "The native paste completion generation is unavailable before tapping.")
    record("\(stage)_paste_tap")
    paste.tap()
    record("\(stage)_paste_tapped")
    // UIPasteControl loads the item provider asynchronously. Wait for the
    // native action's completion before clearing its source or sending Enter.
    let finished = XCTNSPredicateExpectation(
      predicate: NSPredicate(
        format: "value != %@ AND value BEGINSWITH %@",
        initialPasteState ?? "",
        "Ready "
      ),
      object: paste
    )
    let completion = XCTWaiter.wait(for: [finished], timeout: 10)
    writeTerminalPasteDiagnostics(phase: "after_tap", paste: paste)
    if completion != .completed && safeForPostFormScreenshot() {
      capture("terminal-paste-failure")
    }
    // A changed Ready generation proves that the native delivery callback ran.
    // The remote marker below remains the end-to-end transport proof.
    XCTAssertEqual(completion, .completed, "The native paste did not finish.")
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


  // MARK: - Issue #27 session switcher drive (session-side evidence)

  private func switchEnv(_ name: String) -> String {
    ProcessInfo.processInfo.environment[name] ?? ""
  }

  private func switchWaitForFileLines(_ path: URL, expected: [String], timeout: TimeInterval) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
      if let contents = try? String(contentsOf: path, encoding: .utf8) {
        let lines = contents.split(whereSeparator: { $0.isNewline }).map(String.init)
        if lines == expected { return true }
      }
      RunLoop.current.run(until: Date().addingTimeInterval(0.25))
    }
    return false
  }

  private func switchReadKey(_ path: String) -> String {
    (try? String(contentsOfFile: path, encoding: .utf8)) ?? ""
  }

  private func switchTrustHostKey(fingerprint: String, stage: String) -> Bool {
    let alert = app.alerts.firstMatch
    guard alert.waitForExistence(timeout: 60) else {
      record("\(stage)_host_trust_missing")
      XCTFail("The SSH host trust prompt did not appear for \(stage).")
      return false
    }
    XCTAssertTrue(
      alert.staticTexts.allElementsBoundByIndex.contains { $0.label.contains(fingerprint) },
      "The host trust prompt did not display the expected fingerprint for \(stage)."
    )
    let trust = alert.buttons["Trust and connect"].exists
      ? alert.buttons["Trust and connect"]
      : alert.buttons["Trust and continue"]
    guard trust.waitForExistence(timeout: 10), waitForHittable(trust, timeout: 10) else {
      XCTFail("The host trust action is unavailable for \(stage).")
      return false
    }
    trust.tap()
    return waitForDisappearance(alert, timeout: 10)
  }

  private func switchSelectRuntime(sessionName: String, stage: String) -> Bool {
    let pickerTitle = app.staticTexts["Choose a session"]
    guard pickerTitle.waitForExistence(timeout: 60) else {
      record("\(stage)_runtime_picker_missing")
      XCTFail("The runtime picker did not appear for \(stage).")
      return false
    }
    let runtime = button("tmux runtime \(sessionName)")
    guard runtime.waitForExistence(timeout: 30), waitForHittable(runtime, timeout: 30) else {
      record("\(stage)_runtime_row_missing")
      XCTFail("The \(sessionName) runtime row was not selectable for \(stage).")
      return false
    }
    let enabled = XCTNSPredicateExpectation(
      predicate: NSPredicate { _, _ in
        !((runtime.value as? String) ?? "").contains("disabled")
      },
      object: nil
    )
    guard XCTWaiter.wait(for: [enabled], timeout: 30) == .completed else {
      record("\(stage)_runtime_row_disabled")
      XCTFail("The \(sessionName) runtime row stayed disabled for \(stage).")
      return false
    }
    runtime.tap()
    let labels = waitForWorkspaceLabels(minimum: 1)
    guard !labels.isEmpty else {
      record("\(stage)_workspaces_timeout")
      let tree = app.debugDescription
      try? tree.write(
        to: artifactDirectory.appendingPathComponent("ios-issue27-\(stage)-timeout-tree.txt"),
        atomically: true,
        encoding: .utf8
      )
      XCTFail("The connection did not expose workspaces for \(stage).")
      return false
    }
    return true
  }

  private func switchConnect(
    host: String, port: String, username: String, keyFile: String,
    name: String, fingerprint: String, sessionName: String, stage: String
  ) -> Bool {
    openConnectionForm()
    fillTextField(label: "Host", value: host)
    fillTextField(label: "Port", value: port)
    fillTextField(label: "Username", value: username)
    let saveProfile = app.switches["save-server-profile"]
    guard revealAuthenticationControl(saveProfile, stage: "\(stage)_save_profile") else {
      XCTFail("The Save server profile control is unavailable for \(stage).")
      return false
    }
    if (saveProfile.value as? String) != "1" { saveProfile.tap() }
    XCTAssertEqual(saveProfile.value as? String, "1", "Server profile saving was not selected.")
    guard revealAuthenticationControl(input("Server name"), stage: "\(stage)_server_name") else {
      XCTFail("The Server name field is unavailable for \(stage).")
      return false
    }
    fillTextField(label: "Server name", value: name)
    let key = switchReadKey(keyFile)
    XCTAssertFalse(key.isEmpty, "The fixture private key is unavailable for \(stage).")
    guard revealAuthenticationControl(input("Private OpenSSH key"), stage: "\(stage)_key") else {
      XCTFail("Private-key authentication is unavailable for \(stage).")
      return false
    }
    fillPrivateKey(key)
    let saveCredentials = app.switches["save-credentials"]
    guard revealAuthenticationControl(saveCredentials, stage: "\(stage)_save_credentials") else {
      XCTFail("The Save credentials control is unavailable for \(stage).")
      return false
    }
    if (saveCredentials.value as? String) != "1" { saveCredentials.tap() }
    XCTAssertEqual(saveCredentials.value as? String, "1", "Credential saving was not selected.")
    let submit = app.buttons["ssh-submit"]
    guard waitForHittable(submit, timeout: 10) else {
      XCTFail("The Connect action is unavailable for \(stage).")
      return false
    }
    submit.tap()
    guard waitForConnectionFormDismissal(timeout: 30) else {
      writeConnectionFormDiagnostics()
      XCTFail("The SSH connection form did not dismiss for \(stage).")
      return false
    }
    guard switchTrustHostKey(fingerprint: fingerprint, stage: stage) else { return false }
    return switchSelectRuntime(sessionName: sessionName, stage: stage)
  }

  private func openWorkspaceTerminal(expectedName: String) -> Bool {
    let labels = waitForWorkspaceLabels(minimum: 1)
    guard !labels.isEmpty else {
      XCTFail("No workspace rows were exposed.")
      return false
    }
    let expected = "Workspace \(expectedName)"
    let target = labels.contains(expected) ? expected : labels[0]
    let row = button(target)
    if row.waitForExistence(timeout: 15) {
      guard waitForHittable(row, timeout: 10) else { return false }
      row.tap()
    }
    return waitForTerminal()
  }

  private func selectedTerminalPane() -> String {
    waitForTerminalTabs(minimum: 1).first(where: { $0.isSelected })?.identifier ?? ""
  }

  private func openSessionSwitcherSheet() -> Bool {
    if button("Close session switcher").waitForExistence(timeout: 3) { return true }
    var opener = buttonStarting(with: "Switch server and session")
    if !opener.waitForExistence(timeout: 8) {
      let back = button("Back to workspaces")
      if back.waitForExistence(timeout: 5), waitForHittable(back, timeout: 5) {
        back.tap()
        opener = buttonStarting(with: "Switch server and session")
      }
    }
    guard opener.waitForExistence(timeout: 15), waitForHittable(opener, timeout: 10) else {
      XCTFail("The session switcher header control is unavailable.")
      return false
    }
    opener.tap()
    return button("Close session switcher").waitForExistence(timeout: 20)
  }

  private func ensureWorkspaceTerminal(expectedName: String) -> Bool {
    if waitForTerminalElement(timeout: 10) != nil { return true }
    return openWorkspaceTerminal(expectedName: expectedName)
  }

  /// Saves a second server profile through the Server connection menu's
  /// Saved servers row → Add server (restored on 83575b9). This sheet→sheet
  /// path avoids the switcher-footer Manage servers route whose
  /// setSheet('servers') raced the formSheet dismissal and wedged on iOS.
  private func switchAddServerProfile(
    host: String, port: String, username: String, keyFile: String,
    name: String, stage: String
  ) -> Bool {
    if !button("Server connection").waitForExistence(timeout: 8) {
      let back = button("Back to workspaces")
      if back.waitForExistence(timeout: 5), waitForHittable(back, timeout: 5) {
        back.tap()
      }
    }
    let menu = button("Server connection")
    guard menu.waitForExistence(timeout: 10), waitForHittable(menu, timeout: 8) else {
      record("\(stage)_menu_missing")
      XCTFail("The Server connection menu is unavailable for \(stage).")
      return false
    }
    menu.tap()
    let saved = button("Saved servers")
    guard saved.waitForExistence(timeout: 15), waitForHittable(saved, timeout: 10) else {
      record("\(stage)_saved_row_missing")
      capture("sw-saved-row-missing")
      let tree = app.debugDescription
      try? tree.write(
        to: artifactDirectory.appendingPathComponent("ios-issue27-\(stage)-saved-row-tree.txt"),
        atomically: true,
        encoding: .utf8
      )
      XCTFail("The Saved servers menu row is unavailable for \(stage).")
      return false
    }
    saved.tap()
    let add = button("Add server")
    guard add.waitForExistence(timeout: 15), waitForHittable(add, timeout: 10) else {
      record("\(stage)_add_missing")
      capture("sw-add-f2-missing")
      let tree = app.debugDescription
      try? tree.write(
        to: artifactDirectory.appendingPathComponent("ios-issue27-\(stage)-add-missing-tree.txt"),
        atomically: true,
        encoding: .utf8
      )
      XCTFail("The saved-servers Add server action is unavailable for \(stage).")
      return false
    }
    add.tap()
    guard input("Host").waitForExistence(timeout: 15) else {
      record("\(stage)_form_missing")
      XCTFail("The Add server form did not open for \(stage).")
      return false
    }
    fillTextField(label: "Host", value: host)
    fillTextField(label: "Port", value: port)
    fillTextField(label: "Username", value: username)
    // The username keyboard can still cover the last form row; reveal it first.
    _ = revealAuthenticationControl(input("Server name"), stage: "\(stage)_server_name")
    fillTextField(label: "Server name", value: name)
    let key = switchReadKey(keyFile)
    XCTAssertFalse(key.isEmpty, "The fixture private key is unavailable for \(stage).")
    guard revealAuthenticationControl(input("Private OpenSSH key"), stage: "\(stage)_key") else {
      XCTFail("Private-key authentication is unavailable for \(stage).")
      return false
    }
    fillPrivateKey(key)
    let saveCredentials = app.switches["save-credentials"]
    guard revealAuthenticationControl(saveCredentials, stage: "\(stage)_save_credentials") else {
      XCTFail("The Save credentials control is unavailable for \(stage).")
      return false
    }
    if (saveCredentials.value as? String) != "1" { saveCredentials.tap() }
    XCTAssertEqual(saveCredentials.value as? String, "1", "Credential saving was not selected.")
    let submit = app.buttons["ssh-submit"]
    guard waitForHittable(submit, timeout: 10) else {
      XCTFail("The Save action is unavailable for \(stage).")
      return false
    }
    submit.tap()
    guard waitForConnectionFormDismissal(timeout: 30) else {
      record("\(stage)_save_stuck")
      XCTFail("The Add server form did not dismiss for \(stage).")
      return false
    }
    // The saved-servers sheet re-presents after the form; closing it returns
    // to plain workspaces (no deferred switcher intent on this menu path).
    let closeSheet = button("Close sheet")
    guard closeSheet.waitForExistence(timeout: 15), waitForHittable(closeSheet, timeout: 10) else {
      record("\(stage)_servers_sheet_stuck")
      XCTFail("The saved-servers sheet did not reopen for \(stage).")
      return false
    }
    closeSheet.tap()
    guard buttonStarting(with: "Switch server and session").waitForExistence(timeout: 20)
      || button("Server connection").waitForExistence(timeout: 5) else {
      record("\(stage)_workspaces_not_returned")
      XCTFail("The app did not return to workspaces after saving \(stage).")
      return false
    }
    return true
  }

  private func switcherSelectRuntime(serverLabel: String, sessionName: String, stage: String, fingerprint: String? = nil) -> Bool {
    let row = app.buttons.matching(
      NSPredicate(format: "label BEGINSWITH %@", "tmux session \(sessionName) on ")
    ).firstMatch
    if !row.waitForExistence(timeout: 20) {
      let server = button(serverLabel)
      guard server.waitForExistence(timeout: 15), waitForHittable(server, timeout: 10) else {
        record("\(stage)_server_row_missing")
        XCTFail("The \(serverLabel) server row is unavailable for \(stage).")
        return false
      }
      server.tap()
    }
    // A first-time browse may pause on host-key confirmation inside the
    // switcher before the session rows render.
    if let fingerprint {
      let alert = app.alerts.firstMatch
      if alert.waitForExistence(timeout: 20) {
        guard switchTrustHostKey(fingerprint: fingerprint, stage: stage) else { return false }
      }
    }
    guard row.waitForExistence(timeout: 60), waitForHittable(row, timeout: 15) else {
      record("\(stage)_browse_row_missing")
      XCTFail("The \(sessionName) browse row is unavailable for \(stage).")
      return false
    }
    let enabled = XCTNSPredicateExpectation(
      predicate: NSPredicate { _, _ in
        !((row.value as? String) ?? "").contains("disabled")
      },
      object: nil
    )
    guard XCTWaiter.wait(for: [enabled], timeout: 30) == .completed else {
      record("\(stage)_browse_row_disabled")
      XCTFail("The \(sessionName) browse row stayed disabled for \(stage).")
      return false
    }
    record("\(stage)_browse_row_tap")
    row.tap()
    let close = button("Close session switcher")
    _ = waitForDisappearance(close, timeout: 120)
    let landed = waitForTerminalElement(timeout: 60) != nil || !waitForWorkspaceLabels(minimum: 1).isEmpty
    guard landed else {
      record("\(stage)_switch_not_ready")
      let tree = app.debugDescription
      try? tree.write(
        to: artifactDirectory.appendingPathComponent("ios-issue27-\(stage)-notready-tree.txt"),
        atomically: true,
        encoding: .utf8
      )
      XCTFail("The switch did not land on a ready screen for \(stage).")
      return false
    }
    return true
  }

  /// Drives the unified Server + Session switcher against two live OpenSSH
  /// fixtures: same-server session switch, cross-server switch, and a
  /// cancel/no-destruction browse. Fixture-owned tmux sockets prove the
  /// released sessions survive remotely.
  func testIssue27SessionSwitcherDrive() throws {
    let host1 = requiredEnvironment("MEETERM_SSH_HOST")
    let port1 = requiredEnvironment("MEETERM_SSH_PORT")
    let user1 = requiredEnvironment("MEETERM_SSH_USERNAME")
    let fp1 = requiredEnvironment("MEETERM_SSH_FINGERPRINT")
    let key1 = requiredEnvironment("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
    let host2 = switchEnv("MEETERM_SSH2_HOST")
    let port2 = switchEnv("MEETERM_SSH2_PORT")
    let user2 = switchEnv("MEETERM_SSH2_USERNAME")
    let fp2 = switchEnv("MEETERM_SSH2_FINGERPRINT")
    let key2 = switchEnv("MEETERM_SSH2_UNENCRYPTED_PRIVATE_KEY_FILE")
    let workdir = URL(
      fileURLWithPath: switchEnv("MEETERM_SWITCH_WORKDIR").isEmpty
        ? NSTemporaryDirectory() : switchEnv("MEETERM_SWITCH_WORKDIR"),
      isDirectory: true
    )
    XCTAssertFalse(
      [host2, port2, user2, fp2, key2].contains(""),
      "The second-fixture switcher environment is incomplete."
    )
    let preMarker = workdir.appendingPathComponent("switcher-pre-f1.txt")
    let sameMarker = workdir.appendingPathComponent("switcher-same-server.txt")
    let crossMarker = workdir.appendingPathComponent("switcher-cross-server.txt")
    let cancelMarker = workdir.appendingPathComponent("switcher-cancel.txt")
    try? FileManager.default.createDirectory(at: workdir, withIntermediateDirectories: true)

    // The drive was not launched with -meeterm-ui-observation by setUp, so the
    // native paste control never exposes its "Ready N" completion generation
    // that enterTerminalCommand requires. Relaunch with the flag.
    app.terminate()
    app.launchArguments += ["-meeterm-ui-observation"]
    app.launch()
    XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30))

    record("sw_connect_fixture2")
    guard switchConnect(
      host: host2, port: port2, username: user2, keyFile: key2,
      name: "Fixture Two", fingerprint: fp2, sessionName: "meeterm-two", stage: "sw_f2"
    ) else { return }
    record("sw_disconnect_fixture2")
    tapConnectionAction("Disconnect")
    XCTAssertTrue(
      waitForDisconnected(),
      "The fixture-two connection did not disconnect before the same-server phase."
    )

    record("sw_connect_fixture1")
    guard switchConnect(
      host: host1, port: port1, username: user1, keyFile: key1,
      name: "Fixture One", fingerprint: fp1, sessionName: "meeterm", stage: "sw_f1"
    ) else { return }
    guard openWorkspaceTerminal(expectedName: "main") else { return }
    record("sw_f1_terminal_ready")
    guard let terminal1 = try? terminalElement() else { return }
    terminal1.tap()
    enterTerminalCommand(
      "printf '%s\\n' 'pre-f1' >> \(shellQuote(preMarker.path))",
      stage: "sw_f1_pre_marker"
    )
    XCTAssertTrue(
      switchWaitForFileLines(preMarker, expected: ["pre-f1"], timeout: 30),
      "The fixture-one pre-switch marker did not reach its pane exactly once."
    )
    let fixtureOnePane = selectedTerminalPane()
    XCTAssertFalse(fixtureOnePane.isEmpty)

    // Create the second profile BEFORE any committed switch, through the
    // Server connection menu's Saved servers row → Add server — a plain
    // sheet→sheet path that never touches the switcher-dismiss sheet race.
    record("sw_add_f2_menu")
    guard switchAddServerProfile(
      host: host2, port: port2, username: user2, keyFile: key2,
      name: "Fixture Two", stage: "sw_add_f2"
    ) else { return }

    // Item 1 — actual same-server session switch from the switcher.
    record("sw_same_server_select")
    XCTAssertTrue(openSessionSwitcherSheet(), "The session switcher did not open.")
    capture("switcher-open")
    guard switcherSelectRuntime(
      serverLabel: "Current server Fixture One", sessionName: "alt", stage: "sw_same"
    ) else { return }
    record("sw_same_server_committed")
    guard ensureWorkspaceTerminal(expectedName: "alt-main") else { return }
    XCTAssertTrue(connectedElement().waitForExistence(timeout: 30), "The same-server switch lost Connected.")
    let altPane = selectedTerminalPane()
    XCTAssertFalse(altPane.isEmpty)
    XCTAssertTrue(
      waitForAuthoritativeReady(paneIdentifier: altPane, timeout: 90),
      "The same-server switch did not restore Ready on the alt session."
    )
    guard let altTerminal = try? terminalElement() else { return }
    altTerminal.tap()
    enterTerminalCommand(
      "printf '%s\\n' 'post-alt' >> \(shellQuote(sameMarker.path))",
      stage: "sw_same_marker"
    )
    XCTAssertTrue(
      switchWaitForFileLines(sameMarker, expected: ["post-alt"], timeout: 30),
      "The same-server switch input did not reach the alt session pane."
    )
    capture("switcher-same-server-ready")
    // The released `meeterm` session must still be discovered by the app's
    // own browse — proof it survives remotely — then dismiss without a
    // second switch and confirm `alt` stays authoritative.
    XCTAssertTrue(openSessionSwitcherSheet(), "The switcher did not reopen after the same-server switch.")
    XCTAssertTrue(
      app.buttons.matching(
        NSPredicate(format: "label BEGINSWITH %@", "tmux session meeterm on ")
      ).firstMatch.waitForExistence(timeout: 60),
      "The released fixture-one session vanished from the browse: it was destroyed remotely."
    )
    capture("switcher-same-server-browse")
    let closeAfterSame = button("Close session switcher")
    XCTAssertTrue(waitForHittable(closeAfterSame, timeout: 10), "The switcher close control is unavailable.")
    closeAfterSame.tap()
    XCTAssertTrue(waitForDisappearance(closeAfterSame, timeout: 15), "The switcher did not dismiss after the aliveness check.")
    XCTAssertTrue(ensureWorkspaceTerminal(expectedName: "alt-main"), "Dismissing the re-browse lost the workspace terminal.")
    XCTAssertTrue(connectedElement().waitForExistence(timeout: 30), "Dismissing after the same-server check lost Connected.")
    XCTAssertEqual(selectedTerminalPane(), altPane, "Dismissing the re-browse rebound the authoritative terminal.")
    writeFixedArtifact("ios-issue27-same-server.txt", lines: [
      "case=same_server_switch result=passed",
      "released_session_alive=yes",
      "released_session_listed_in_browse=yes",
      "marker_roundtrip=exactly_once",
    ])

    // Item 2 — actual cross-server switch to the second fixture endpoint.
    // Fixture Two was saved through the switcher's own Manage servers →
    // Add server flow before the same-server commit (a second plain
    // connect would edit the current profile in place, not add a new one).
    record("sw_open_switcher_cross_server")
    XCTAssertTrue(openSessionSwitcherSheet(), "The session switcher did not reopen.")
    capture("switcher-cross-server-open")
    guard switcherSelectRuntime(
      serverLabel: "Server Fixture Two", sessionName: "meeterm-two", stage: "sw_cross",
      fingerprint: fp2
    ) else { return }
    record("sw_cross_server_committed")
    guard ensureWorkspaceTerminal(expectedName: "two-main") else { return }
    XCTAssertTrue(connectedElement().waitForExistence(timeout: 30), "The cross-server switch lost Connected.")
    let fixtureTwoPane = selectedTerminalPane()
    XCTAssertFalse(fixtureTwoPane.isEmpty)
    XCTAssertTrue(
      waitForAuthoritativeReady(paneIdentifier: fixtureTwoPane, timeout: 90),
      "The cross-server switch did not restore Ready on the second fixture."
    )
    guard let twoTerminal = try? terminalElement() else { return }
    twoTerminal.tap()
    enterTerminalCommand(
      "printf '%s\\n' 'post-two' >> \(shellQuote(crossMarker.path))",
      stage: "sw_cross_marker"
    )
    XCTAssertTrue(
      switchWaitForFileLines(crossMarker, expected: ["post-two"], timeout: 30),
      "The cross-server switch input did not reach the second fixture pane."
    )
    capture("switcher-cross-server-ready")
    // Browse the first server again: its `meeterm` session must still be
    // discovered after the cross-server bind, then dismiss the browse.
    XCTAssertTrue(openSessionSwitcherSheet(), "The switcher did not reopen after the cross-server switch.")
    let fixtureOneServer = button("Server Fixture One")
    XCTAssertTrue(fixtureOneServer.waitForExistence(timeout: 10), "The first server's row is missing from the switcher.")
    if waitForHittable(fixtureOneServer, timeout: 5) {
      fixtureOneServer.tap()
    }
    XCTAssertTrue(
      app.buttons.matching(
        NSPredicate(format: "label BEGINSWITH %@", "tmux session meeterm on ")
      ).firstMatch.waitForExistence(timeout: 60),
      "The first fixture's meeterm session vanished after the cross-server switch."
    )
    capture("switcher-cross-server-first-server-browse")
    let closeAfterCross = button("Close session switcher")
    XCTAssertTrue(waitForHittable(closeAfterCross, timeout: 10), "The switcher close control is unavailable.")
    closeAfterCross.tap()
    XCTAssertTrue(waitForDisappearance(closeAfterCross, timeout: 15), "The switcher did not dismiss after the cross-server check.")
    XCTAssertTrue(ensureWorkspaceTerminal(expectedName: "two-main"), "Closing the re-browse lost the workspace terminal.")
    XCTAssertTrue(connectedElement().waitForExistence(timeout: 30), "Closing the re-browse lost Connected.")
    XCTAssertEqual(selectedTerminalPane(), fixtureTwoPane, "The re-browse rebound the authoritative terminal.")
    writeFixedArtifact("ios-issue27-cross-server.txt", lines: [
      "case=cross_server_switch result=passed",
      "first_server_session_alive=yes",
      "first_server_session_listed_in_browse=yes",
      "marker_roundtrip=exactly_once",
    ])

    // Item 3 — browse another server, then cancel: workspace must be unchanged.
    record("sw_open_switcher_cancel")
    XCTAssertTrue(openSessionSwitcherSheet(), "The session switcher did not open for the cancel check.")
    let otherServer = button("Server Fixture One")
    if otherServer.waitForExistence(timeout: 10), waitForHittable(otherServer, timeout: 5) {
      otherServer.tap()
    }
    RunLoop.current.run(until: Date().addingTimeInterval(1.0))
    let close = button("Close session switcher")
    XCTAssertTrue(waitForHittable(close, timeout: 10), "The switcher close control is unavailable.")
    close.tap()
    XCTAssertTrue(waitForDisappearance(close, timeout: 15), "The session switcher did not dismiss.")
    XCTAssertTrue(ensureWorkspaceTerminal(expectedName: "two-main"), "Cancel lost the workspace terminal.")
    XCTAssertTrue(connectedElement().waitForExistence(timeout: 30), "Cancel lost the Connected state.")
    let cancelPane = selectedTerminalPane()
    XCTAssertEqual(cancelPane, fixtureTwoPane, "Cancelling the browse rebound the authoritative terminal.")
    guard let cancelTerminal = try? terminalElement() else { return }
    cancelTerminal.tap()
    enterTerminalCommand(
      "printf '%s\\n' 'post-cancel' >> \(shellQuote(cancelMarker.path))",
      stage: "sw_cancel_marker"
    )
    XCTAssertTrue(
      switchWaitForFileLines(cancelMarker, expected: ["post-cancel"], timeout: 30),
      "The workspace was not authoritative after dismissing the browse."
    )
    capture("switcher-cancel-authoritative")
    writeFixedArtifact("ios-issue27-cancel.txt", lines: [
      "case=cancel_no_destruction result=passed",
      "bound_terminal_unchanged=yes",
      "marker_roundtrip=exactly_once",
    ])

    record("sw_disconnect_final")
    tapConnectionAction("Disconnect")
    XCTAssertTrue(
      waitForDisconnected(),
      "The final explicit disconnect did not complete."
    )
    writeFixedArtifact("ios-issue27-switcher-validation.txt", lines: [
      "case=same_server_switch result=passed",
      "case=cross_server_switch result=passed",
      "case=cancel_no_destruction result=passed",
      "case=switcher_drive result=passed",
    ])
    record("sw_complete")
    app.terminate()
  }

  /// Minimal probe: connect fixture one, open the switcher, tap Manage
  /// servers, and snapshot the accessibility tree at intervals so the
  /// post-dismiss destination (servers sheet vs workspaces list) is visible.
  func testSwitcherManageProbe() throws {
    let host1 = switchEnv("MEETERM_SSH_HOST")
    let port1 = switchEnv("MEETERM_SSH_PORT")
    let user1 = switchEnv("MEETERM_SSH_USERNAME")
    let fp1 = switchEnv("MEETERM_SSH_FINGERPRINT")
    let key1 = switchEnv("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
    XCTAssertFalse([host1, port1, user1, fp1, key1].contains(""))
    let host2 = switchEnv("MEETERM_SSH2_HOST")
    let port2 = switchEnv("MEETERM_SSH2_PORT")
    let user2 = switchEnv("MEETERM_SSH2_USERNAME")
    let fp2 = switchEnv("MEETERM_SSH2_FINGERPRINT")
    let key2 = switchEnv("MEETERM_SSH2_UNENCRYPTED_PRIVATE_KEY_FILE")
    let workdir = URL(
      fileURLWithPath: switchEnv("MEETERM_SWITCH_WORKDIR").isEmpty
        ? NSTemporaryDirectory() : switchEnv("MEETERM_SWITCH_WORKDIR"),
      isDirectory: true
    )
    let preMarker = workdir.appendingPathComponent("switcher-pre-f1.txt")
    try? FileManager.default.createDirectory(at: workdir, withIntermediateDirectories: true)
    app.terminate()
    app.launchArguments += ["-meeterm-ui-observation"]
    app.launch()
    guard switchConnect(
      host: host2, port: port2, username: user2, keyFile: key2,
      name: "Fixture Two", fingerprint: fp2, sessionName: "meeterm-two", stage: "probe_f2"
    ) else { return }
    tapConnectionAction("Disconnect")
    XCTAssertTrue(waitForDisconnected(), "probe f2 did not disconnect.")
    guard switchConnect(
      host: host1, port: port1, username: user1, keyFile: key1,
      name: "Fixture One", fingerprint: fp1, sessionName: "meeterm", stage: "probe_f1"
    ) else { return }
    guard openWorkspaceTerminal(expectedName: "main") else { return }
    // Replicate the drive's terminal-input prefix to isolate the trigger.
    guard let terminal1 = try? terminalElement() else { return }
    terminal1.tap()
    enterTerminalCommand(
      "printf '%s\\n' 'pre-f1' >> \(shellQuote(preMarker.path))",
      stage: "probe_pre_marker"
    )
    XCTAssertTrue(
      switchWaitForFileLines(preMarker, expected: ["pre-f1"], timeout: 30),
      "probe pre-marker missing."
    )
    XCTAssertTrue(openSessionSwitcherSheet(), "The session switcher did not open.")
    let manage = button("Manage servers")
    XCTAssertTrue(manage.waitForExistence(timeout: 15), "Manage servers row missing.")
    // Is the row actually enabled (busy state disabled = hittable but inert)?
    record(manage.isEnabled ? "probe_manage_enabled" : "probe_manage_disabled")
    manage.tap()
    var samples: [String] = []
    let deadline = Date().addingTimeInterval(12)
    while Date() < deadline {
      let t = String(format: "%.1f", 12 - deadline.timeIntervalSinceNow)
      samples.append(
        "t=\(t) sheet=\(app.staticTexts["Saved servers"].exists) "
          + "switcher=\(button("Close session switcher").exists) "
          + "add=\(button("Add server").exists) "
          + "workspaces=\(app.staticTexts["Workspaces"].exists)"
      )
      RunLoop.current.run(until: Date().addingTimeInterval(0.2))
    }
    try? samples.joined(separator: "\n").write(
      to: artifactDirectory.appendingPathComponent("ios-probe-manage-samples.txt"),
      atomically: true,
      encoding: .utf8
    )
    let tree = app.debugDescription
    try? tree.write(
      to: artifactDirectory.appendingPathComponent("ios-probe-manage-tree-final.txt"),
      atomically: true,
      encoding: .utf8
    )
    capture("probe-manage-final")
    let add = button("Add server")
    record(add.exists ? "probe_add_present" : "probe_add_absent")
  }

  /// Supplementary acceptance: switcher footer Manage servers round-trip +
  /// focused sheet diagnostics (open/close, swipe-dismiss, hardware Escape).
  func testAc9SwitcherDiagnostics() {
    continueAfterFailure = false
    let host1 = switchEnv("MEETERM_SSH_HOST")
    let port1 = switchEnv("MEETERM_SSH_PORT")
    let user1 = switchEnv("MEETERM_SSH_USERNAME")
    let key1 = switchEnv("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
    let fp1 = switchEnv("MEETERM_SSH_FINGERPRINT")
    app.launchArguments += ["-meeterm-ui-observation"]
    app.launch()
    guard switchConnect(
      host: host1, port: port1, username: user1, keyFile: key1,
      name: "Fixture One", fingerprint: fp1, sessionName: "meeterm", stage: "ac9_f1"
    ) else { return }
    guard ensureWorkspaceTerminal(expectedName: "main") else { return }

    // Sheet open.
    XCTAssertTrue(openSessionSwitcherSheet(), "The session switcher did not open.")
    capture("ac9-switcher-open")
    record("ac9_sheet_open=yes")

    // C: footer Manage servers -> in-sheet Saved servers panel (embedded
    // SwitcherManage, 8c35313 contract). Coverage: 1) panel opens while the
    // sheet stays presented, 2) Add->dirty Back->'Discard changes?'
    // (Keep editing / Discard), 3) dirty form gates the sheet swipe,
    // 4) Add->Save->list row, 5) Edit + Remove, 6) server tap re-targets the
    // switcher, 7) Back->list then Close->origin (via the Escape/swipe flow).
    let manage = button("Manage servers")
    XCTAssertTrue(manage.waitForExistence(timeout: 15), "Manage servers footer missing.")
    XCTAssertTrue(manage.isEnabled, "Manage servers footer was disabled.")
    manage.tap()
    let serversAppeared = app.staticTexts["Saved servers"].waitForExistence(timeout: 15)
    record(serversAppeared ? "ac9_manage_sheet=yes" : "ac9_manage_sheet=no")
    capture(serversAppeared ? "ac9-manage-servers-sheet" : "ac9-manage-dropped")
    // The switcher route must remain presented: the panel owns the same
    // Close control and the sheet is never collapsed.
    record(button("Close session switcher").exists
      ? "ac9_switcher_after_manage=presented" : "ac9_switcher_after_manage=dropped")
    record(app.staticTexts["Fixture One"].exists
      ? "ac9_manage_profile_row=yes" : "ac9_manage_profile_row=no")
    if serversAppeared {
      // 2: Add -> type a host -> Back -> 'Discard changes?' -> Keep editing.
      let add = button("Add server")
      XCTAssertTrue(add.waitForExistence(timeout: 10), "Add server missing.")
      add.tap()
      XCTAssertTrue(app.staticTexts["Add server"].waitForExistence(timeout: 10),
                    "Add server form did not open inside the panel.")
      let hostField = input("Host")
      XCTAssertTrue(hostField.waitForExistence(timeout: 10), "Host field missing.")
      hostField.tap()
      hostField.typeText("ac9-guard.invalid")
      let backBtn = button("Back to saved servers")
      XCTAssertTrue(backBtn.waitForExistence(timeout: 10), "Save-form Back missing.")
      backBtn.tap()
      let discardAlert = app.alerts["Discard changes?"]
      let discardShown = discardAlert.waitForExistence(timeout: 10)
      record(discardShown ? "ac9_discard_prompt=yes" : "ac9_discard_prompt=no")
      capture("ac9-discard-prompt")
      if discardShown {
        discardAlert.buttons["Keep editing"].tap()
        record(app.staticTexts["Add server"].waitForExistence(timeout: 5)
          ? "ac9_keep_editing=yes" : "ac9_keep_editing=no")
        // 3: a dirty save form must gate the sheet swipe gesture.
        app.swipeDown(velocity: .fast)
        record(app.staticTexts["Add server"].waitForExistence(timeout: 3)
          ? "ac9_dirty_swipe_gated=yes" : "ac9_dirty_swipe_gated=no")
        capture("ac9-dirty-swipe-gated")
        backBtn.tap()
        let discardAgain = app.alerts["Discard changes?"]
        if discardAgain.waitForExistence(timeout: 10) {
          discardAgain.buttons["Discard"].tap()
        }
        record(app.staticTexts["Saved servers"].waitForExistence(timeout: 10)
          ? "ac9_discard_return=yes" : "ac9_discard_return=no")
      }

      // 4: Add -> Save -> the list returns with the new row.
      XCTAssertTrue(add.waitForExistence(timeout: 10), "Add server missing after discard.")
      add.tap()
      XCTAssertTrue(app.staticTexts["Add server"].waitForExistence(timeout: 10),
                    "Add server form did not reopen.")
      let host2 = input("Host")
      XCTAssertTrue(host2.waitForExistence(timeout: 10), "Host field missing on re-add.")
      host2.tap()
      host2.typeText("ac9-managed.invalid")
      let user2 = input("Username")
      XCTAssertTrue(user2.waitForExistence(timeout: 10), "Username field missing.")
      user2.tap()
      user2.typeText("ac9user")
      let name2 = input("Server name")
      if name2.waitForExistence(timeout: 3) { name2.tap(); name2.typeText("AC9 Managed") }
      button("Save server").tap()
      let savedRow = app.buttons["Connect saved server AC9 Managed"].waitForExistence(timeout: 15)
      record(savedRow ? "ac9_manage_save_row=yes" : "ac9_manage_save_row=no")
      capture(savedRow ? "ac9-manage-saved-row" : "ac9-manage-save-missing")

      // 5: options -> Edit opens the same form; Remove -> confirm -> row gone.
      let opts = button("Server options AC9 Managed")
      if opts.waitForExistence(timeout: 10) {
        opts.tap()
        let editBtn = button("Edit server")
        record(editBtn.waitForExistence(timeout: 8)
          ? "ac9_manage_options=yes" : "ac9_manage_options=no")
        if editBtn.exists && editBtn.isHittable {
          editBtn.tap()
          record(app.staticTexts["Edit server"].waitForExistence(timeout: 10)
            ? "ac9_manage_edit_form=yes" : "ac9_manage_edit_form=no")
          capture("ac9-manage-edit-form")
          // Clean form -> Back returns to the list without a prompt.
          let backClean = button("Back to saved servers")
          if backClean.waitForExistence(timeout: 8) { backClean.tap() }
          _ = app.staticTexts["Saved servers"].waitForExistence(timeout: 10)
        }
        let opts2 = button("Server options AC9 Managed")
        if opts2.waitForExistence(timeout: 8) {
          opts2.tap()
          let rm = button("Remove")
          if rm.waitForExistence(timeout: 8) {
            rm.tap()
            let rmAlert = app.alerts["Remove saved server?"]
            if rmAlert.waitForExistence(timeout: 8) {
              capture("ac9-manage-remove-confirm")
              rmAlert.buttons["Remove"].tap()
            }
            record(app.staticTexts["AC9 Managed"].waitForExistence(timeout: 3)
              ? "ac9_manage_remove=no" : "ac9_manage_remove=yes")
          }
        }
      }

      // 6: tapping a server returns to the switcher targeted at that server.
      let f1 = button("Connect saved server Fixture One")
      XCTAssertTrue(f1.waitForExistence(timeout: 10), "Fixture One row missing in Manage.")
      f1.tap()
      let reTargeted = app.staticTexts["Switch session"].waitForExistence(timeout: 15)
      record(reTargeted ? "ac9_manage_target=yes" : "ac9_manage_target=no")
      capture(reTargeted ? "ac9-manage-retarget" : "ac9-manage-retarget-missing")
      record(app.staticTexts["meeterm"].waitForExistence(timeout: 15)
        ? "ac9_manage_browse=yes" : "ac9_manage_browse=no")

      // 7: explicit Back restores the session list from Manage again.
      let manage2 = button("Manage servers")
      if manage2.waitForExistence(timeout: 10) {
        manage2.tap()
        let back2 = button("Back to session switcher")
        XCTAssertTrue(back2.waitForExistence(timeout: 10), "Back to session switcher missing.")
        _ = waitForHittable(back2, timeout: 10)
        back2.tap()
        let listBack = app.staticTexts["Switch session"].waitForExistence(timeout: 15)
        record(listBack ? "ac9_switcher_restored=yes" : "ac9_switcher_restored=no")
        capture(listBack ? "ac9-switcher-restored" : "ac9-switcher-not-restored")
      }
    } else {
      record("ac9_manage_defect=panel_missing")
      if !button("Close session switcher").waitForExistence(timeout: 2) {
        XCTAssertTrue(openSessionSwitcherSheet(), "Switcher did not reopen after the manage drop.")
        capture("ac9-switcher-reopened")
      }
    }

    // Hardware Escape (iOS keyboard dismiss; external-keyboard back analog).
    app.typeKey(.escape, modifierFlags: [])
    let escapeDismissed = !button("Close session switcher").waitForExistence(timeout: 4)
    record(escapeDismissed ? "ac9_escape_dismissed=yes" : "ac9_escape_dismissed=no")
    capture(escapeDismissed ? "ac9-escape-dismissed" : "ac9-escape-persisted")
    if escapeDismissed {
      XCTAssertTrue(waitForTerminalElement(timeout: 10) != nil || !waitForWorkspaceLabels(minimum: 1).isEmpty,
                    "Workspace not authoritative after Escape dismiss.")
      XCTAssertTrue(openSessionSwitcherSheet(), "Switcher did not reopen after Escape.")
    }

    // Swipe-to-dismiss on the presented sheet.
    app.swipeDown(velocity: .fast)
    let swipeDismissed = !button("Close session switcher").waitForExistence(timeout: 4)
    record(swipeDismissed ? "ac9_swipe_dismissed=yes" : "ac9_swipe_dismissed=no")
    capture(swipeDismissed ? "ac9-swipe-dismissed" : "ac9-swipe-persisted")
    if !swipeDismissed {
      let close = button("Close session switcher")
      if waitForHittable(close, timeout: 5) { close.tap() }
    }
    XCTAssertTrue(
      waitForTerminalElement(timeout: 15) != nil || !waitForWorkspaceLabels(minimum: 1).isEmpty,
      "Workspace not authoritative after sheet close."
    )
    capture("ac9-workspace-after-close")
    record("ac9_workspace_authoritative=yes")
    record("ac9_complete=yes")

    tapConnectionAction("Disconnect")
    XCTAssertTrue(waitForDisconnected(), "Final disconnect did not complete.")
  }

}
