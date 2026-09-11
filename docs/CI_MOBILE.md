# Mobile CI guide

This guide defines the intended GitHub-hosted Android emulator and iOS Simulator validation. It complements the Android device runbook in [`POC_ANDROID.md`](POC_ANDROID.md); it does not turn a simulator/emulator into a physical-device substitute.

The standard day-to-day sequence and exact commands are in [TESTING.md](TESTING.md).
The user-approved policy of 2026-09-11 uses iOS `standard` for normal acceptance,
with a separate short `ssh` suite when connection/input changes. Android retains
its full smoke; the long iOS `full` is an optional diagnostic.

The implementation lives in [CI](../.github/workflows/ci.yml) for shared and fast checks and [Mobile smoke](../.github/workflows/mobile-smoke.yml) for native builds and runtime verification.

## Source of truth and CNG

Tracked source remains:

- root TypeScript and Expo app config;
- `modules/meeterm-terminal/`, including each platform's native adapter;
- `native/meeterm-core/`, including shared Rust terminal semantics and bridge source;
- lockfiles and pinned toolchain/dependency declarations.

`android/` and `ios/` are Expo Continuous Native Generation (CNG) output and remain untracked. Each acceptance build starts from a fresh checkout, runs `npm ci`, generates only the requested platform with `expo prebuild`, and builds that generated project. The iOS runtime job consumes pristine test products from its build job. Explicit same-commit product reuse can validate another suite against that fresh build; it is not a new CNG/build run. Record both source and runtime runs. Do not make a generated Gradle/Xcode/Podfile edit the source of truth.

## Staged jobs

| Job | Runner | Machine purpose | Evidence |
| --- | --- | --- | --- |
| Shared checks | `ubuntu-24.04` | Typecheck, Expo config/doctor, Rust format/test/clippy | Test output and logs |
| Android emulator | Pinned Ubuntu image | CNG, native build, emulator install/launch, native readiness, first frame, no crash | Screenshot and sanitized log, always uploaded |
| iOS driver preflight | Pinned macOS/Xcode image | UI/input Swift typecheck without CNG, app build, or Simulator | Compiler output; does not cover the production storage module |
| iOS build | Pinned macOS/Xcode image | Fresh CNG and unsigned app/test build | Build diagnostics always uploaded; pristine Products tar on success |
| iOS Simulator | Same pinned macOS/Xcode image | Restore matching products, install/launch, selected scope | Screen captures and sanitized log, always uploaded; standard retains native runtime gates |
| Physical devices | Separate later infrastructure | Android GPU/IME/font and iOS device/IME/font validation | Device-specific evidence |

Bring the Android emulator and iOS Simulator jobs online early, in parallel with the thin native adapters. A temporary toolchain/CNG bootstrap check may run before the iOS adapter exists, but it must not be described as iOS terminal verification or produce a fake terminal screenshot.

