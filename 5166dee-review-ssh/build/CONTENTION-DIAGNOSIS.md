# Standard-timeout diagnosis — 5166dee (observed at ssh-suite time; read-only, no disruption)

## Timing regression vs 1351186 (same VM, earlier today)
- Route phase: ~199s -> ~526s; theme_keyboard_open reached at 751.7s vs deadline 833.2s.
- Per-stage deltas this run: os_dark 9.5s, os_light 1.4s, pinned_dark 16.1s, pinned_light 5.5s, pinned_retained 4.9s — each several x slower than the POSIX run.
- Pattern: broad slowdown across launch/routes/theme legs, not isolated to keyboard.

## VM state observed during subsequent ssh suite (unchanged env since standard)
- 12 cores / 16GB. Load average ~78 (1/5/15m 78.26/78.33/84.98) — ~6.5x oversubscribed.
- TWO booted simulators: iPhone 18 Pro Max 7EE2EBC1 (suite target) AND iPhone Air DE19DC37 (leftover, booted ~24min earlier).
- Two SpringBoard instances + two sim-runtime diagnosticd; diagnosticd PIDs at 25-80% CPU for ~43min; host diagnosticd also ~34%.
- mds/mds_stores indexing (Spotlight) 5-7% — likely churning ~19GB of just-deleted temp dirs.
- rustc compile of meeterm-core overlapped (ssh fixture preflight).
- Memory: 79% free, swapouts=0 — CPU-bound, not memory-bound.
- Disk: recovered to 22Gi free before standard suite ran; disk_full=0 in suite diagnostics.

## Assessment
- Most probable cause of the broad slowdown: CPU oversubscription from the leftover second booted simulator (SpringBoard+diagnosticd+log churn) compounded by Spotlight indexing the deleted ~19GB and concurrent rustc — all concurrent during the standard XCTest. UNCERTAIN whether any single factor dominates; per-process historical CPU attribution was not captured during the standard run.
- Leftover iPhone Air likely booted at Simulator.app launch earlier in session; not created by this run.
- No deadline/assertion change made; result stands as FAILED at xcodebuild_timeout.
