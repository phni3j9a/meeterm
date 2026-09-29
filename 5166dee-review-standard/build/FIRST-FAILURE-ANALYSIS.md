# First-failure analysis — 5166dee review run (corrected)

## 1. Preflight (attempt 1) — FAILED (failures=1); root cause UNDETERMINED
- Command: `scripts/ci/ios-typecheck.sh` (tail invokes `MEETERM_SWIFTC=$(xcrun --find swiftc) python3 scripts/ci/test_ios_selection_text.py`).
- Attempt-1 retained evidence: `preflight-attempt1-summary.txt` — only `Ran 2 tests ... FAILED (failures=1)`; the failing test name, assertion, and compiler output were discarded by pipeline tail truncation (logging gap).
- The file has two tests; which one failed is not established. ENOSPC is a plausible inference only (disk confirmed 100% full minutes later at build attempt 1) — not proven.
- Attempt 2 (identical command, -v): full retained output in `preflight-attempt2.txt` (both tests ok, 6.120s). The green rerun does not establish the attempt-1 cause.

## 2. build-for-testing — attempt 1 exit 65 (ENOSPC), attempt 2 SUCCEEDED
- Attempt 1 retained tail: `build-attempt1-tail.txt` — `unable to open output file .../UserNotifications-*.pcm: No space left on device`; xcactivitylog save also ENOSPC; df 116Mi avail.
- Cleanup scope: ~19GB of this session's own prior-run temp dirs deleted (all suites done, evidence pushed). 26Gi reclaimed.
- Attempt 2: same RUNNER_TEMP/flags → TEST BUILD SUCCEEDED (xcodebuild.log). Recorded as a SECOND attempt.

## 3. standard suite — FAILED at xcuitest_standard: xcodebuild_timeout
- elapsed 834990ms > deadline 833233ms; last stage `theme_keyboard_open`. disk_full=0, device_preparation_failed=0, testing_cancelled=0.
- Completed: storage 6/6, appearance pass, 26 routes, theme matrix, settings/recovery/os/pinned legs, mp4 234s. Not reached: keyboard completion, 11 theme_dialog legs, verification/foundation/complete markers; ios-native-input-validation.txt absent.
- Broad slowdown across phases vs 1351186 — see CONTENTION-DIAGNOSIS.md. Cause: CPU oversubscription (load ~78, leftover second booted simulator + Spotlight indexing + concurrent rustc) is the most probable contributor; historical per-process attribution unavailable, so contributing factors remain uncertain. No retry performed.
