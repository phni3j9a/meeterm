# Issue #45: Herdr compatibility contract

The production contract is protocol 22, API schema 1, the JSON methods used by
meeterm, and the list/status/direct-control CLI. SemVer and absolute executable
path equality are not connection gates. Host authentication, selected runtime,
stable terminal identity, ordinary controller acquisition without takeover, and
the first authoritative full frame remain required for retained recovery.

The bundled schema advertises JSON methods. Read-only CLI help probes cover the
list/status/control entry points and controller target/size options. Actual
list/status, direct stream-local subscription/snapshot, lease and frame handling
are verified when used. Input/resize/scroll/release are protocol 22 semantics;
discovery never sends these mutations merely to probe support. Unknown additive
schema fields/methods are allowed; required method omissions are rejected.

## Reproducible fixtures

| Verified fixture | Official Linux x86_64 SHA-256 | Role |
| --- | --- | --- |
| Herdr 0.9.0 | `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f` | Existing full native integration baseline |
| Herdr 0.9.1 | `2a02fed16beb651ef006e1d43f048f652ca4dc58ad053cd2d44450563d5c54b7` | Additional compatibility/recovery CI fixture |

These are tested fixtures, not a production version allowlist or a minimum
supported release. The tests use isolated Herdr processes and a test-only russh
endpoint; they do not modify user Herdr installations or sessions.

The new live regression advertises incompatible protocol/schema/missing-method
candidates before the compatible binary, changes the endpoint's resolved CLI
alias during transport loss, verifies the same native terminal and input after
recovery, then advertises only an incompatible replacement and requires a
stopped read-only state. Alias and negative-schema substitutions are explicit
test endpoint behavior; the actual runtime, controller, frames and input use
the official binary.

## Validation record

Local development checks on 2026-09-28:

- Rust: 238 unit tests, 4 Herdr fixture/parser tests, 1 tmux layout test passed;
  opt-in live tests are reported separately. `cargo fmt --check` and Clippy
  with `-D warnings` passed.
- TypeScript typecheck and App tests: 130 passed.
- Herdr Python driver regression: 6 passed.
- SHA-verified Herdr 0.9.0: all 4 ignored native integration cases passed
  (21.86 seconds), including the new compatibility/recovery case.
- SHA-verified Herdr 0.9.1: all 4 ignored native integration cases passed
  (22.06 seconds). Real OpenSSH/tmux session integration passed (27.31 seconds).

