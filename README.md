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

Greenfield. The first engineering milestone is a native terminal vertical slice on Android before broader UI work. See issue #1 for the active PoC scope.

## Design docs

- [Product definition](docs/PRODUCT.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Development](docs/DEVELOPMENT.md)
- [ADR 0001: native terminal first](docs/decisions/0001-native-terminal-first.md)
- [Agent instructions](AGENTS.md)
