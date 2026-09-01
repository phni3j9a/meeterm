# meeterm

**meeterm** is a smartphone-first SSH and tmux client for carrying the same development environment between phone and desktop.

The core idea is simple: the phone is not a separate development environment. It is another viewport into the same tmux workspace you can later attach to from a PC.

## Product model

- **Server** = SSH host
- **Managed session** = tmux session `meeterm`
- **Workspace** = tmux window
- **Terminal** = tmux pane

On mobile, panes are presented as tabs and the active pane is expanded for a phone-sized viewport. On desktop, `tmux attach -t meeterm` exposes the same windows and panes using their normal tmux layout.

## Architecture direction

```text
React Native / Expo
        │
        │ commands, navigation, snapshots only
        ▼
Rust native core
├── russh
├── tmux Control Mode
├── connection / terminal lifecycle
├── alacritty_terminal
└── native GPU renderer
        │
        │ SSH
        ▼
OpenSSH server
└── tmux session: meeterm
    ├── window = Workspace
    └── pane   = Terminal
```

Terminal byte streams, ANSI parsing, terminal cell state, scrollback, IME composition, and rendering frames must stay out of JavaScript. React Native owns app chrome and product state; the native core owns terminal data and rendering.

## Project status

Greenfield. The first engineering milestone is a native terminal vertical slice on Android before broader UI work. The repository now contains the initial Expo scaffold, a custom Android `TerminalView`, a Rust-owned `alacritty_terminal::Term`, and a native GLES renderer. The Development Build vertical slice has been exercised on a physical Pixel 3 running Android 11, including CJK/combining text, native Japanese IME composition/commit, rotation, keyboard resize, and compact-window resize.

The milestone deliberately has no SSH, tmux, server profiles, or remote backend. Committed input is currently looped back into the local Rust terminal only for native IME testing. See the [Android PoC runbook](docs/POC_ANDROID.md) for pinned prerequisites, reproducible commands, automated checks, and the manual device checklist.

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
- [Development](docs/DEVELOPMENT.md)
- [Android PoC runbook](docs/POC_ANDROID.md)
- [Issue #1 Android device validation](docs/evidence/issue-1-android-device.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)
- [ADR 0001: native terminal first](docs/decisions/0001-native-terminal-first.md)
- [Agent instructions](AGENTS.md)

The public Fressh repository was consulted during feasibility research only. meeterm does not copy Fressh source and does not depend on a Fressh binary; see [the provenance notice](THIRD_PARTY_NOTICES.md).
