# Product definition

## What meeterm is

meeterm is a smartphone-first client for an existing SSH development environment. The default backend is ordinary tmux; an authenticated SSH host may use an existing Herdr runtime after explicit selection in the picker.

It is not intended to create a second, mobile-only development environment. A developer should be able to work from a phone, stop, then later sit at a PC and continue by attaching to the same selected ordinary tmux session without rebuilding context.

The product promise is:

> The same development workspace, presented appropriately for the device you are using.

## Canonical tmux model

The remote tmux state is the source of truth.

| meeterm concept | tmux concept |
| --- | --- |
| Server | SSH host |
| Remote runtime | selected session on the ordinary tmux server |
| Workspace | window |
| Terminal | pane |

This mapping is deliberate and must not be inverted merely to simplify the mobile UI. The suggested name for an explicitly created new session is `meeterm`; it is also retained as a legacy last-used hint, not as a fixed runtime.

### Example

```text
selected session: <session-name> (example: `meeterm`)
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

On a PC, the user can run the ordinary tmux client against the selected
session:

```bash
tmux attach -t <selected-session>
```

and see the same windows and panes in their ordinary tmux layout. For example,
Codex and nvim can appear side by side within the same window while app-a and
app-b remain separate tmux windows. Substitute `meeterm` only when that is the
session selected by the user.

## Issue #17 common model and implementation

Issue #17 defines one semantic hierarchy shared by the implemented tmux and
Herdr backends:

| Common concept | tmux backend | Herdr backend |
| --- | --- | --- |
| Workspace | tmux window | Herdr workspace |
| TerminalGroup | one virtual mobile group per window | Herdr tab |
| Terminal | tmux pane | Herdr pane |

The tmux virtual group is presentation state. It must not create a remote
window, flatten a tmux layout, or break an ordinary attach to the selected
session. Backend/runtime selection happens explicitly after SSH host
authentication in the runtime picker. A saved profile owns the SSH endpoint
and authentication; legacy backend/runtime fields are migrated to a
non-authoritative last-used hint and do not bypass the picker. An additional
backend may use ordinary SSH to a user-selected remote runtime, while meeterm
itself still requires no gateway, daemon, hosted relay, HTTP API, or WebSocket
terminal transport.

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

SSH connections are transport and may disappear. For tmux, the selected
ordinary session is the durable development environment; for Herdr, the
selected running `default` or named session is durable. `meeterm` is only the
suggested new-session name and a legacy hint.

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

1. Add an SSH server profile containing the endpoint and authentication details.
2. Connect, verify the server host key, and authenticate.
3. On a fresh manual connection or cold start, review the runtime picker. It
   shows bounded, read-only sections for tmux and Herdr. A highlighted
   last-used row is a hint only; the user must select a candidate explicitly,
   even when only one candidate is available.
4. Select an existing tmux session, or a running Herdr `default`/named session.
5. View the selected runtime's workspaces.
6. Open a workspace and select its group when it has more than one.
7. View the group's panes as terminal tabs.
8. Work in one pane at phone-friendly size.
9. Switch panes without losing the other panes' native terminal state.
10. Leave the phone; the selected remote runtime continues running.
11. Later, attach with the ordinary tmux client or Herdr client and continue in
    the runtime's normal layout.

## Runtime picker and lifecycle

The authenticated SSH host connection is separate from runtime selection. The
picker is always shown for a fresh manual profile entry and a cold start. Its
discovery is bounded, backend-independent, and read-only: it never creates,
starts, attaches, or mutates a runtime. Duplicate names remain distinct by
backend, and a backend-specific failure stays inside that backend's section so
the other section remains usable.

The tmux section lists arbitrary sessions from the user's ordinary tmux server.
An explicitly verified empty result is an empty section; an ambiguous or
unexpected command failure is an error. Selecting a row attaches to its exact
discovered session identity. The user may explicitly create a detached tmux
session when needed; the form suggests `meeterm`, verifies the created identity,
and only then selects it. A session that disappears or is replaced between
listing and selection produces a stale-selection error and refreshes the list;
meeterm never silently creates a replacement.

The Herdr section lists `default` and named sessions with running/stopped
status. Only running candidates are selectable, and selection revalidates the
Herdr 0.9.0 / protocol 22 / schema 1 compatibility and direct stream-local
operations. Stopped rows explain that the session must be opened with the
ordinary Herdr client and then refreshed. Herdr start/create is not promised by
Issue #21; no meeterm action starts, creates, installs, or updates Herdr, and
there is no automatic fallback from Herdr to tmux.

An automatic transport reconnect may return directly to the already selected
backend/runtime only after host identity, executable/capability, runtime
identity, and compatibility are verified again. A missing runtime, tmux server
restart, same-name replacement, or uncertain identity returns to the picker
with an explanation and a fresh discovery. A manual reconnect is a fresh
selection flow as well.

The profile stores SSH endpoint/authentication metadata. Existing backend and
runtime fields represent a non-authoritative logical `lastUsedRuntime` hint;
profile IDs and credentials remain independent, and the hint is updated only
after the selected runtime reaches `Ready`. Switching servers or runtimes keeps
one selected runtime actor per host connection: the current controller is
released/drained before the next one is acquired, while the remote runtime and
processes remain alive.

Topology mutations are subject to the selected runtime's safety boundary. For
linked or shared tmux topology, workspace close and a terminal close that may
remove the final pane must be checked immediately before execution in the same
Rust actor/control queue. If cross-session safety cannot be proved at that
point, the mutation fails closed with an actionable message.

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
