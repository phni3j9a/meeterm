# Failure analysis — `99855a25` standard run

## Result shape

The XCTest itself **passed completely** — `ios-ui-stages.txt` reaches
`theme_verification_complete` → `foundation_launch` → `foundation_verified` →
`standard_complete` → `teardown_complete`, and
`ios-standard-validation.txt` records `result=passed`, `stage=complete`.
All 11 `theme-dialog-*` captures exist (dark + light legs incl. ActionSheet,
draft, discard Alert, and system inheritance legs — the round-1
`button.value` fix works: every draft/applied assertion passed).

The suite then failed at its **post-test terminal gate**:
`ios-foundation-validation.txt` → `result=failed reason=malformed_marker`
(`ios-validate-foundation.py`, xcodebuild/ script exit 1).

## Root cause (verified, deterministic)

`scripts/ci/ios-validate-foundation.py` treats any unrecognized
`MEETERM_SMOKE_*` line in the foundation-window `simulator.log` as
`malformed_marker`. Its whitelist (`MARKER_NAMES` ∪ `DIAGNOSTICS`) covers
STARTUP / INPUT_* / PASTE_* / NATIVE_READY / FIRST_FRAME_* — but **not**
`MEETERM_SMOKE_THEME`.

The candidate adds `NSLog("MEETERM_SMOKE_THEME %@", "light"/"dark")` in
`modules/meeterm-terminal/ios/MeetermTerminalView.swift:402` (emitted on every
terminal mount, including the foundation relaunch). The captured
`simulator.log` contains exactly one `MEETERM_SMOKE_THEME dark` line inside
the window — marker census:

```
 1 MEETERM_SMOKE_FIRST_FRAME_METAL   (whitelisted, fresh pid)
 1 MEETERM_SMOKE_NATIVE_READY       (whitelisted, fresh pid)
 1 MEETERM_SMOKE_THEME              (NOT whitelisted → malformed_marker)
14 MEETERM_SMOKE_STARTUP            (whitelisted phases)
 5 MEETERM_SMOKE_INPUT_*            (whitelisted)
```

Re-running the validator standalone on the captured log reproduces
`malformed_marker` deterministically. Android's validator already accepts the
same marker (`scripts/ssh/android_smoke_impl.py:88`
`TERMINAL_THEME_PATTERN = r"MEETERM_SMOKE_THEME (light|dark)"`); the iOS
foundation validator was not updated. Every standard run on this branch will
fail here regardless of app behavior — the foundation evidence itself
(NATIVE_READY + FIRST_FRAME_METAL from the fresh pid inside the window) is
present and correct in `suite/simulator.log`.

This is the second suite-side defect surfaced by this branch's validation;
the first (unreachable `staticTexts` asserts) was fixed by this candidate and
is verified working.

## Scope notes

- No source edits; suite budget/assertions untouched; single invocation, no
  rerun.
- All upstream gates passed: 26/26 routes, 6/6 theme matrix pairs,
  settings preview, recovery rail, real OS flips (same handle), pinned
  terminal, keyboard leg, all dialog legs, storage+input XCTest bundles.