Initial GitHub CI on `690b09b2` exposed a race in the new standalone test:
[push run 36438261747](https://github.com/phni3j9a/meeterm/actions/runs/36438261747)
failed its 0.9.0 compatibility marker, while
[PR run 36438394226](https://github.com/phni3j9a/meeterm/actions/runs/36438394226)
passed all 0.9.0 cases and failed the 0.9.1 compatibility marker. The test called
`set_terminal_visible(true)` and immediately sent input while that accepted
visibility transition temporarily closed the actor gate. It now waits for the
existing `control.terminalInputReady` acknowledgment before the first marker.
The input/identity/recovery assertions and deadlines are unchanged. This fixes
only the Rust integration test, not product or mobile-suite code.

The correction is `a3e43dff82aa5123a8184afa2fa05a26844da699`.
[Rust CI job 108986817107](https://github.com/phni3j9a/meeterm/actions/runs/36439742190/job/108986817107)
passed: 238 unit cases, 4 fixture/parser cases, 1 layout case, real OpenSSH/tmux
(29.10 seconds), all 4 SHA-verified Herdr 0.9.0 live cases (22.35 seconds), and
the SHA-verified 0.9.1 compatibility/recovery case (3.14 seconds). JavaScript/Expo
and iOS Swift preflight passed in the same workflow. Android native build
(job `108986817419`, 13m03s) also passed; all four workflow jobs succeeded.

The later evidence-only push `1f642e4` exposed an existing unit-fixture race in
[`36444092822`, Rust job `109001765837`](https://github.com/phni3j9a/meeterm/actions/runs/36444092822/job/109001765837):
`stopped_retry_restores_retained_work_without_reenabling_automatic_retries`
expected Reconnecting after starting a real connection to closed local port 1.
An immediate connection refusal could legitimately finish that actor as Stopped
before the test inspected it. A temporary 20 ms scheduling delay reproduced the
same failure locally; that diagnostic delay was removed. The fixture now binds
an isolated TCP listener, waits at most one second for the replacement's actual
connection, and holds its peer before the SSH banner while checking the original
state/identity/duplicate-Retry assertions. No product retry or deadline changed.
The corrected case passed 100/100 invocations without retries; the full local
238-unit/4-parser/1-layout suite, formatting and Clippy also passed. This change
is confined to the existing `#[cfg(test)]` module. Final follow-up CI is linked
from [PR #46](https://github.com/phni3j9a/meeterm/pull/46).

Initial mobile product source is `690b09b2e1ad097ad2747922c8d48a04b32c48bd`.
Changes through `91934c8` are evidence documentation and the standalone integration/unit
test corrections above. Code compiled into the mobile library, mobile suites,
CNG and build configuration remain unchanged. The production portion of
`ssh.rs` before its `#[cfg(test)] mod tests` is byte-identical to that candidate
(SHA-256 `9b83ac6fac97379bc500865cbd4cf931dd82250bdf10eefd62e1ed02f2becd4a`).
The initial mobile runs used that exact original candidate; iOS standard-to-ssh product
reuse matched that commit and toolchain as recorded below.
The SWE-2 Max mobile sessions are
[Android](https://app.devin.ai/sessions/9429c00e8cc14fb2b140b3e23bb28ec1) and
[iOS](https://app.devin.ai/sessions/7a32a4e6ed984961b5194e22feeba407).

- iOS `standard` passed on the exact mobile source above with fresh clean CNG
  and unsigned build-for-testing. Evidence:
  [`evidence/ios-20260928-issue45-standard`](https://github.com/phni3j9a/meeterm/tree/79a684c21639c93fde495805aaa2aedb9bb9f4a3/690b09b2-standard).
  Xcode 26.6 (17F113), iOS 27.0, arm64 Simulator; 26/26 routes,
  14/14 native input cases, 6/6 app-hosted storage cases, 6/6 theme combinations,
  and 11/11 theme/dialog checks passed. Foundation records confirm native
  readiness, `FIRST_FRAME_METAL`, and no-crash survival; this run used Metal,
  not the Simulator software fallback. Main downloaded the evidence commit
  and viewed all 26 `standard-*.png` and 23 `theme-*.png` captures, including
  picker, partial error, Herdr terminal/groups, recovery and theme states.
  No visible regression was identified in those captures.
- iOS `ssh` passed using the same session's pristine products from the fresh
  `standard` build. The strict restore helper verified manifest, checksum,
  source SHA and Xcode match before reuse. Evidence:
  [`evidence/ios-20260928-issue45-ssh`](https://github.com/phni3j9a/meeterm/tree/fbec803016f5f05cf4dc0cf0ad30e255659e454e/690b09b2-ssh).
  Real OpenSSH/tmux runtime selection, native input, session switching, theme
  markers and transport-loss recovery passed. Loss retained the terminal
  identifier/handle and selected pane, showed the cached read-only surface,
  and accepted no input while disconnected. Pre/post markers occurred once
  on the same pane/PID with other panes clean. Main viewed all 8 PNG captures,
  including native input, light/dark terminals, keyboards and disconnect.
  The renderer marker was again Metal.
- Android `full` passed on the exact mobile source above: fresh clean CNG,
  release build (2m22s), install/launch/native-ready/first-frame/no-crash gates,
  and 116 SSH/tmux stages through `disconnect_after_resume` (about 35 minutes).
  Evidence:
  [`evidence/android-20260928-issue45`](https://github.com/phni3j9a/meeterm/tree/da662f473eecf8ee4a8fc36fb937df66da8aa637).
  API 36 x86_64 emulator, JDK 17.0.19, NDK 27.1.12297006, Gradle 9.3.1,
  Rust 1.96.0. Transport loss retained the native handle and pane, accepted no
  input while stale, and produced pre/post markers exactly once with other
  panes clean. Settings, six theme combinations, OS theme changes, dialogs,
  saved credentials, native copy/paste, CJK atlas and desktop layout restoration
  completed. Main downloaded the evidence commit and viewed all 59 PNGs:
  31 seeded routes and 28 interaction captures. The observational `empty`
  route was unavailable with `missing_screen_element_0,screen_element_1`,
  recorded in `empty-unavailable.txt`; it is not counted as a viewed/passing
  screenshot. Screenshot presence is not a machine gate. The other viewed
  captures showed the expected picker, terminal, recovery and theme states.

Mobile SSH suites exercise tmux; seeded Herdr screens only verify presentation.
Physical-device GPU, fonts and Japanese IME parity remain outside this change's
emulator/Simulator acceptance scope. Prior Issue #17 evidence remains unchanged.

## POSIX resolver follow-up

Final review reproduced a production resolver issue when the remote login shell
is zsh: with no mise install directory, its default NOMATCH rejects the optional
mise glob before even an existing `~/.local/bin/herdr` candidate is enumerated.
The original command exited 1 with no candidates in an isolated local zsh test.
The resolver now explicitly runs its static script under `/bin/sh`; the same
test exits 0 and returns the local candidate without executing it. The existing
resolver filesystem test also compares POSIX sh with outer bash `failglob`, so
CI covers strict unmatched-glob behavior without requiring a new shell package.

This changes code compiled into the mobile library. The mobile results above
remain scoped to their recorded `690b09b2` source. Final acceptance therefore
uses fresh Android full and iOS standard/ssh runs on the follow-up candidate,
recorded below.

Follow-up candidate: `1351186c518cddd24a0cc4538c5704712d1c9868`.
Local fmt/Clippy, 238 unit cases, 4 fixture/parser cases and 1 layout case passed.
Real Herdr 0.9.0 and 0.9.1 each passed all four ignored cases (41.55 and 41.93
seconds while the two isolated version runs overlapped). The first local
invocation omitted `MEETERM_HERDR_INTEGRATION=1`, so three cases stopped at their
opt-in precondition; those invocations are not counted as successful execution.

[Follow-up Rust CI](https://github.com/phni3j9a/meeterm/actions/runs/36447040511/job/109011907881)
passed on that exact candidate: all 238 unit cases, 4 fixture/parser cases,
1 layout case, real OpenSSH/tmux (28.15 seconds), all four SHA-verified 0.9.0
cases (22.12 seconds), and the SHA-verified 0.9.1 compatibility/recovery case
(3.13 seconds). JavaScript/Expo, Swift preflight, and Android native build
(job `109011908110`, 13m43s) also passed. Both
[PR CI](https://github.com/phni3j9a/meeterm/actions/runs/36447040511) and
[push CI](https://github.com/phni3j9a/meeterm/actions/runs/36447033610) succeeded.

Final mobile acceptance uses that exact `1351186` candidate and the same SWE-2
Max platform sessions linked above:

- iOS `standard`: passed with fresh CNG and unsigned build-for-testing, with
  no reuse of `690b09b2` products. Evidence
  [`evidence/ios-20260928-issue45-posix-standard`](https://github.com/phni3j9a/meeterm/tree/330b19edca2a4b59d0d439f866dff8fd8bb23f5d/1351186-posix-standard)
  records the exact source and Xcode 26.6 (17F113), arm64, iOS 27.0 toolchain.
  All 26 routes, 14 native input cases, 6 app-hosted storage cases, 6 theme
  combinations and 11 theme/dialog checks passed. Native readiness, Metal
  first frame and no-crash survival passed. Main downloaded this exact
  evidence commit and viewed all 26 standard, 23 theme and one foundation PNGs;
  the picker, Herdr presentation, terminal, recovery, keyboard and theme states showed
  no visible regression in these captures.
- iOS `ssh`: passed using pristine products from the same session's fresh
  `1351186` standard build. The strict restore helper verified source SHA,
  manifest, checksums and Xcode before reuse. Evidence
  [`evidence/ios-20260928-issue45-posix-ssh`](https://github.com/phni3j9a/meeterm/tree/52a7d28c3b07d42051e6c88696108dde9f5799e1/1351186-posix-ssh)
  records real OpenSSH/tmux selection, native input, theme markers and
  transport-loss success. The native terminal identifier/handle and selected
  pane remained the same, the cached surface stayed read-only during loss,
  and pre/post markers occurred once on the same pane/PID with other panes
  clean. Metal executed. Main downloaded this exact evidence commit and
  viewed all eight PNGs, including input, keyboard, both terminal themes and
  disconnect. No visible regression was identified in these captures.
- Android `full`: passed with fresh CNG and a release build (2m17s). Evidence
  [`evidence/android-20260928-issue45-posix`](https://github.com/phni3j9a/meeterm/tree/3b4d472344e3f02004527974203d6efc106d78e2)
  records the exact `1351186` source, API 36 x86_64 emulator, JDK 17.0.19,
  NDK 27.1.12297006, Rust 1.96.0 and Gradle 9.3.1. Install, launch, native
  readiness, first frame and no-crash gates passed. The real SSH/tmux suite
  completed all 116 stages through `disconnect_after_resume` in about 33
  minutes. Transport loss retained the native handle and pane, blocked input
  on the stale surface, and produced pre/post markers exactly once with other
  panes clean. Settings, saved credentials, native copy/paste, CJK atlas,
  desktop layout restoration, six theme combinations, OS theme changes and
  dialogs completed. Main downloaded this exact evidence commit and viewed
  all 59 PNGs: 31 seeded routes and 28 interaction captures. No visible
  regression was identified in those captures. The observational `empty`
  route again records `missing_screen_element_0,screen_element_1` in its fresh
  unavailable diagnostic; it is not counted as a viewed/passing screenshot.
  The artifact directory was reused, so Main requested a provenance audit
  before publication. `artifact-provenance.txt` records the post-suite mtime
  sweep: suite files were overwritten by this run, and two manual summaries
  were regenerated from its fresh logs. No stale files remained or needed
  exclusion; no earlier-stage screenshot was back-filled.

Subsequent evidence-only commits do not change the accepted product, native,
test or build inputs. Their CI status is linked from PR #46. The mobile live
connection cases above exercise tmux; live Herdr compatibility and zero-tap
recovery are established by the separate Rust/russh binary cases. Emulator and
Simulator captures do not establish physical-device GPU or external-IME parity.
