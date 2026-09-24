# RUN RECORD — iOS final acceptance on be7e23e

Source: `be7e23e0e9df5320ae0b601ba7ad96af7b019521` (be7e23e), branch
issue-27-session-switcher — FP-010 fix (`noticeKey` scrolls the single
FlatList owner to top on a new failure notice; no structural change vs
461d2a7).

Toolchain: Xcode 26.6 (17F113), SDKROOT iphonesimulator26.5, ARCHS=arm64,
Rust 1.96.0, Node 22.23.2/npm 10.9.8, CocoaPods 1.17.0, tmux 3.7c.
React Native 0.86.3 / Expo ~57.0.24 (package.json — corrected record).

Products: fresh clean CNG build-for-testing
(`EXPO_PUBLIC_MEETERM_SMOKE=1`, `ENABLE_TESTABILITY=YES`).
products-archive sha256
`db7f772bcd549482e489d2a78bed49ba63bd2224b2456c3c000377c5f8ef29af`
(the `sha256` field inside products-manifest.json).
products-manifest.json FILE sha256
`4624a9102f9538200c9124122e6e7b23935d63f03c0a28fdef4988005fe59d05`.

Simulator: iPhone 18 Pro Max 7EE2EBC1-0640-4950-AAFF-B4CDF455F377, iOS 27.0.
simulator.txt may still list the iPhone 18 Pro auto-boot record — all stages
targeted the 18 Pro Max via explicit destination id.

## Suite results (all on the same be7e23e products)

| Stage | Result | Evidence |
|---|---|---|
| standard | PASSED | `case=standard result=passed`; 13/13 UI tests + storage/native; all 45 manifest captures incl. 20 session-switcher-* routes; foundation `result=passed`, `renderer_backend=metal` |
| ssh | PASSED | `case=ssh result=passed`; `transport_loss=passed` + all invariants (same pane/native-terminal/selected-pane identifiers, cached read-only surface, input during loss=none, fixture control stopped+started) |
| polish | PASSED | `case=polish result=passed`; 7 presentation states + native nav/keyboard/back; foundation `result=passed` metal |
| testIssue27SessionSwitcherDrive | PASSED | 704.4s, `sw_complete`; same_server/cross_server/cancel_no_destruction/switcher_drive all `passed`; remote proof `socket1=alt,meeterm`, `socket2=meeterm-two` |
| testAc9SwitcherDiagnostics | FAILED | `XCTAssertTrue failed - Fixture One row missing in Manage` (MeetermSmokeUITests.swift:4068) — blocked by the embedded-form defect below; identical signature on 461d2a7 and be7e23e run-1 |

## Known product defect (verified, recorded — not worked around)

Embedded Add/Edit server form inside the Manage panel renders its scroll
content ~90px too high: "Back"/"Save" paint inside the formSheet grabber zone
and the intro collides under the heading. The Back button's painted position
is in the grabber dead zone — XCTest taps AND three real GUI taps on the
label all failed to close the form; the only exit is dismissing the sheet.
Settled state (still overlapped at 8s interactive). Same (0,0) mechanism
class as the list defect — the form's ScrollView is not a direct child of
the sheet content wrapper. REGRESSION of the 461d2a7 restructure (ba707cd ran
the identical steps clean). AC9 blocks at steps 5b–7 (remove-confirm,
retarget, restored) because the list never remounts. Full diagnosis +
captures: `manage-overlap-diagnosis/be7e23e-verification/`.

The first-entry Manage LIST overlap is FIXED and verified on this build
(clean initial + settled captures, XXXL included — see
`manage-overlap-diagnosis/be7e23e-verification/entry-*.png`).

## Harness anomalies during this run (test-side, not product)

1. xcodebuild `test-without-building` intermittently fails to exit after the
   suite finishes (post-restart environment): standard attempt 1 and the
   #27/AC9 drive runs completed their tests (`*_complete` + teardown) but
   xcodebuild lingered to the 900s cap / had to be killed. Recorded as
   `xcodebuild_timeout` at the script level — the XCTest results inside are
   complete and valid. For standard, the script's two post-steps
   (foundation log query + ios-validate-foundation.py) were run manually
   against the same artifact dir with the same commands and PASSED.
   ssh and polish exited xcodebuild cleanly (exit 0 end-to-end).
2. Stale `.xcresult` bundles from a killed run block the next invocation
   (`Existing file at -resultBundlePath`). Cleared before each re-run.
3. `polish` attempt 1 flaked at workspace search ("Main" typed while the iOS
   first-use keyboard tip was up — filtered row never appeared); re-run
   passed 84s clean. Attempt-1 snapshot kept under `polish-attempt1/`.
4. `ios-switcher-drive.sh` / `ios-ac9-drive.sh` did not clear
   `switcher-drive-work/` between runs — remote markers append (`>>`) so a
   previous run's `pre-f1` line broke the "exactly once" assertion.
   Scripts now `rm -rf "${WORKDIR:?}/"*` at start (see drive-tooling).
   Drive attempt 1 failed on that stale marker; attempt 2 passed 704.4s.

## Per-suite artifact roots

- `standard/` — all captures + validation/diagnostics/metadata (attempt-5 run;
  `standard-attempt1/` sibling snapshot preserved under obs-preserve on the
  runner, XCTest result identical)
- `ssh/` — ssh suite artifacts incl. transport-loss record
- `polish/` — polish captures + validations (attempt 2)
- `issue27-drive/` — stages, remote-session proof, validation, raw xcodebuild
  log, `drive-tooling/` (fragment source, scripts incl. workdir-clean fix,
  post-splice sha256, products manifest, xctestrun copies)
- `ac9/` — stages (green through `ac9_manage_edit_form=yes` then teardown),
  fresh captures through the defect step, raw log; captures past the defect
  step renamed `stale-2e4b3e3-ac9-*` (leftover from the last fully-passing
  AC9 run on 2e4b3e3 — NOT this build's output)
- Overlap saga evidence: `../manage-overlap-diagnosis/` (diagnosis on
  2e4b3e3, ba707cd failed-verification, be7e23e verification + the new
  form-defect record)

sysdiagnose: skipped — wedged collector subsystem (simctl diagnose hits the
600s cap; watchdog terminates ~46s in). Tests complete before diagnostics;
results unaffected.
