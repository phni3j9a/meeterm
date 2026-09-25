# meeterm

**meeterm** is a smartphone-first SSH client for carrying the same development environment between phone and desktop. It supports the ordinary tmux backend and an explicitly selected Herdr backend.

The core idea is simple: the phone is not a separate development environment. It is another viewport into the selected tmux or Herdr workspace you can later open from a PC.

## Product model

- **Connection** = SSH host
- **Runtime** = a selected ordinary tmux session, or a selected running Herdr `default`/named session
- **Workspace** = tmux window, or Herdr workspace
- **TerminalGroup** = one virtual group for a tmux window, or a Herdr tab
- **Terminal** = tmux pane, or Herdr pane

On mobile, panes are presented as tabs and the active pane is expanded for a phone-sized viewport. A profile stores the SSH endpoint and authentication details. After authentication, the user explicitly selects a runtime from the tmux and Herdr session lists; a saved backend/runtime value is only a last-used hint. On desktop, `tmux attach -t <selected-session>` exposes the selected tmux windows and panes using their normal layout, while a selected Herdr runtime remains available to the normal Herdr client.

## Issue #17 implementation status

The common model is `Workspace → TerminalGroup → Terminal`. The Rust/native
backend boundary maps tmux to `window → virtual group → pane` and Herdr to
`workspace → tab → pane`. Herdr support uses the existing 0.9.0 public protocol
(protocol 22, schema 1) over direct SSH stream-local control. Terminal frames,
input, scroll, resize, lifecycle, stable terminal IDs, and native rendering
remain below the JavaScript boundary; Herdr itself is unchanged.

The [production native integration](docs/evidence/issue-17-herdr-native.md)
exercises real Herdr 0.9.0 through an isolated russh endpoint, including normal
PC client handoff and safe handling of related Git workspaces. The prior
OpenSSH public CLI proof remains historical evidence. The
[mobile acceptance record](docs/evidence/issue-17-herdr-mobile.md) tracks exact
source revisions, suite results, actual screenshot review, and remaining limits.
See [`docs/HERDR.md`](docs/HERDR.md) for setup, input semantics, handoff behavior,
close-scope restrictions, and verification commands.

## Issue #21 implementation status

Fresh and manual connections now authenticate the SSH host before showing a
runtime picker. Discovery is bounded and read-only. tmux sessions can be
selected or explicitly created as detached sessions; `meeterm` is the suggested
new-session name rather than a fixed target. Herdr lists running and stopped
sessions, allows selection only for running sessions, and resolves the 0.9.0
binary from the non-interactive PATH, the official `~/.local/bin` default, and
common package-manager locations. Starting or creating Herdr sessions remains
an ordinary Herdr-client action.

Same-process transport recovery keeps the last authoritative workspace and
native terminal visible but read-only, and automatically resumes both tmux and
Herdr without a tap when their concrete checks succeed. tmux verifies the
selected session and pane. Herdr verifies the approved SSH host and
authentication, compatible Herdr capability, selected runtime, original
stable terminal ID, ordinary controller lease without takeover, and an
authoritative full frame. Herdr 0.9.0 does not expose a comparable
server-instance identity; that missing proof alone does not block recovery.
Recovery stops on a concrete mismatch, authentication or synchronization
failure, missing runtime/terminal, incompatibility, or controller conflict,
and keeps the stale screen read-only. `runtimeMismatch`, controller conflict,
and retry-exhaustion/unknown stops offer **Retry** and **Change**; the
`runtimeMissing`, `terminalMissing`, and `incompatible` reasons offer **Change**
only. A changed host key offers **Review key**, and authentication failure
offers **Connection details**. Reconnect in Workspaces or the Server
sheet is available while retained recovery is reconnecting or stopped for a
retry-eligible reason, and uses the same-intent path; both controls are hidden
during resynchronization and for Change-only or security stops.

When retained work exists, **Reconnect** in Workspaces and the Server sheet,
and **Retry**, all use the same-intent `retryRecovery` path. Its first automatic attempt is immediate;
bounded exponential backoff applies after failures, and foreground return or a
network-change notification wakes a sleeping retry. `reconnect` and the
runtime picker remain the fresh-selection path for cold start, explicit
server/Session changes, or **Change** after the retained target is lost. Saved
backend/runtime fields remain non-authoritative last-used hints and are updated
only after the selected runtime reaches `Ready`.

