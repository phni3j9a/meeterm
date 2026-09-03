# Mobile CI guide

This guide defines the intended GitHub-hosted Android emulator and iOS Simulator validation. It complements the Android device runbook in [`POC_ANDROID.md`](POC_ANDROID.md); it does not turn a simulator/emulator into a physical-device substitute.

The repository workflow is the integration point for the shared checks and both mobile runtime jobs. The target shape is described here so that the native implementation and CI environment are designed together: [`../.github/workflows/ci.yml`](../.github/workflows/ci.yml).

## Source of truth and CNG

Tracked source remains:

- root TypeScript and Expo app config;
- `modules/meeterm-terminal/`, including each platform's native adapter;
- `native/meeterm-core/`, including shared Rust terminal semantics and bridge source;
- lockfiles and pinned toolchain/dependency declarations.

`android/` and `ios/` are Expo Continuous Native Generation (CNG) output and remain untracked. Every mobile job starts from a fresh checkout, runs `npm ci`, generates only the requested platform with `expo prebuild`, and builds that generated project. Do not make a generated Gradle/Xcode/Podfile edit the source of truth.

## Staged jobs

| Job | Runner | Machine purpose | Evidence |
| --- | --- | --- | --- |
| Shared checks | `ubuntu-24.04` | Typecheck, Expo config/doctor, Rust format/test/clippy | Test output and logs |
| Android emulator | Pinned Ubuntu image | CNG, native build, emulator install/launch, native readiness, first frame, no crash | Screenshot and sanitized log, always uploaded |
| iOS Simulator | Pinned macOS/Xcode image | CNG, unsigned simulator build, simulator install/launch, native readiness, first frame, no crash | Screenshot and sanitized log, always uploaded |
| Physical devices | Separate later infrastructure | Android GPU/IME/font and iOS device/IME/font validation | Device-specific evidence |

Bring the Android emulator and iOS Simulator jobs online early, in parallel with the thin native adapters. A temporary toolchain/CNG bootstrap check may run before the iOS adapter exists, but it must not be described as iOS terminal verification or produce a fake terminal screenshot.

GitHub-hosted runners provide the required OS split: Android jobs can run on Ubuntu, while iOS Simulator jobs require macOS with Xcode and simulator runtimes. Pin the macOS runner/Xcode generation used by the project instead of relying on `macos-latest` for reproducibility. The current workflow uses `macos-26-intel` with Xcode 26.6 so the simulator Rust slice is `x86_64-apple-ios`. See [GitHub-hosted runners](https://docs.github.com/en/actions/concepts/runners/github-hosted-runners) and the [macOS runner image inventory](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-Readme.md).

## Machine gates

Both runtime jobs use the same acceptance boundary:

1. The fresh CNG-generated native project builds.
2. The app installs in the emulator or Simulator.
3. The app launches successfully.
4. The expected native terminal module reports readiness.
5. The native terminal reports a first rendered frame from a real Rust-owned snapshot.
6. The app remains alive without a crash during the smoke interaction.

Readiness and first-frame signals must come from the native module/view or a sanitized native log, not from OCR, a fixed sleep, or the existence of a screenshot. The Android module already has native event/readiness and metrics concepts; the iOS adapter should expose the same low-frequency contract without moving terminal bytes, cells, render frames, or IME composition through JavaScript.

The jobs may exercise fixed local demo bytes while SSH/tmux are not implemented. They must still use the shared Rust terminal state and the native snapshot/render path. A static React Native label, WebView terminal, or screenshot fixture is not an acceptable substitute.

## Artifacts and visual review

Each job always uploads an observability bundle, including on failed runs. Once app launch is reached, the job attempts to capture a screenshot and sanitized native log; if an earlier stage or capture itself fails, the bundle contains an explicit `screenshot-unavailable.txt` or runtime-log diagnostic rather than a fake image. Logs must not contain private keys, passwords, passphrases, host credentials, or raw authentication material.

There is no screenshot-existence or pixel-difference machine gate at this stage. Native readiness, renderer-specific first-frame evidence, and process survival are the runtime gates. For every native UI change, Codex must download and actually view both the Android emulator screenshot and the iOS Simulator screenshot before reporting visual success. If either PNG is unavailable or invalid, visual success remains unverified even when the machine gates pass. Pixel comparisons may be reconsidered only after renderer/font/device variance is understood and the visual contract is explicitly defined.

## iOS signing boundary

The Simulator job is an unsigned simulator build/install check. It must not require distribution certificates, provisioning profiles, an Apple Developer account, or signing secrets. Physical-device installation and TestFlight distribution are later signed workflows with protected credentials, provisioning decisions, and separate acceptance evidence.

The hosted runtime jobs use Release configuration to embed the JavaScript bundle and avoid depending on Metro or the Expo development launcher. This is still a local smoke binary, not an App Store/distribution build. Interactive local work and physical-device input testing use Expo Development Builds.

Standard GitHub-hosted macOS runners do not guarantee Metal/GPU passthrough. The iOS job therefore records a Metal preflight and accepts one of two distinct native first-frame markers: Metal when a device is available, or an explicitly named Simulator-only CoreGraphics fallback when it is not. Both paths consume the same Rust snapshot and remain entirely native, but the software fallback is not evidence that the Metal renderer ran. iOS Metal execution remains a physical-device or GPU-capable-runner validation item.

## Native dependency updates

An update to Expo/React Native, `expo-build-properties` or `expo-dev-client`, Rust/`alacritty_terminal`, Android SDK/NDK/Gradle, Xcode/SDK/CocoaPods, bundled fonts, or the iOS renderer backend is a cross-platform native dependency update. Pin or document the relevant versions, regenerate CNG output, and run both mobile jobs before merging it. Since generated iOS dependency output is not currently committed, the runner/Xcode/CocoaPods policy must be explicit rather than relying on a local `Podfile.lock`.

The iOS renderer choice remains an implementation tradeoff. The architecture records ANGLE/OpenGL ES compatibility to Metal as a candidate direction, while direct Metal remains possible. Select the backend from a native prototype's evidence about snapshot throughput, text/CJK rendering, IME/lifecycle behavior, build cost, and maintenance; do not hide the decision in generated project files or bypass it with JavaScript rendering.

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
