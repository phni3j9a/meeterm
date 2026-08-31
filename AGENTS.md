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

The first implementation milestone is the native terminal vertical slice on Android. Before broad UI or SSH/tmux features, prove:

1. Expo Development Build + custom native package.
2. Native `TerminalView` mounted from React Native.
3. Rust-owned `alacritty_terminal::Term`.
4. Fixed ANSI/VT byte input.
5. Native GPU rendering.
6. Japanese/CJK/font behavior.
7. Native Japanese IME composition and committed input.
8. Deterministic resize behavior.

Only then add `russh`, tmux Control Mode, pane routing, reconnect/resync, and full product UI.

## Change policy

If an implementation task reveals that one of these invariants is technically unsound, do not silently work around it. Document the observed constraint, reproduce it with a focused test/PoC, and propose the smallest architecture change needed.
