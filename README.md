# meeterm

**meeterm** is a smartphone-first SSH client for carrying the same development environment between phone and desktop. It supports the ordinary tmux backend and an explicitly selected Herdr backend.

The core idea is simple: the phone is not a separate development environment. It is another viewport into the same tmux workspace you can later attach to from a PC.

## Product model

- **Connection** = SSH host
- **Runtime** = tmux session `meeterm`, or a Herdr `default`/named session
- **Workspace** = tmux window, or Herdr workspace
- **TerminalGroup** = one virtual group for a tmux window, or a Herdr tab
- **Terminal** = tmux pane, or Herdr pane

On mobile, panes are presented as tabs and the active pane is expanded for a phone-sized viewport. A profile selects its backend and runtime explicitly; profiles saved before backend support continue to use tmux. On desktop, `tmux attach -t meeterm` exposes the tmux windows and panes using their normal layout, while a Herdr runtime remains available to the normal Herdr client.

## Issue #17 implementation status

The common model is `Workspace → TerminalGroup → Terminal`. The Rust/native
backend boundary maps tmux to `window → virtual group → pane` and Herdr to
`workspace → tab → pane`. Herdr support uses the existing 0.9.0 public protocol
(protocol 22, schema 1) over direct SSH stream-local control. Terminal frames,
input, scroll, resize, lifecycle, stable terminal IDs, and native rendering
remain below the JavaScript boundary; Herdr itself is unchanged.

Issue #17 implementation is present, but the full acceptance record is still
pending. The [production native integration](docs/evidence/issue-17-herdr-native.md)
passed against an isolated russh endpoint and real Herdr 0.9.0, including normal
PC client handoff. The prior OpenSSH public CLI proof remains historical evidence.
The new Rust CI job and mobile screen evidence remain pending, so this status does not claim
that Issue #17 is complete. See [`docs/HERDR.md`](docs/HERDR.md) for the exact
protocol, input semantics, handoff behavior, and verification commands.

## Architecture direction

```text
React Native / Expo
        │
        │ commands, navigation, snapshots only
        ▼
Rust native core
├── russh
├── backend selector
│   ├── tmux Control Mode
│   └── Herdr direct stream-local control
├── connection / terminal lifecycle
├── alacritty_terminal
└── native GPU renderer
        │
        │ ordinary SSH
        ▼
OpenSSH server
├── tmux session: meeterm
│   ├── window = Workspace
│   └── pane   = Terminal
└── existing Herdr 0.9.0 session/socket
```

Terminal byte streams, ANSI parsing, terminal cell state, scrollback, IME composition, and rendering frames must stay out of JavaScript. React Native owns app chrome and product state; the native core owns terminal data and rendering.

## Project status

The first evaluation app has passed real SSH connection, workspace/pane selection, native input, and disconnect/reconnect on both the hosted Android emulator and iOS Simulator. The [verified run on `012c987`](https://github.com/phni3j9a/meeterm/actions/runs/34243185286) also passed both native readiness/first-frame/no-crash gates, iOS UIKit input tests, and ordinary desktop tmux attach. Android and iOS screenshots were downloaded and reviewed. iOS reported Metal frames in this Simulator run; physical iPhone GPU and Japanese IME behavior remain unverified. A self-contained Android APK that does not need Metro is available through the [installation guide](docs/FIRST_APP.md#android). See the [Issue #13 acceptance record](docs/evidence/issue-13-ios-acceptance.md) for the iOS launch fix, evidence, and remaining limits.

The shared Rust terminal foundation has Android and iOS native adapters, with GLES on Android and Metal on iOS. Hosted iOS Simulators without Metal use an explicitly identified native CoreGraphics fallback. Both platforms have build/install/launch/first-frame smoke jobs. The original Android foundation was also exercised on a physical Pixel 3, including Japanese IME composition/commit and resize; that historical device evidence remains separate from later SSH validation.

The session path uses a Rust-owned `russh` connection and an explicit backend. The tmux path attaches to or creates the ordinary `meeterm` session through Control Mode; the Herdr path connects to the selected existing `default` or named session through its direct public stream-local API. Use **Connect** to enter a host and username, select the backend/runtime, then choose OpenSSH private-key authentication (with an optional passphrase) or the SSH `password` authentication method. Explicitly verify the host key. Keyboard-interactive prompts and MFA are not added. **Disconnect** leaves the selected remote runtime running, and **Reconnect** uses the process-local authentication credential to resume it. The daily-use milestone adds saved server profiles and opt-in platform-secure credentials, so a saved server can be reopened after an app restart without returning its secret to JavaScript. Approved host identities remain pinned. Input, output, resize, scroll, and rendering stay in the native terminal path. The workspace-first real app follows the [HTML mock](docs/mock/README.md); normal startup shows the unconnected workspace screen. Workspace/group/pane selection, reconnect, and PC handoff guidance use the real native session state. Daily-use additions also cover window/pane management, native automatic reconnect, selection/copy, Ctrl/Alt input and persisted terminal preferences; [the milestone record](docs/DAILY_USE.md) distinguishes implementation from verified acceptance. [First-app usage and acceptance evidence](docs/FIRST_APP.md) tracks the implementation and outstanding mobile verification. See [SSH validation and limitations](docs/SSH.md), the [mobile CI guide](docs/CI_MOBILE.md), and the [Android PoC runbook](docs/POC_ANDROID.md).

## Quick start

Use the pinned environment in [the Android PoC runbook](docs/POC_ANDROID.md), then run:

```sh
npm ci
npx expo prebuild --platform android --non-interactive --no-install
npx expo run:android --device
```

`android/` and `ios/` are Expo CNG outputs when generated locally; the root app config, local native module, and Rust source remain the source of truth. The recorded device result and its known limitations are in the runbook; a future emulator-only run must not overwrite that evidence or be treated as proof of native Japanese IME and GPU behavior.

## Design docs

- [Product definition](docs/PRODUCT.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Herdr backend and usage](docs/HERDR.md)
- [Development](docs/DEVELOPMENT.md)
- [Standard testing workflow](docs/TESTING.md)
- [First-app evaluation, installation, and evidence](docs/FIRST_APP.md)
- [Daily-use features and acceptance](docs/DAILY_USE.md)
- [SSH password authentication and Fold7 installation evidence](docs/evidence/password-auth-fold7.md)
- [SSH validation and limitations](docs/SSH.md)
- [Mobile CI guide](docs/CI_MOBILE.md)
- [Android PoC runbook](docs/POC_ANDROID.md)
- [Issue #1 Android device validation](docs/evidence/issue-1-android-device.md)
- [Issue #13 iOS SSH and native smoke acceptance](docs/evidence/issue-13-ios-acceptance.md)
- [Issue #17 Herdr feasibility evidence](docs/evidence/issue-17-herdr-feasibility.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)
- [ADR 0001: native terminal first](docs/decisions/0001-native-terminal-first.md)
- [Agent instructions](AGENTS.md)

The public Fressh repository was consulted during feasibility research only. meeterm does not copy Fressh source and does not depend on a Fressh binary; see [the provenance notice](THIRD_PARTY_NOTICES.md).
