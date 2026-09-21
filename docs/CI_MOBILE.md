# Mobile validation guide

This guide defines Android emulator and iOS Simulator validation on **Devin Cloud
persistent sessions**. It complements the Android device runbook in
[`POC_ANDROID.md`](POC_ANDROID.md); it does not turn a simulator/emulator into a
physical-device substitute.

The standard day-to-day sequence and exact commands are in
[TESTING.md](TESTING.md). The user-approved policy of 2026-09-11 uses iOS
`standard` for normal acceptance, with a separate short `ssh` suite when
connection/input changes. Android retains its full smoke; the long iOS `full`
is an optional diagnostic.

History: until 2026-09-20 the same suites ran on GitHub-hosted runners via
`.github/workflows/mobile-smoke.yml`. That workflow was retired after the Devin
Cloud path proved the same gates end to end; old workflow runs remain in git
and Actions history as historical evidence.

Shared and fast checks still run in [CI](../.github/workflows/ci.yml). Native
builds and runtime verification run on the sessions below.

## Validation sessions

| Session | Platform | Purpose |
| --- | --- | --- |
| [`7a32a4e6ed984961b5194e22feeba407`](https://app.devin.ai/sessions/7a32a4e6ed984961b5194e22feeba407) | Devin Cloud macOS (Apple Silicon) | iOS Simulator suites: `standard`, `ssh`, `polish`, `polish-navigation`, `native`, `forms`, `names`, optional `full` |
| [`9429c00e8cc14fb2b140b3e23bb28ec1`](https://app.devin.ai/sessions/9429c00e8cc14fb2b140b3e23bb28ec1) | Devin Cloud Linux (KVM) | Android build, emulator smoke, screen fixtures, real SSH/tmux smoke |

How a run works:

1. Sessions are created once in the Devin Web UI with the **SWE-2** model.
   The Sessions API cannot select SWE-2 (`devin_mode` accepts only
   `normal/fast/lite/ultra/fusion`); web-created SWE-2 sessions report
   `devin_mode: null`. Session creation is therefore a one-time manual step per
   platform; everything after that is API-driven.
   The repository's `.devin/blueprint.yaml` (snapshot builds
   `sbj-*`) preinstalls the toolchain on both `linux` and `macos` — Node 22.22.2,
   Rust 1.96.0 + platform targets, Android SDK/NDK, cargo-ndk, the Maven mirror
   init script, and brew tmux/cocoapods — so a fresh session starts nearly warm
   and can be recreated whenever context or VM drift accumulates. After creating
   a replacement session, update the ID in the table above.
2. The driver (Main) sends a validation prompt through the Sessions API
   (`POST /v3/organizations/{org}/sessions/{id}/messages`), then polls session
   status. Sessions sleep while idle and wake on the message; idle time does
   not consume quota.
3. The session executes the suite, pushes the observability bundle to an
   `evidence/<platform>-<yyyymmdd>` orphan branch, and reports a structured
   result. Main fetches the branch and actually views the images; the Director
   judges acceptance from that evidence.
4. On failure, the same session can investigate in place — live `adb`/`xcrun`,
   remote tmux polling, fixture logs — which is the main advantage over hosted
   runner logs.

Persistent VMs keep the installed toolchain, so runs after the first skip
setup. Every run still starts with `git fetch` and `git reset --hard <SHA>` on
the exact candidate commit and regenerates CNG output; persistent state is
cache, never source of truth. If a VM drifts, recreate the session from the
saved blueprint or rerun the setup section of the runbook.

## Source of truth and CNG

Tracked source remains:

- root TypeScript and Expo app config;
- `modules/meeterm-terminal/`, including each platform's native adapter;
- `native/meeterm-core/`, including shared Rust terminal semantics and bridge source;
- lockfiles and pinned toolchain/dependency declarations.

`android/` and `ios/` are Expo Continuous Native Generation (CNG) output and
remain untracked. Each acceptance run resets to the exact candidate commit,
runs `npm ci`, generates only the requested platform with `expo prebuild`, and
builds that generated project. Explicit same-commit product reuse inside the
same session can validate another suite against that build; it is not a new
CNG/build run. Record both the fresh build and the reuse run. Do not make a
generated Gradle/Xcode/Podfile edit the source of truth.

## Focused execution and product reuse

`MEETERM_IOS_SUITE` selects `standard|polish|polish-navigation|ssh|full|forms|native|names`.
The default is `standard`. `MEETERM_IOS_PROFILE=compact-xl` selects the explicit
accessibility diagnostic simulator profile (SE-class layout, extra-large
content size); report it separately from normal Pro-class results and never
substitute a large device for a small one.

- `standard`: four production storage cases, eleven native input/recovery-bridge
  cases plus one scroll-gesture case, direct screen captures from public deterministic state, and a fresh native foundation
  launch/readiness/frame/no-crash observation. Its source-level screen manifest
  has 22 routes: the previous 18 plus `recovery-progress`,
  `recovery-exhausted`, `recovery-mismatch`, and `herdr-recovery-confirm`. The existing
  `herdr-connection` route is the picker with the Herdr `default` candidate's
  non-authoritative `Last used` hint. The four recovery presentation routes
  are `recovery-progress`, `recovery-exhausted`, `recovery-mismatch`, and
  `herdr-recovery-confirm`; they retain the native terminal and verify the
  applicable recovery rail copy/action state. Runtime-picker and recovery
  states are seeded only for presentation; no SSH fixture is started.
- `ssh`: the actual connection and host-key boundary, runtime discovery and
  explicit selection, a healthy same-process app background/foreground return
  while the selected tmux pane remains active, and a resumed native input and
  remote acknowledgment. It also runs a separate deterministic transport-loss
  case: the disposable fixture stops/restarts only its sshd through its
  fixture-owned control files, while tmux remains alive. After the stop ACK,
  the Android driver reaches the fixture directly through the Android
  Emulator's reserved `10.0.2.2` alias for the host loopback interface. It
  requires exactly one ready `emulator-<port>` transport before credential
  entry, requires an empty serial-scoped reverse list before and after the
  loss case, and runs a bounded zero-I/O reachability probe to the exact alias
  and fixture port. It does not create or remove an ADB reverse mapping. The fixture's exact accepted sshd session
  therefore owns the connection-loss boundary without an intermediate relay.
  The app must then show the retained read-only rail and
  return Ready to the same pane/native handle without reopening the picker,
  followed by one post-loss remote acknowledgment and disconnect. No daily
  CRUD/copy/restart chain; this path does not claim arbitrary packet loss,
  network handover, physical device, or Herdr mobile recovery. The healthy
  foreground cycle and transport-loss case are separate evidence categories.
- `forms`, `native`, `names`, and the old `full`: explicitly requested diagnostics.
  Full preserves its original assertions and result; a prior failed full remains failed.

For Issue #26, Android `full` and iOS `standard` plus `ssh` are the applicable
mobile paths. Shared/native tests separately cover bounded no-side-effect
discovery, tmux list/create/select and identity races, Herdr executable
resolution and running-only selection, profile migration, reconnect identity,
switch/release, backend-local partial failures, and fail-closed linked/shared
tmux mutations. Retained-work checks additionally cover strict original tmux
pane recovery, Herdr in-work confirmation, operation-epoch input gating, no
automatic picker/fallback, and authoritative resynchronization before Ready.
The iOS `standard` source-level manifest has 22 routes and Android's
observational `SCREEN_NAMES` has 29 routes: each includes the four
runtime-picker routes `runtime-picker`, `runtime-partial-error`, `runtime-empty`,
and `runtime-create`, plus `recovery-progress`, `recovery-exhausted`,
`recovery-mismatch`, and `herdr-recovery-confirm`, while `herdr-connection` is
the repurposed picker state described above. These are source-level scopes only;
this document does not claim remote CI or visual review.

Standard and ssh each have a 15-minute XCTest budget. Native has 10 minutes,
forms/names have 15, and optional full retains its 30-minute storage/UI budget.
Session-side build and Simulator setup time is outside those budgets and is
recorded with the result.

For `ssh`, `full`, and `names`, `scripts/ci/ios-smoke.sh` itself runs the
preflight the retired workflow enforced: fixture-only tmux installation,
`scripts/ssh/fixture.py --check` (authenticated SSH and remote tmux resolution
with the disposable host key), and the
`real_openssh_existing_tmux_runtime_selection` cargo test through the fixture.
This preflight is environment validation and does not count as iOS terminal or
SSH UI evidence. Standard and focused forms/native runs skip fixture
installation and startup entirely.

Same-source reuse: within a session, `RUNNER_TEMP` derived-data persists, so a
second suite on the identical commit can reuse the fresh `build-for-testing`
products instead of rebuilding. The driver must verify the commit and toolchain
match before reusing and must never claim a new CNG/build for a reuse run. See
[TESTING.md](TESTING.md) for commands, triage, the limits of reuse and the final
acceptance procedure.

## Machine gates

Both platforms use the same acceptance boundary:

1. The fresh CNG-generated native project builds.
2. The app installs in the emulator or Simulator.
3. The app launches successfully.
4. The expected native terminal module reports readiness.
5. The native terminal reports a first rendered frame from a real Rust-owned snapshot.
6. The app remains alive without a crash during the smoke interaction.

Readiness and first-frame signals must come from the native module/view or a sanitized native log, not from OCR, a fixed sleep, or the existence of a screenshot. The Android module already has native event/readiness and metrics concepts; the iOS adapter should expose the same low-frequency contract without moving terminal bytes, cells, render frames, or IME composition through JavaScript.

The foundation gates exercise fixed local demo bytes through the shared Rust
terminal state and native snapshot/render path. A static React Native label,
WebView terminal, or screenshot fixture is not an acceptable substitute.

Android additionally runs the disposable OpenSSH/tmux fixture and
drives the connection form through `adb`. The script verifies the displayed
host fingerprint before explicitly trusting it, waits for the Rust-backed
connection state, and sends native terminal input that writes a marker inside
the fixture's temporary directory. It also disconnects and reconnects, checks
the same pane identity, and uses a shell variable to prove that the original
remote process resumed. This verifies the real SSH/tmux input path
without adding a production test endpoint or passing terminal streams through
JavaScript. While connected, the selected mobile pane may legitimately be
zoomed, so the resume check compares stable window/pane identities and shell
PIDs. After each explicit disconnect, including a second disconnect after the
resume check, the script requires the original split shape and no remaining
mobile zoom. The PC-help screenshot documents the UI; ordinary desktop attach
is covered separately by the shared Rust integration test and the iOS fixture.
The remote-shell screenshot is separate from the foundation
screenshot and is for human inspection; neither screenshot is a pixel gate.
The daily-use extension also checks saved-profile management, native credential
restoration after process restart, settings persistence, workspace/pane mutations,
CJK atlas rollover, and exact native selection/copy/paste. Android’s deliberate HOME
transition requires the configured launcher, the same app PID on return and a
fresh acknowledgment from the original remote shell. Recording starts after
credential entry and restoration; detected unexpected foreground loss discards
the recording rather than capturing another app.
The HOME transition just described is the healthy foreground evidence and does
not simulate a transport failure. The same Android `full` run separately sends
`stop` and `start` requests through the fixture's mode-0600
`sshd-control-request`/`sshd-control-status` files. After the stop ACK, the
driver has already required an emulator serial and replaced only the fixture's
published `127.0.0.1` form value with `10.0.2.2`, Android Emulator's reserved
alias for the host loopback interface. The expected host-key fingerprint still
comes from the same disposable fixture key. A read-only pre/post check requires
one ready selected emulator and an empty reverse list. No ADB reverse relay, adbd restart,
host ADB server restart, or product-side transport hook is involved. Only the fixture
sshd process tree is stopped; the tmux server/session/shell and endpoint identity
are left alive. The fixture's verified listener/session owner exit is therefore
the accepted-stream boundary directly observed by the app's TCP socket. The driver
records sanitized fixed results for the pre-loss and post-loss markers, the
cached read-only rail, the same native handle, the same selected pane, and the
absence of input while stopped. On iOS, the final
sanitized artifact carries the independent `native_handle_same` and
`selected_pane_identifier_same` booleans in addition to the native surface
binding boolean. This transport-loss
branch is a remote acceptance check; local source/Python tests and this document
do not claim that a remote run has passed.
The iOS `ssh` suite records the same two categories separately: its existing
Home/background → activate cycle is healthy foreground evidence, while the
fixture control request is the intentional SSH/Control Mode loss. XCTest sends
no input between the stop and start acknowledgements, checks the retained
read-only terminal and smoke-only opaque native handle, records the stale and
recovered handle comparison as `native_handle_same`, and records the independent
selected-pane checks as `selected_pane_identifier_same`. It then sends the unique
post-loss marker only after the same pane is authoritative Ready. The host
driver verifies the marker pair against the live tmux pane and rejects any
duplicate or other-pane occurrence. These checks remain remote-only until the
app is run on the session Simulator; no local source result is a mobile
acceptance claim.

The iOS run generates a temporary app-hosted storage test target and an XCUITest
target in the fresh CNG project. The storage target compiles only
`ClientStoreTests.swift` and resolves the production module through `TEST_HOST`;
it does not copy the storage implementation or link a second native runtime.
After CocoaPods integration, `scripts/ci/ios-strip-storage-rust-link.py` removes
the inherited Rust library flag from the storage target's generated
Debug/Release configurations and verifies that the app retains it. It also checks
that only the app generates an Expo provider. Standard, full and native suites
require four fresh storage-case success markers before their input/UI tests run;
a successful runner exit without those cases does not pass the gate. The
forms-only diagnostic does not run storage tests.

In the optional full scope, the XCUITest target drives the actual connection form, host trust, workspace/pane selection,
native input, disconnect, and reconnect against the same disposable fixture.
The Python driver then checks remote markers and ordinary desktop attach.
The daily iOS flow also checks saved-credential cold restart, native selection
and Copy, persisted settings, and workspace/pane management. After a real Copy
and selection clear, XCTest requests a bounded host-side `simctl pbpaste` check
through a fresh per-run marker. The host compares clipboard content in memory
and returns only a fixed result; clipboard text is never logged or uploaded.
Missing observations, content mismatch and command timeout fail the gate. The
UI runner must not read another app's `UIPasteboard.general.string`, because
iOS can block that synchronous read behind a paste permission alert. Production
copy/paste and its permission behavior remain unchanged. Storage plus input/UI
execution retain one shared 30-minute deadline.
After the real SSH flow, XCUITest terminates the app and opens the explicit
foundation URL with `XCUIApplication.open(_:)`. It requires the preview and
native terminal to appear, then continuously observes the app in the foreground
for ten seconds. The host validates that this fresh process reported native
readiness and a Metal or Simulator-only software first frame at least five
seconds before that observation ended. UTC log timestamps keep foundation
frames separate from the real SSH frame gate. The collector preserves the
XCTest screenshot rather than capturing an arbitrary post-test screen.
Raw XCTest output and xcresult bundles can contain typed credentials and remain
in session temporary storage; only sanitized stages and safe screenshots are pushed.
Standard also finishes with that fresh-foundation observation, independently of
the optional full sequence. Current suite evidence is tracked in `DAILY_USE.md`.
Both foundation previews require a smoke build and an explicit
`meeterm://foundation?foundation=1` launch. Ordinary startup opens the real app.
See [`SSH.md`](SSH.md) for credentials, commands, and the remaining limits.

## Direct screen fixtures

Screenshot coverage opens fixed screen states through a smoke-build-only explicit
launch route. It reuses production screen components and public metadata; it
must not type credentials, connect to a real host, or mutate saved profiles to
prepare an image. Ordinary startup and non-smoke builds do not activate this route.
The terminal image still uses the Rust fixture and native TerminalView.

A seeded server/workspace/settings image proves presentation, not successful
creation, persistence, connection or copy. Keep that boundary in reports. The
short SSH suite supplies actual connection/input evidence separately; more
extensive lifecycle, clipboard and editing flows retain their actual evidence
or are explicitly listed as unverified. New policy does not retroactively turn
old full failures into success.

## Evidence and visual review

Each run pushes an observability bundle to an `evidence/<platform>-<yyyymmdd>`
orphan branch in this repository — including on failed runs. Once app launch is
reached, the run attempts to capture a screenshot and sanitized native log; if
an earlier stage or capture itself fails, the bundle contains an explicit
`screenshot-unavailable.txt` or runtime-log diagnostic rather than a fake image.
Logs must not contain private keys, passwords, passphrases, host credentials, or
raw authentication material.

- `artifacts/android-emulator-observability/` — build log, launch/process/logcat
  records, screen-fixture captures, SSH validation results, failure-state
  screenshot and recording. Large binaries (`app-release.apk`) are excluded by
  default; request explicit upload when an evaluation APK is needed.
- `artifacts/ios-simulator-observability/` — suite results, stage/timing
  records, screen captures, sanitized simulator log, foundation observation.

Evidence branches accumulate; prune old ones once their runs are recorded in
the acceptance documents. Session-page attachments are a secondary, expiring
channel — the branch is the durable record.

There is no screenshot-existence or pixel-difference machine gate at this stage. Native readiness, renderer-specific first-frame evidence, and process survival are the runtime gates. For every native UI change, Main must fetch the evidence branch and actually view both the Android emulator screenshot and the iOS Simulator screenshot before reporting visual success. If either PNG is unavailable or invalid, visual success remains unverified even when the machine gates pass. Pixel comparisons may be reconsidered only after renderer/font/device variance is understood and the visual contract is explicitly defined.

## Environment notes (measured 2026-09-20)

- **Maven Central is blocked** by the organization network policy (HTTP 403).
  The Android session injects Google's official mirror
  `maven-central.storage-download.googleapis.com/maven2/` plus a plugin mirror
  via `~/.gradle/init.gradle` (`beforeSettings` hook — `settingsEvaluated` is
  too late for included builds). The permanent fix is adding
  `repo.maven.apache.org`, `repo1.maven.org`, and `plugins.gradle.org` to the
  Devin Security Profile allowlist (Settings → Customization → Security
  profiles); keep the mirror init script until that lands.
- **arm64 macOS session**: build with `ARCHS=arm64` — an `x86_64` simulator
  product cannot install on the arm64 host's simulator (Rosetta does not cover
  sim apps). The Rust slice target is `aarch64-apple-ios-sim`. This is a
  legitimate platform deviation from the retired Intel runner's `x86_64` path.
