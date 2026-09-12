# Product definition

## What meeterm is

meeterm is a smartphone-first client for an existing SSH development environment. The default backend is ordinary tmux; an explicitly selected profile may use an existing Herdr runtime.

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

## Issue #17 common model and implementation

Issue #17 defines one semantic hierarchy shared by the implemented tmux and
Herdr backends:

| Common concept | tmux backend | Herdr backend |
| --- | --- | --- |
| Workspace | tmux window | Herdr workspace |
| TerminalGroup | one virtual mobile group per window | Herdr tab |
| Terminal | tmux pane | Herdr pane |

The tmux virtual group is presentation state. It must not create a remote
window, flatten a tmux layout, or break `tmux attach -t meeterm`. A backend is
selected explicitly for a saved profile/runtime; a profile without a backend
continues to use tmux. An additional backend may use ordinary SSH to a
user-selected remote runtime, while meeterm itself still requires no gateway,
daemon, hosted relay, HTTP API, or WebSocket terminal transport.

The Rust/native backend boundary, profile/runtime fields, common snapshots,
group operations, and Herdr mobile routes are implemented. The production native
integration has passed against real Herdr, including ordinary PC client handoff
and linked-workspace close safety. See the [native evidence](evidence/issue-17-herdr-native.md)
and [mobile acceptance record](evidence/issue-17-herdr-mobile.md) for measured
results, exact source revisions, and validation limits.

Herdr is an existing external application and must remain unchanged. Input
adaptation belongs in meeterm using existing public interfaces; an upstream API
addition is not a prerequisite of this issue. Mode-aware special keys and
Japanese/LF bracketed paste are verified through existing public operations.

Herdr 0.9.0 / protocol 22 / schema 1 is the fixed compatibility target. It is
connected through the public direct stream-local API over ordinary SSH. See
[`HERDR.md`](HERDR.md) for the input, scroll, resize, lease, and handoff
contract, and the [Issue #17 feasibility record](evidence/issue-17-herdr-feasibility.md)
for historical probe evidence. Herdr remains unchanged.

## Product principles

### 1. The selected backend is the durable workspace

SSH connections are transport and may disappear. For tmux, session `meeterm`
is the durable development environment; for Herdr, the selected `default` or
named session is durable.

The app must tolerate backgrounding, network loss, and process restart by
reconnecting to the selected remote runtime instead of treating the SSH
connection as the source of truth.

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

The remote host should require ordinary SSH access and the selected runtime:
tmux or an existing Herdr 0.9.0 session/socket.

meeterm must not require a dedicated gateway, daemon, HTTP API, WebSocket service, or self-hosted meeterm backend for the core product.

That rule concerns a meeterm-owned relay. It does not prohibit connecting over
ordinary SSH to a user-selected Herdr server/runtime when the Herdr backend is
explicitly selected.

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
3. Select tmux/session `meeterm`, or Herdr/runtime `default` or a named session.
4. View the selected runtime's workspaces.
5. Open a workspace and select its group when it has more than one.
6. View the group's panes as terminal tabs.
7. Work in one pane at phone-friendly size.
8. Switch panes without losing the other panes' native terminal state.
9. Leave the phone; the selected remote runtime continues running.
10. Later, attach with the ordinary tmux client or Herdr client and continue in the runtime's normal layout.

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
