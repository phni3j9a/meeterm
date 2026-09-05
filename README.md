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

The shared Rust terminal foundation has Android and iOS native adapters, with GLES on Android and Metal on iOS. Hosted iOS Simulators without Metal use an explicitly identified native CoreGraphics fallback. Both platforms have build/install/launch/first-frame smoke jobs. The original Android foundation was also exercised on a physical Pixel 3, including Japanese IME composition/commit and resize; that historical device evidence remains separate from later SSH validation.

The SSH slice adds a Rust-owned `russh` connection, explicit persisted host-key trust, public-key authentication, and an interactive remote PTY. Use **Connect** to enter a host, username, OpenSSH private key, and optional passphrase. Credentials are transient; only approved host identities are saved. Input, output, and resize stay in the native terminal path. The fixed demo runs before connecting. tmux, server profiles, and automatic reconnect remain future work. See [SSH validation and limitations](docs/SSH.md), the [mobile CI guide](docs/CI_MOBILE.md), and the [Android PoC runbook](docs/POC_ANDROID.md).

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
- [SSH validation and limitations](docs/SSH.md)
- [Mobile CI guide](docs/CI_MOBILE.md)
- [Android PoC runbook](docs/POC_ANDROID.md)
- [Issue #1 Android device validation](docs/evidence/issue-1-android-device.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)
- [ADR 0001: native terminal first](docs/decisions/0001-native-terminal-first.md)
- [Agent instructions](AGENTS.md)

The public Fressh repository was consulted during feasibility research only. meeterm does not copy Fressh source and does not depend on a Fressh binary; see [the provenance notice](THIRD_PARTY_NOTICES.md).
