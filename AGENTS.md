# AGENTS.md

This repository is a greenfield implementation of **meeterm**, a smartphone-first SSH + tmux client.

Read `docs/PRODUCT.md` and `docs/ARCHITECTURE.md` before making architectural or product changes. Treat them as the current source of truth unless the task explicitly changes the product direction.

## Product invariants

- Remote tmux is the durable workspace source of truth.
- The managed tmux session is named `meeterm`.
- **Workspace = tmux window.**
- **Terminal = tmux pane.**
- On mobile, panes are presented as tabs and the selected pane receives a phone-appropriate full-size experience.
- On desktop, the same environment must remain usable through ordinary `tmux attach -t meeterm`.
- Smooth phone-to-PC handoff is required. Simultaneous interactive phone+PC use is not an initial requirement.

Do not change the session/window/pane mapping merely because another mapping simplifies mobile implementation.

## Architecture invariants

The intended data path is:

```text
React Native / Expo
        │ commands / state snapshots only
        ▼
Rust native core
├── russh
├── tmux Control Mode
├── terminal lifecycle/registry
├── alacritty_terminal
└── native GPU renderer
        │
        ▼
OpenSSH + tmux
```

### Keep the terminal data plane native

Do not stream the following through JavaScript:

- terminal output bytes;
- ANSI/VT streams;
- terminal cells;
- continuous scrollback/render data;
- rendering frames;
- cursor blinking;
- IME composition events that can remain inside the native terminal path.

React Native should own navigation, screens, controls, dialogs, settings, and low-frequency state snapshots.

### No server-side meeterm component

Do not introduce, for the core product:

- a meeterm gateway;
- a meeterm daemon;
- HTTP terminal transport;
- WebSocket terminal transport;
- a hosted relay as a required component.

The remote host should require ordinary SSH access and tmux only.

### No WebView terminal fallback

Do not replace the native terminal architecture with xterm.js/WebView as a shortcut unless a task explicitly changes the architecture after documenting the tradeoff.

The target is `alacritty_terminal` plus a native GPU renderer.

### Use ordinary tmux

Do not isolate meeterm into a separate tmux server/socket such as `tmux -L meeterm` for the normal product path.

A desktop user must be able to run:

```bash
tmux attach -t meeterm
```

without meeterm-specific desktop software.

## tmux integration

Use tmux Control Mode as the structured mobile integration boundary.

- Treat pane IDs (`%...`) and window IDs (`@...`) as stable runtime identities where appropriate.
- Decode Control Mode output as bytes; do not assume pane output is ordinary UTF-8 text.
- Route each pane's output to its own native terminal state.
- Preserve the underlying tmux window/pane layout while adapting presentation for mobile.
- Mobile pane selection/zoom must not permanently destroy the desktop layout.
- Do not install global tmux hooks or mutate user configuration without a demonstrated need and narrowly scoped design.

Avoid shell command construction from untrusted or user-visible names. Prefer typed command/argument encoding and explicit tmux targets.

## Rust / native structure

Prefer simple module boundaries first. Do not create crates, abstraction layers, traits, registries, or generalized frameworks solely because they may be useful later.

One mobile native package and one shared native library/runtime/terminal registry are preferred. Avoid independent native libraries that accidentally duplicate Tokio runtimes or live terminal registries.

Generated typed bindings are preferred for the low-frequency React Native ↔ Rust control plane. The native terminal view should bind to stable terminal IDs instead of owning the remote session lifetime.

The terminal semantics, opaque IDs, registry ownership, input contract, and native-only snapshot format should be shared across Android and iOS. Android and iOS should remain thin adapters for their own view/surface lifecycle, font metrics, text-input protocol, renderer backend, and native build integration. Do not duplicate the Rust terminal state or create a platform-specific second source of truth merely to make one adapter convenient.

## Japanese and CJK are first-class

Do not treat Japanese support as post-MVP polish.

The terminal foundation must account for:

- Japanese IME composition;
- CJK wide characters;
- fallback fonts;
- combining marks;
- Unicode text;
- representative emoji;
- terminal cell width consistency.

Do not route IME composition through a JavaScript `TextInput` merely because it is easier to implement.

## Security