- **Simulator runtime mismatch**: when `xcodebuild` reports the SDK's expected
  iOS runtime build missing, bind the installed build with
  `xcrun simctl runtime match set <platform> <build>` (verify with
  `xcrun simctl runtime match list`).
- **`SIMCTL_CHILD_TZ=UTC`**: `simctl spawn log show --start` parses its
  timestamp in the simulator's local timezone regardless of `--timezone UTC`.
  Export it for the smoke run or the post-XCTest log query window is empty on
  non-UTC hosts.
- **Suite re-runs**: app profile state persists between runs on the same VM;
  `adb shell pm clear dev.meeterm.app` restores first-run conditions for
  Android. Stale `.xcresult`/`.xctestrun` products under `RUNNER_TEMP` similarly
  break iOS re-runs and must be cleared.
- **Headless by default**: the Android emulator runs `-no-window -gpu
  swiftshader_indirect`; screenshots come from `adb exec-out screencap`, so no
  visible window appears on the session desktop. The iOS Simulator.app shows a
  window — that difference is expected.

## iOS signing boundary

The Simulator run is an unsigned simulator build/install check. It must not require distribution certificates, provisioning profiles, an Apple Developer account, or signing secrets. Physical-device installation and TestFlight distribution are later signed workflows with protected credentials, provisioning decisions, and separate acceptance evidence.

