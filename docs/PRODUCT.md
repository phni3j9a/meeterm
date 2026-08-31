# Product definition

## What meeterm is

meeterm is a smartphone-first client for an existing SSH + tmux development environment.

It is not intended to create a second, mobile-only development environment. A developer should be able to work from a phone, stop, then later sit at a PC and continue by attaching to the same tmux session without rebuilding context.

The product promise is:

> The same development workspace, presented appropriately for the device you are using.

## Canonical tmux model

The remote tmux state is the source of truth.

| meeterm concept | tmux concept |
| --- | --- |
| Server | SSH host |
| Managed environment | session named `meeterm` |
| Workspace | window |
| Terminal | pane |

This mapping is deliberate and must not be inverted merely to simplify the mobile UI.

### Example

```text
session: meeterm
├── window: app-a
│   ├── pane: Codex
│   └── pane: nvim / shell
├── window: app-b
│   ├── pane: Codex
│   └── pane: shell
└── window: rfkit-rs
    ├── pane: Codex
    └── pane: tests
```

On a phone, `app-a` is a workspace and its panes are shown as terminal tabs. The selected pane receives the phone-sized viewport and is presented full-screen.

On a PC, the user can run:

```bash
tmux attach -t meeterm
```

and see the same windows and panes in their ordinary tmux layout. For example, Codex and nvim can appear side by side within the same window while app-a and app-b remain separate tmux windows.

## Product principles

### 1. tmux is the durable workspace

SSH connections are transport and may disappear. The tmux session is the durable development environment.

The app must tolerate backgrounding, network loss, and process restart by reconnecting to the remote tmux state instead of treating the SSH connection as the source of truth.

### 2. Mobile presentation must not redefine the remote model

A pane is still a pane even when the phone presents panes as tabs. A window is still a window even when the phone calls it a workspace.

Device-specific UI should adapt presentation, not mutate the semantic model solely for presentation convenience.

### 3. Phone and PC optimize the same state differently

Mobile:

- one active workspace at a time;
- panes exposed as tabs;
- selected pane expanded for the phone viewport;
- touch-first navigation and keyboard affordances.

Desktop tmux:

- ordinary tmux window switching;
- ordinary pane layouts such as side-by-side Codex and nvim;
- no meeterm-specific desktop client required.

Simultaneous interactive use from phone and PC is not an initial product requirement. Smooth handoff between them is.

### 4. No meeterm server component

The remote host should require only ordinary SSH access and tmux.

meeterm must not require a dedicated gateway, daemon, HTTP API, WebSocket service, or self-hosted meeterm backend for the core product.

### 5. Native terminal quality is a core product requirement

The terminal is not a generic web view embedded inside an app. It should behave like a first-class mobile terminal with:

- responsive rendering;
- correct terminal semantics;
- durable scrollback while the app process lives;
- accurate resize handling;
- Japanese/CJK text support;
- robust Japanese IME composition;
- reliable special-key and modifier input;
- smooth pane/tab switching.

Japanese input and CJK rendering are first-class acceptance requirements, not optional polish.

## Primary user flow

1. Add an SSH server.
2. Connect and verify the server host key.
3. Ensure or attach to tmux session `meeterm`.
4. View tmux windows as workspaces.
5. Open a workspace.
6. View its panes as terminal tabs.
7. Work in one pane at phone-friendly size.
8. Switch panes without losing the other panes' terminal state.
9. Leave the phone; tmux continues running remotely.
10. Later, on a PC, run `tmux attach -t meeterm` and continue in the normal tmux layout.

## Non-goals for the first product

- A browser client.
- A PC-specific meeterm application.
- A tablet-specific layout as a separate product surface.
- A hosted relay or synchronization backend.
- Simultaneous phone/PC editing guarantees.
- File-manager, system-monitoring, or general remote-admin features.
- Replacing tmux with a proprietary session model.

These may be reconsidered only if concrete product needs justify them.

## Brand direction

The meerkat is meeterm's theme character: an alert companion that watches over long-running development sessions.

The visual product should remain mature, quiet, minimal, and professional. Character use should be restrained, especially inside the active terminal experience.