tmux mobile zoom ownership is tracked by window identity within the selected
runtime/generation, so pane switches do not lose the cleanup target and
pre-existing desktop zoom is preserved. Disconnect performs bounded,
same-Control-Mode cleanup and reports `layout_restore_unconfirmed` through the
existing connection error fields if the desktop layout cannot be confirmed;
the local connection still ends in `Disconnected` without replaying input or
starting automatic recovery. See [SSH validation and limitations](docs/SSH.md)
for the exact cleanup boundary and current evidence.

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
├── selected ordinary tmux session
│   ├── window = Workspace
│   └── pane   = Terminal
└── selected existing Herdr 0.9.0 session/socket
```

Terminal byte streams, ANSI parsing, terminal cell state, scrollback, IME composition, and rendering frames must stay out of JavaScript. React Native owns app chrome and product state; the native core owns terminal data and rendering.

## Project status

The first evaluation app has passed real SSH connection, workspace/pane selection, native input, and disconnect/reconnect on both the hosted Android emulator and iOS Simulator. The [verified run on `012c987`](https://github.com/phni3j9a/meeterm/actions/runs/34243185286) also passed both native readiness/first-frame/no-crash gates, iOS UIKit input tests, and ordinary desktop tmux attach. Android and iOS screenshots were downloaded and reviewed. iOS reported Metal frames in this Simulator run; physical iPhone GPU and Japanese IME behavior remain unverified. A self-contained Android APK that does not need Metro is available through the [installation guide](docs/FIRST_APP.md#android). See the [Issue #13 acceptance record](docs/evidence/issue-13-ios-acceptance.md) for the iOS launch fix, evidence, and remaining limits.

The shared Rust terminal foundation has Android and iOS native adapters, with GLES on Android and Metal on iOS. Hosted iOS Simulators without Metal use an explicitly identified native CoreGraphics fallback. Both platforms have build/install/launch/first-frame smoke jobs. The original Android foundation was also exercised on a physical Pixel 3, including Japanese IME composition/commit and resize; that historical device evidence remains separate from later SSH validation.

The session path uses a Rust-owned `russh` connection and an explicit backend. Use **Connect** to enter the SSH host, username, and OpenSSH private-key credential (with an optional passphrase) or SSH `password`, then explicitly verify the host key. After authentication, choose an existing tmux session or a running Herdr `default`/named session. tmux creation is a separate explicit action; Herdr creation and startup stay in the normal Herdr client. **Disconnect** leaves the selected remote runtime running. Native automatic recovery retains the last authoritative workspace as read-only and resumes tmux or Herdr with no tap after concrete target, lease, and full-frame checks succeed. **Reconnect** and **Retry** use same-intent recovery while retained work exists; the first retry is immediate, and foreground/network events wake backoff. A fresh manual/cold connection, explicit server/Session change, or **Change** after target loss uses the picker. Keyboard-interactive prompts and MFA are not added. The daily-use milestone adds saved server profiles and opt-in platform-secure credentials, so a saved server can be reopened after an app restart without returning its secret to JavaScript. Approved host identities remain pinned. Input, output, resize, scroll, and rendering stay in the native terminal path. The workspace-first real app follows the [HTML mock](docs/mock/README.md); normal startup shows the unconnected workspace screen. Workspace/group/pane selection, reconnect, and PC handoff guidance use the real native session state. Daily-use additions also cover window/pane management, native automatic reconnect, selection/copy, Ctrl/Alt input and persisted terminal preferences; [the milestone record](docs/DAILY_USE.md) distinguishes implementation from verified acceptance. [First-app usage and acceptance evidence](docs/FIRST_APP.md) tracks the implementation and outstanding mobile verification. See [SSH validation and limitations](docs/SSH.md), the [mobile CI guide](docs/CI_MOBILE.md), and the [Android PoC runbook](docs/POC_ANDROID.md).

## Quick start

Use the pinned environment in [the Android PoC runbook](docs/POC_ANDROID.md), then run:

```sh
npm ci
npx expo prebuild --platform android --non-interactive --no-install
npx expo run:android --device
```

`android/` and `ios/` are Expo CNG outputs when generated locally; the root app config, local native module, and Rust source remain the source of truth. The recorded device result and its known limitations are in the runbook; a future emulator-only run must not overwrite that evidence or be treated as proof of native Japanese IME and GPU behavior.

## Design docs

- [Current mobile UI, design research, and verification scope](docs/UI_UX.md)
- [Product definition](docs/PRODUCT.md)
- [Engineering principles](docs/ENGINEERING_PRINCIPLES.md)
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