- Verify SSH host keys. Never silently accept a changed known host key.
- Keep private keys, passwords, and passphrases out of logs.
- Store secrets in platform secure storage.
- Keep authentication and host-key behavior explicit and testable.
- Treat remote command/input encoding as a security boundary.

## State and lifecycle

- SSH is transport; tmux is durable state.
- Connection/reconnect behavior belongs in the Rust core, not scattered React hooks/timers.
- A React Native view unmount must not imply pane destruction.
- Backgrounding and transport loss should be recoverable through reconnect/resynchronization.
- Process-death recovery for full-screen TUIs is a technical-risk area; test it rather than assuming scrollback capture is sufficient.

## Scope discipline

meeterm should stay focused. Do not add broad remote-admin functionality without an explicit requirement.

Initial non-goals include:

- browser client;
- PC-specific meeterm application;
- hosted backend;
- file manager;
- system monitor;
- simultaneous phone/PC editing guarantees;
- proprietary session model replacing tmux.

Prefer the smallest implementation that proves the current milestone. Avoid speculative extensibility and over-engineering.

## Initial engineering sequence

The first implementation milestone is a dual-platform native terminal foundation. Prove shared Rust terminal semantics first, keep Android and iOS as thin native adapters, and establish both GitHub-hosted mobile jobs early. An environment-only iOS check must not be reported as iOS terminal verification.

Before broad UI or SSH/tmux features, prove:

1. Shared Rust-owned `alacritty_terminal::Term` semantics, deterministic resize, input encoding, and native-only snapshot fixtures.
2. One native package contract in which both platform views bind stable terminal IDs and retain one shared registry/runtime.
3. Expo Development Build/CNG generation for Android and iOS, with generated native directories remaining untracked.
4. Thin Android and iOS native `TerminalView` adapters that keep terminal bytes, cells, render frames, and IME composition out of JavaScript.
5. A real native GPU surface on each platform consuming the shared terminal snapshot. The iOS backend choice remains an evidence-driven implementation decision; see `docs/ARCHITECTURE.md`.
6. Japanese/CJK/font behavior, native Japanese IME composition and committed input, and deterministic resize behavior on the applicable platform paths.
7. GitHub-hosted Android emulator and iOS Simulator jobs that build, install, launch, signal native readiness and a first frame, and detect crashes while always uploading an observability bundle.

Only after both adapters and their meaningful mobile smoke gates are in place should the project add `russh`, tmux Control Mode, pane routing, reconnect/resync, and full product UI.

## CI and visual evidence boundary

For both mobile jobs, the machine-gated acceptance boundary is: generated project/build succeeds, the app installs, the app launches, the expected native module is ready, a first native terminal frame is reported, and the process does not crash. These gates do not claim physical-device GPU, font fallback, rotation, or IME parity.

Standard GitHub-hosted macOS runners do not guarantee Metal. The iOS job must distinguish a Metal first-frame marker from the Simulator-only native CoreGraphics fallback marker. The fallback still validates the Rust snapshot, CoreText, view, and input boundary, but it is not evidence that Metal executed.

The observability bundle is uploaded on every job, including failed jobs. After app launch it should contain a screenshot and sanitized native log; if launch or capture was not reached, it must contain an explicit unavailable diagnostic rather than a fake image. Do not add a screenshot-existence or pixel-difference gate at this stage. For every native UI change, Codex must download and actually view both the Android emulator and iOS Simulator screenshots before reporting visual success; an uploaded bundle or a passing process check is not visual review.

The iOS Simulator job is an unsigned simulator build/install boundary and must not require distribution certificates, provisioning profiles, or Apple signing secrets. Physical-device validation and TestFlight distribution are later, separate signed workflows with their own credentials and acceptance criteria.

Treat updates to Expo/React Native, the Rust terminal stack, Android SDK/NDK/Gradle, Xcode/SDK/CocoaPods, fonts, or the chosen iOS renderer backend as cross-platform native dependency changes. Regenerate CNG output on a fresh checkout and run both mobile jobs; do not patch ignored generated directories to accommodate a dependency update.

## Change policy

If an implementation task reveals that one of these invariants is technically unsound, do not silently work around it. Document the observed constraint, reproduce it with a focused test/PoC, and propose the smallest architecture change needed.
