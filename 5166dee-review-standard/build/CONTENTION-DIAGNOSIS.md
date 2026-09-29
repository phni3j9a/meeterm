# Standard-timeout diagnosis — 5166dee (v2, corrected temporal attribution)

## Timing regression vs 1351186 (same VM, earlier same day)
- Route phase ~199s -> ~526s; theme_keyboard_open at 751.7s vs 833.2s deadline.
- Per-stage deltas: os_dark 9.5s, os_light 1.4s, pinned_dark 16.1s, pinned_light 5.5s, pinned_retained 4.9s — each several x slower.
- Pattern: broad slowdown across launch/routes/theme legs, not keyboard-isolated.

## Observations (collection times explicit)
- Collected during the subsequent SSH-suite window (standard already ended): load avg ~78 on 12 cores (1/5/15m 78.26/78.33/84.98); TWO booted sims — iPhone 18 Pro Max 7EE2EBC1 (target) + iPhone Air DE19DC37 (leftover, no meeterm install, not referenced by any validation run); dual SpringBoard+diagnosticd 25-80% CPU; mds/mds_stores indexing ~19GB just-deleted temp dirs; rustc compiling meeterm-core — NOTE: rustc was observed during the SSH fixture preflight and does NOT establish overlap with the earlier standard UI run.
- Memory 79% free, swapouts=0; disk_full=0 in suite diagnostics; 22Gi free before standard ran.
- Load-average is a decaying measure; instantaneous check later (20:38) showed 71% idle while 1-min load still ~22 — residue, not live saturation at that instant.

## Remediation performed (this session, validation-owned only)
- `xcrun simctl shutdown DE19DC37` (unused Air) — no meeterm install; ~20:20.
- `xcrun simctl shutdown` then `boot` of 7EE2EBC1 (authorized) — cleared its diagnosticd log-drain backlog; ~20:38-20:39.
- No host daemons killed; mds/mediaanalysisd/XProtect/logd left to settle naturally.

## After-measurements
- ~20:46: load 10.2 (1-min, below 12 cores; 5-min 31 decaying); instantaneous CPU 93% idle (top -l 2 second sample); only fresh-sim diagnosticd 28% + apsd 12% + host diagnosticd 11% settling; mediaanalysisd dropped below top-6.
- Verdict: measured contention resolved at ~20:46 → ONE controlled standard-r2 rerun authorized by Main proceeded.

## Assessment (hypotheses, not proof)
- Most probable contributor to the broad standard-run slowdown: CPU oversubscription from the leftover second booted simulator (SpringBoard+diagnosticd+log churn) plus Spotlight/mediaanalysisd indexing of the ~19GB deleted dirs; possibly other transient churn not captured.
- Historical per-process CPU attribution during the standard XCTest window was not recorded; relative contribution of each factor is undetermined. The run-1 failures are preserved as real results; r2 is an environment-controlled rerun, not a pass claim.
