# Issue #45: Herdr compatibility contract

The production contract is protocol 22, API schema 1, the JSON methods used by
meeterm, and the list/status/direct-control CLI. SemVer and absolute executable
path equality are not connection gates. Host authentication, selected runtime,
stable terminal identity, ordinary controller acquisition without takeover, and
the first authoritative full frame remain required for retained recovery.

The bundled schema advertises JSON methods. Read-only CLI help probes cover the
list/status/control entry points and controller size options. Help argument
display names such as `<TARGET>` are not compatibility requirements. Actual
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

## PR #46 review follow-up (2026-09-29)

[Review 5346704282](https://github.com/phni3j9a/meeterm/pull/46#pullrequestreview-5346704282)
identified two gaps: candidate-local probe errors escaped the resolver instead
of trying the next path, and control help required the cosmetic `<TARGET>`
argument label. The Herdr-only probe now returns a rejected candidate for a
local timeout, stdout/stderr overflow, exec refusal or abnormal exit. Cancellation,
stale operation epochs and dead SSH transport still terminate resolution.
Schema and help share a five-second candidate budget inside the unchanged
30-second total probe budget. Opened probe channels are closed on all completion
paths, with bounded best-effort cleanup. The common tmux command helper is unchanged.

The existing live compatibility case now injects ten failed candidates (the
five failure modes above for both schema and help), followed by the existing
protocol/schema/missing-method negatives and a real compatible binary. It
observes client channel closure, accepts a `<TERMINAL>` help label, and retains
the same-terminal recovery/input checks. Additional real-SSH legs end a stalled
probe through explicit disconnect and transport loss; both must finish before
the candidate timeout and must not probe the next executable. Unit help checks
also accept `<TARGET>`, `<PANE>` and `<TERMINAL>` while retaining command/option
requirements.

During local test development, the initial close assertion expected a callback
after the fixture itself had already closed its abnormal-exit channel. That
case now sends the nonzero exit status and leaves the channel open so the test
observes the client's cleanup. A new abort leg initially waited for a host-key
prompt despite the existing helper already writing the isolated known-hosts
file; it now synchronizes on the actual probe request. Neither failed local
invocation counts as acceptance. The corrected focused case passed in 25.00s;
the full local Rust suite and Clippy also passed.

The previously recorded `1351186` mobile results remain evidence for that
source. Android full and iOS standard/ssh for this review follow-up use a fresh
build of the review candidate; exact-source results are linked from PR #46 and
recorded below. The earlier `cad6e16` push run's pre-test OpenSSH fixture failure
([job 109030574597](https://github.com/phni3j9a/meeterm/actions/runs/36452528703/job/109030574597))
remains an unresolved historical fixture diagnostic; the same-head PR run
passed. This review follow-up does not change that fixture or claim its root
cause is fixed.

Review product candidate: `5166dee92d22610e769cc2726a77c70641e63b27`.
Local full Herdr runs passed all four cases with each SHA-verified fixture:
0.9.0 in 63.42s and 0.9.1 in 63.17s (the isolated version runs overlapped).
[Review-candidate Rust CI](https://github.com/phni3j9a/meeterm/actions/runs/36512708507/job/109228171219)
passed: 238 unit cases, 4 parser cases, 1 layout case, real OpenSSH/tmux
(26.52s), all four Herdr 0.9.0 cases (24.50s), and the focused Herdr 0.9.1
compatibility/recovery case (24.51s). The local full runs serialize cases;
CI's 0.9.0 invocation uses the normal parallel test runner.
All four jobs passed in both the
[PR workflow](https://github.com/phni3j9a/meeterm/actions/runs/36512708507) and
[push workflow](https://github.com/phni3j9a/meeterm/actions/runs/36512704009),
including JavaScript/Expo, Swift preflight and Android native build.

Android `full` passed on the exact `5166dee` candidate with fresh CNG and a
release build (2m39s). Evidence
[`evidence/android-20260929-issue45-review`](https://github.com/phni3j9a/meeterm/tree/419462da9dfc8b629b248ff60542ecb53ed671d1)
records all machine gates, all 116 real SSH/tmux stages, and transport-loss
recovery preserving the native handle and pane with no input during loss and
exactly-once pre/post markers. Theme, credentials, native selection, CJK atlas,
and desktop layout cases completed. Main downloaded `8a44d19` and viewed all
59 PNGs (31 seeded routes and 28 interaction captures); no visible regression
was identified in those captures. The `empty` route again has a fresh
unavailable diagnostic and is not counted as a passing screenshot. The bundle
was created in an empty per-run directory. A subsequent documentation-only
evidence commit corrects its inventory to 78 files (59 PNGs and 19 others);
Main verified that images and logs did not change. An ACP control connection
closed during the suite, but the same remote test process continued without
restart.

The first iOS `standard` attempt on `5166dee` failed. Evidence
[`evidence/ios-20260929-issue45-review-standard`](https://github.com/phni3j9a/meeterm/tree/c9bb30fd326dcd3a969b826f1cb88d415ce07f4a)
records `xcuitest_standard` / `xcodebuild_timeout`: 834,990ms elapsed against
the remaining 833,233ms budget, with `theme_keyboard_open` as the last stage.
Storage 6/6, appearance, all 26 routes, the six theme combinations and OS/pinned
theme checks completed. The keyboard completion, 11 dialog checks, native input
result and final foundation gate were not reached. Main downloaded and viewed
all 37 available PNGs; these are partial diagnostic presentation evidence and
do not establish standard acceptance. Compared with the prior `1351186` run,
route capture took 526,319ms instead of 198,573ms and keyboard entry began at
751,727ms instead of 264,800ms; the slowdown spans the flow rather than only
the final keyboard stage. The deadline and assertions were unchanged.

Before that suite, preflight attempt 1 reported one failure but its detailed
output was discarded by the validation session's pipeline. Its cause and even
the failing test name remain undetermined; a passing second attempt does not
diagnose it. The first build separately failed with exit 65 and `No space left
on device`; after clearing completed prior-run build products, build attempt 2
succeeded on the same source. Disk exhaustion is only a possible explanation
for the earlier preflight failure, not an established cause. The standard
timeout and the incomplete failure-log preservation remain explicitly recorded.
The [evidence addendum](https://github.com/phni3j9a/meeterm/tree/09e9b976efc79a3aee05cc4cb43c13e6d099061b)
adds the retained build failure tail and passing standalone selection-text
preflight rerun output; it cannot recover the discarded first preflight detail.
During the subsequent SSH suite, the VM reported load averages around 78 on
12 cores, two booted simulators and active diagnostic/indexing processes, while
memory and disk checks did not show exhaustion. This is consistent with a
contention hypothesis, not proof of per-process causality during the earlier
standard run. In particular, the later SSH fixture's Rust compilation does not
establish overlap with the preceding standard UI test.

The independent first iOS `ssh` attempt also failed. Evidence
[`evidence/ios-20260929-issue45-review-ssh`](https://github.com/phni3j9a/meeterm/tree/5a11d95b569d516d097cf30aa34d4f28f3ba5bab)
records successful exact-source/toolchain/checksum product restoration and
fixture preflight, followed by `ui_test_failed` at the existing short-field
readback assertion (Swift line 3304). The username retained 1 of 5 characters
on both of the test's existing attempts. The test ran 133.7s (xcodebuild 220s),
so this was not a suite timeout. It stopped before connection, theme or
transport-loss assertions. Main viewed the sole `ssh-entry-initial.png`; it
shows a blank launch surface and establishes no successful SSH interaction.
High load is a possible contributor to dropped input, not a proven root cause.

The [corrected contention record](https://github.com/phni3j9a/meeterm/tree/5ea4405087316af29fa54a4c374ea7e00fa9da76)
keeps observation times separate from causal hypotheses. The unused iPhone Air
was shut down, followed by a normal shutdown/boot of the validation target
after the failed suites had ended. No host daemons or unrelated jobs were
terminated. The first instantaneous measurement still showed 71% CPU idle even
with a decaying load average around 22. After the target restart and settling,
the second sample showed 93% idle and a one-minute load of 10.2 on 12 cores.
Only after that measured recovery was one controlled rerun per suite authorized,
using unchanged source, exact same-session pristine products, assertions and
deadlines, with separate fresh output/evidence branches.

iOS `standard` then passed in the controlled rerun. Evidence
[`evidence/ios-20260929-issue45-review-standard-r2`](https://github.com/phni3j9a/meeterm/tree/226b5bf2610fbf4d0154af672e95abbd29b2a611)
records strict restoration of the pristine products from the same session's
fresh `5166dee` build, with matching manifest, checksums, source and Xcode
26.6 (17F113). All 26 routes, 14 native input cases, 6 app-hosted storage cases,
6 theme combinations and 11 theme/dialog checks passed. The fresh foundation
verified native readiness, a Metal first frame and no-crash survival; it did
not use the Simulator software renderer. UI teardown completed at 309,121ms.
The xcodebuild wrapper observed the explicit passed result and then used its
existing bounded post-result exit handling (60,189ms), as recorded separately
from the 380,500ms total invocation. Main downloaded this exact evidence and
viewed all 50 PNGs: 26 standard, 23 theme and one foundation image. No visible
regression was identified in these captures. Main first viewed `64c96d3`, then
verified that `226b5bf` changes only the run-record's observation-time wording;
images, logs and test results are unchanged. This passing rerun does not erase
or relabel the earlier failed attempt or establish the lost preflight cause.

iOS `ssh` also passed in its separate controlled rerun. Evidence
[`evidence/ios-20260929-issue45-review-ssh-r2`](https://github.com/phni3j9a/meeterm/tree/5d258729a6b829bbbca02bf0f8759a7fe7304dd7)
records the same strict source/toolchain/product restoration, with measured
load 2.4–4 during the run. Real OpenSSH/tmux input, server/session switching,
theme markers and transport-loss checks completed. Light/dark and pre/post-loss
markers occurred exactly once on the same pane/PID, with other panes clean.
Recovery retained the native terminal identifier/handle and selected pane,
kept the cached surface read-only and admitted no input during loss. Metal
executed. Main downloaded the final evidence and viewed all eight PNGs;
no visible regression was identified in these captures. The preceding failed
input attempt remains a separate result, not a passing SSH run.

Final review acceptance is therefore scoped to `5166dee`: both source CI runs,
Android full, and the controlled iOS standard/ssh reruns above. Subsequent
evidence-only commits do not change product, native, test or build inputs.
Mobile real-connection cases exercise tmux; live Herdr compatibility and
same-terminal recovery are established by the separate Rust/russh binary tests.
Emulator/Simulator results do not establish physical-device GPU or external-IME
parity. The initial preflight failure's root cause remains unknown because its
detailed output was not retained.
