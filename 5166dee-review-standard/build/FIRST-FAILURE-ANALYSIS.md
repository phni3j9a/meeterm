# First-failure analysis — 5166dee review run

## 1. Preflight (attempt 1) — FAILED (failures=1); root cause UNDETERMINED
- Command: `scripts/ci/ios-typecheck.sh` (tail invokes `MEETERM_SWIFTC=$(xcrun --find swiftc) python3 scripts/ci/test_ios_selection_text.py`).
- Attempt-1 retained evidence: summary line only — `Ran 2 tests ... FAILED (failures=1)`. The failing test name, assertion, and compiler output were discarded by pipeline tail truncation. **Root cause cannot be determined from retained output.**
- The file contains two tests: `test_terminal_and_agent_readiness_use_stable_accessibility_owners` (source-string assertions) and `test_copy_handles_pane_deletion_and_selection_changes` (compiles production `selectionText` into a tempdir binary, runs 8 fault-injection cases). Which one failed is not established.
- ENOSPC is a plausible inference only: the disk was later confirmed 100% full at build attempt 1 (exit 65). Not proven for preflight.
- Attempt 2 (identical command, -v): both tests `... ok`, `Ran 2 tests in 6.120s / OK` — captured in full in this bundle as separate evidence; it does not establish the attempt-1 cause.

## 2. build-for-testing
- Attempt 1: exit 65 — `unable to open output file '...ModuleCache.../UserNotifications-*.pcm': No space left on device` (EXJSONUtils ScanDependencies); xcactivitylog save also failed ENOSPC. df: 123Gi total / 116Mi avail (100%). Retained log tail in bundle.
- Cleanup scope: deleted this session's own prior-run temp dirs (~19GB): runner-temp-ios37{,-r2,-r3,-529supp,-r3-ssh}, runner-temp-pr44, runner-temp-issue45{,-ssh,-posix,-posix-ssh}, runner-temp, native/meeterm-core/target (rebuildable). All suites needing those products had completed; evidence already pushed.
- Attempt 2 (same RUNNER_TEMP/flags): TEST BUILD SUCCEEDED. Recorded as a SECOND attempt; not first-attempt success.

## 3. standard suite — FAILED at xcuitest_standard: xcodebuild_timeout
- xcodebuild_elapsed_ms=834990 vs xcodebuild_timeout_ms=833233; last stage `theme_keyboard_open`.
- Diagnostics: disk_full=0, device_preparation_failed=0, testing_cancelled=0 (standard + storage).
- Completed: storage 6/6 passed; appearance observer passed; all 26 standard routes captured; theme_matrix_complete; settings_preview + recovery captures; os flips (dark/light retained); pinned retained; theme-transitions.mp4 captured.
- Not reached: theme_keyboard completion, all 11 theme_dialog legs, theme_verification_complete, foundation_verified, standard_complete; ios-native-input-validation.txt not produced by the suite.
- Cause of slowness UNDETERMINED: same suite completed ~8-9min on candidates 690b09b2/1351186 earlier today; this attempt hit the unchanged 833s deadline mid-keyboard leg. No retry performed.