GitHub-hosted runners provide the required OS split: Android jobs can run on Ubuntu, while iOS Simulator jobs require macOS with Xcode and simulator runtimes. Pin the macOS runner/Xcode generation used by the project instead of relying on `macos-latest` for reproducibility. The current workflow uses `macos-26-intel` with Xcode 26.6 so the simulator Rust slice is `x86_64-apple-ios`. See [GitHub-hosted runners](https://docs.github.com/en/actions/concepts/runners/github-hosted-runners) and the [macOS runner image inventory](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-Readme.md).

## Focused execution and product reuse

`Mobile smoke` accepts `platform=both|android|ios` and
`ios_suite=standard|ssh|full|forms|native|names`. The default is `standard`.

- `standard`: four production storage cases, seven native input cases, direct
  screen captures from public deterministic state, and a fresh native foundation
  launch/readiness/frame/no-crash observation. No SSH fixture is started.
- `ssh`: the actual connection and host-key boundary plus one native input and
  remote acknowledgment, followed by disconnect. No daily CRUD/copy/restart chain.
- `forms`, `native`, `names`, and the old `full`: explicitly requested diagnostics.
  Full preserves its original assertions and result; a prior failed full remains failed.

Standard and ssh each have a 15-minute XCTest budget. Native has 10 minutes,
forms/names have 15, and optional full retains its 30-minute storage/UI budget.
The build and Simulator runtime jobs have separate budgets.

The build uploads `ios-test-products` before fixture environment injection.
Executables and symlinks are preserved in a tar archive, with a manifest binding
its checksum to the commit, Xcode version/build, architecture and Simulator
configuration. The runtime checks both GitHub run provenance and the manifest.
An explicit `ios_build_run` may reuse an identical-source build for another suite or diagnosis;
source changes require a new build. Runtime injection modifies a disposable
xctestrun copy, never the pristine products. See [TESTING.md](TESTING.md) for
commands, triage, the limits of reuse and the final acceptance procedure.

## Machine gates

Both runtime jobs use the same acceptance boundary:

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
Before ssh, full or names iOS runtime testing, the runtime job installs fixture-only tmux if needed and runs
`python3 scripts/ssh/fixture.py --check`. This verifies authenticated SSH and
remote `tmux` resolution using the disposable host key. The fixture supplies
its tmux binary directory through its own sshd environment; this preflight is
environment validation and does not count as iOS terminal or SSH UI evidence.
Standard and focused forms/native runs skip fixture installation and startup. The build job
does not boot a Simulator or start an SSH fixture.

The iOS build job generates a temporary app-hosted storage test target and an XCUITest
target in the fresh CNG project. The storage target compiles only
`ClientStoreTests.swift` and resolves the production module through `TEST_HOST`;
it does not copy the storage implementation or link a second native runtime.
After CocoaPods integration, the job removes the inherited Rust library flag
from the storage target's generated Debug/Release configurations and verifies
that the app retains it. It also checks that only the app generates an Expo
provider. Standard, full and native suites require four fresh storage-case success markers
before their input/UI tests run; a successful runner exit without those cases
does not pass the gate. The forms-only diagnostic does not run storage tests.

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
in runner temporary storage; only sanitized stages and safe screenshots are uploaded.
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

## Artifacts and visual review

Each job always uploads an observability bundle, including on failed runs. Once app launch is reached, the job attempts to capture a screenshot and sanitized native log; if an earlier stage or capture itself fails, the bundle contains an explicit `screenshot-unavailable.txt` or runtime-log diagnostic rather than a fake image. Logs must not contain private keys, passwords, passphrases, host credentials, or raw authentication material.

There is no screenshot-existence or pixel-difference machine gate at this stage. Native readiness, renderer-specific first-frame evidence, and process survival are the runtime gates. For every native UI change, Codex must download and actually view both the Android emulator screenshot and the iOS Simulator screenshot before reporting visual success. If either PNG is unavailable or invalid, visual success remains unverified even when the machine gates pass. Pixel comparisons may be reconsidered only after renderer/font/device variance is understood and the visual contract is explicitly defined.

## iOS signing boundary

The Simulator job is an unsigned simulator build/install check. It must not require distribution certificates, provisioning profiles, an Apple Developer account, or signing secrets. Physical-device installation and TestFlight distribution are later signed workflows with protected credentials, provisioning decisions, and separate acceptance evidence.

For production Keychain tests, the disposable Simulator app host embeds XML and
DER entitlement sections in its Mach-O executable. The workflow verifies both
sections before execution. This is a Simulator-only test configuration and does
not provide device signing or distribution credentials. Storage tests run in
that app host before the separate UI runner starts.

The hosted runtime jobs use Release configuration to embed the JavaScript bundle and avoid depending on Metro or the Expo development launcher. This is still a local smoke binary, not an App Store/distribution build. Interactive local work and physical-device input testing use Expo Development Builds.

Standard GitHub-hosted macOS runners do not guarantee Metal/GPU passthrough. The iOS job therefore records a Metal preflight and accepts one of two distinct native first-frame markers: Metal when a device is available, or an explicitly named Simulator-only CoreGraphics fallback when it is not. Both paths consume the same Rust snapshot and remain entirely native, but the software fallback is not evidence that the Metal renderer ran. iOS Metal execution remains a physical-device or GPU-capable-runner validation item.

## Native dependency updates

An update to Expo/React Native, `expo-build-properties` or `expo-dev-client`, Rust/`alacritty_terminal`, Android SDK/NDK/Gradle, Xcode/SDK/CocoaPods, bundled fonts, or the iOS renderer backend is a cross-platform native dependency update. Pin or document the relevant versions, regenerate CNG output, and run both mobile jobs before merging it. Since generated iOS dependency output is not currently committed, the runner/Xcode/CocoaPods policy must be explicit rather than relying on a local `Podfile.lock`.

The current iOS adapter uses direct Metal and the explicitly identified Simulator-only CoreGraphics fallback described above. Any future backend change must be supported by native evidence for snapshot throughput, text/CJK rendering, IME/lifecycle behavior, build cost and maintenance, and recorded in the architecture. Do not hide a backend change in generated project files or route rendering through JavaScript.

## Minimal job shape

The exact action versions may evolve with the pinned runner images, but each job should follow this order:

```text
npm ci
native dependency/toolchain setup
expo prebuild --platform android|ios --non-interactive --no-install
build the generated native project
boot the emulator/Simulator
install and launch the self-contained smoke app
wait for native readiness and first-frame evidence
capture screenshot and sanitized log
upload both artifacts, even on failure
```

The Android-specific toolchain values and physical-device commands remain in [`POC_ANDROID.md`](POC_ANDROID.md). iOS simulator build glue belongs in the local module/app source and the macOS job, not in an ignored generated directory.