For production Keychain tests, the disposable Simulator app host embeds XML and
DER entitlement sections in its Mach-O executable. The run verifies both
sections before execution. This is a Simulator-only test configuration and does
not provide device signing or distribution credentials. Storage tests run in
that app host before the separate UI runner starts.

The runtime uses Release configuration to embed the JavaScript bundle and avoid depending on Metro or the Expo development launcher. This is still a local smoke binary, not an App Store/distribution build. Interactive local work and physical-device input testing use Expo Development Builds.

## Native dependency updates

An update to Expo/React Native, `expo-build-properties` or `expo-dev-client`, Rust/`alacritty_terminal`, Android SDK/NDK/Gradle, Xcode/SDK/CocoaPods, bundled fonts, or the iOS renderer backend is a cross-platform native dependency update. Pin or document the relevant versions, regenerate CNG output, and run both mobile validations before merging it. Since generated iOS dependency output is not currently committed, the session toolchain policy must be explicit rather than relying on a local `Podfile.lock`.

The current iOS adapter uses direct Metal and the explicitly identified Simulator-only CoreGraphics fallback described below. Any future backend change must be supported by native evidence for snapshot throughput, text/CJK rendering, IME/lifecycle behavior, build cost and maintenance, and recorded in the architecture. Do not hide a backend change in generated project files or route rendering through JavaScript.

Metal execution: the Devin Cloud macOS session runs on Apple Silicon where the
Metal renderer path is available, and the 2026-09-20 `standard` run recorded the
Metal first-frame marker. The validation must still distinguish a Metal
first-frame marker from the Simulator-only native CoreGraphics fallback marker —
the fallback validates the Rust snapshot, CoreText, view, and input boundary,
but it is not evidence that Metal executed. iOS Metal parity on physical devices
remains a device-validation item.

## Minimal run shape

Each validation run on a session follows this order:

```text
git fetch && git reset --hard <candidate SHA>
npm ci
native dependency/toolchain check
expo prebuild --platform android|ios --non-interactive --no-install
build the generated native project
boot the emulator/Simulator
install and launch the self-contained smoke app
wait for native readiness and first-frame evidence
capture screenshot and sanitized log
push the observability bundle to the evidence branch, even on failure
```

The Android-specific toolchain values and physical-device commands remain in
[`POC_ANDROID.md`](POC_ANDROID.md). iOS simulator build glue belongs in the
local module/app source and the session procedure, not in an ignored generated
directory.
