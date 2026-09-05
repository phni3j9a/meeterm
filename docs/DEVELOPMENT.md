# Development

## Current phase

meeterm is in the architecture-validation phase. The Android/iOS native terminal foundation and hosted smoke jobs are in place. Issue #3 extends the shared Rust core with a direct SSH PTY shell, explicit host-key trust, and public-key authentication. The [SSH runbook](SSH.md) records this slice's validation and limitations. The first-milestone contract below is retained as the foundation and regression boundary; its deliberate omissions describe that earlier milestone.

Read, in order:

1. `PRODUCT.md`
2. `ARCHITECTURE.md`
3. repository root `AGENTS.md`

## First milestone: dual-platform native terminal foundation

The milestone is complete when the shared Rust terminal semantics are covered, the Android and iOS Expo native projects both contain terminal views whose state is owned by Rust, and both GitHub-hosted self-contained smoke apps can build, install, launch, report native readiness and a first native frame, and detect a crash. Local interactive development continues to use Expo Development Builds; hosted runtime smoke uses Release configuration so it does not depend on Metro or the development launcher. The simulator/emulator gates are early machine validation; they do not replace later physical-device validation.

The slice should prove:

- React Native can mount one native terminal-view contract through thin Android and iOS adapters;
- Rust owns an `alacritty_terminal::Term`;
- fixed byte sequences, including ANSI/VT behavior, update that Term;
- a native GPU renderer on each platform displays a real Rust-owned snapshot;
- each renderer has an explicit path for representative Latin and Japanese/CJK text;
- each platform's native keyboard/text-input path keeps IME composition out of JavaScript and commits text exactly once;
- viewport resize is deterministic on both platform paths where the check is available;
- the mobile CI jobs emit a deterministic native-readiness and first-frame signal rather than inferring success from a screenshot.

## Deliberate omissions from the first milestone

Do not add these merely to make the project look more complete:

- SSH;
- tmux;
- server profiles;
- reconnect logic;
- workspace screens;
- production navigation;
- broad settings;
- file browsing;
- backend services.

They come after the terminal foundation is proven.

## Implementation strategy

Use a proven native-terminal implementation as a reference where useful, especially for mobile build glue and Alacritty-derived renderer integration. Preserve applicable license notices when code is reused or adapted. Keep the Rust terminal semantics shared and keep platform-specific view, renderer, font, input, and build glue in thin adapters.

Prefer a thin vertical implementation over a generalized terminal framework. It is acceptable for the first slice to feed fixed local terminal bytes into the Rust terminal state.

Do not create speculative APIs for SSH/tmux before those layers exist.

## Validation

At minimum, validation should include:

### Rendering

- ASCII text;
- ANSI foreground/background/style changes;
- cursor positioning;
- wrapping;
- enough scrolling to exercise scrollback;
- Japanese wide characters such as `日本語`;
- mixed ASCII/CJK text;
- combining characters;
- representative terminal emoji if supported by the chosen font strategy.

### Input

- ASCII typing;
- Enter, Backspace, Tab, Escape;
- arrow keys;
- Japanese IME composition such as converting `きょう` to `今日` before commit;
- committed Japanese text enters the native terminal input path exactly once;
- composition is not prematurely transmitted through JavaScript.

### Resize

- portrait resize;
- landscape resize;
- software keyboard appearance/disappearance;
- foldable/window-size changes where the available test device permits them.

## CI gates and visual evidence

The Android emulator and iOS Simulator jobs should be established early and run the same machine-gated contract:

- the fresh CNG-generated native project builds;
- the app installs and launches;
- the expected native module reports ready;
- the native terminal reports its first rendered frame;
- the process remains alive without a crash.

An observability bundle must be uploaded on every run, including failed runs. After launch it should contain the screenshot and sanitized native log; an earlier failure or failed capture must instead leave an explicit unavailable diagnostic. There is no screenshot-existence or pixel-difference gate at this stage. For any native UI change, Codex must download and actually view both platform screenshots before reporting visual success; an artifact URL, a screenshot file existing, or a passing launch check is not visual review.

The machine gates do not claim physical-device GPU, font fallback, rotation, or IME parity. The Android physical-device record in [`POC_ANDROID.md`](POC_ANDROID.md) remains separate evidence, and the iOS physical-device/TestFlight path is later work.

The standard hosted macOS runner may not expose Metal. The iOS smoke log and metadata distinguish a Metal frame from the Simulator-only native CoreGraphics fallback. A fallback frame still exercises the real Rust snapshot and native CoreText/view path, but must not be reported as iOS Metal validation.

## Definition of done

The milestone is done when:

- the Android project and iOS Simulator project build reproducibly from documented commands;
- both self-contained Release smoke apps launch in their respective GitHub-hosted emulator/simulator jobs;
- both native terminal views render from the shared Rust-owned terminal state;
- the input tests above work without a WebView terminal or JS terminal byte stream;
- known limitations are documented explicitly;
- automated tests cover byte/terminal logic that can be tested below the platform renderer;
- CI checks formatting/static/unit-level concerns and the mobile machine gates without pretending to replace physical-device validation;
- each CI run has an observability bundle, and native UI changes are reported visually successful only after both actual screenshots have been downloaded and viewed.

## iOS signing, CNG, and native dependency boundary

The iOS Simulator job is an unsigned simulator build/install check. It must not depend on distribution certificates, provisioning profiles, or Apple signing secrets. Physical iOS device installation and TestFlight distribution require a separate signed workflow, protected credentials, and device-specific acceptance evidence.

`android/` and `ios/` are Expo Continuous Native Generation (CNG) output and remain untracked. Change the root app config, local module config/plugins, platform source, or Rust source; regenerate the native projects from a fresh checkout in CI rather than editing generated files. See [`POC_ANDROID.md`](POC_ANDROID.md) and [`CI_MOBILE.md`](CI_MOBILE.md).

An update to Expo/React Native, the Rust terminal stack, Android SDK/NDK/Gradle, Xcode/SDK/CocoaPods, bundled fonts, or the selected iOS renderer backend is a cross-platform native dependency change. Pin or document the relevant versions, regenerate CNG output, and run both mobile jobs before treating the update as complete. Keep direct Metal versus ANGLE/OpenGL ES as an evidence-driven iOS renderer decision; do not hide that tradeoff in generated output.

Do not mark the milestone complete solely because a simulator/emulator renders a Latin-only terminal, or because a screenshot was uploaded without being viewed.
