# Product definition

meeterm is a smartphone-first SSH client for continuing the same remote work
from phone and desktop. The selected remote runtime is the durable workspace;
the phone adapts its presentation without creating a separate environment.

## Model

| Concept | Ordinary tmux | Herdr |
| --- | --- | --- |
| Server | SSH host | SSH host |
| Runtime | selected ordinary session | selected running default/named session |
| Workspace | window | workspace |
| TerminalGroup | one virtual mobile group per window | tab |
| Terminal | pane | pane |

The tmux virtual group creates no remote object. Panes appear as mobile tabs;
the selected pane receives a phone-sized viewport while desktop layout remains
recoverable. On PC, ordinary `tmux attach -t <selected-session>` or the normal
Herdr client continues the work. Simultaneous interactive phone/PC use is not
an initial requirement. `meeterm` is a suggested new tmux session name and a
legacy last-used hint, never a required runtime name.

## Connection and selection

1. Add/select an SSH profile and authenticate, explicitly verifying the host key.
2. Every fresh manual connection and cold start performs bounded, read-only
   discovery and shows a picker grouped by tmux and Herdr. Even one candidate
   requires explicit selection. Last-used is a hint, not an automatic attach.
3. Select an existing tmux session or running Herdr session. Creating a detached
   tmux session is a separate explicit action. A stale selection refreshes;
   it never creates a replacement. Herdr start/create/install/update stays in
   the normal Herdr tools; meeterm does not do it or silently fall back to tmux.
4. Open a workspace, group and terminal. Tab selection preserves native terminal
   state and the other remote processes.
5. Disconnect or hand off to PC while leaving the remote runtime alive.

Backend discovery errors remain local to their section. Duplicate display names
remain distinct by runtime identity. Credentials and profile identity belong to
the SSH endpoint; changing a runtime hint must not change secure-storage identity.
Write the hint only after the selected runtime reaches Ready.

## Switching and recovery

Headers show the current Server and Session. Opening the hierarchical switcher
only displays known state; it does not connect or discover. Choosing a server
releases the old owner, authenticates/discovers for the destination, and requires
explicit Session selection before returning to Workspaces. One owner is active
at a time. Authentication and changed-key review remain explicit.

A rejected switch before release keeps the current work. After release, failure
or cancellation must not revive the old owner as connected; the next attempt uses
a saved profile or connection details. Release never kills remote processes.
Local history need not survive an intentional switch.

Transport loss after Ready instead retains the workspace, terminal and local
history as read-only and automatically restores the same target. Reconnect and
Retry preserve that intent; Change starts a fresh selection. Stop on concrete
host/auth/target/capability/controller/resynchronization failures, without
retargeting or replaying uncertain input. The reason-specific actions and exact
identity gates are defined in [ARCHITECTURE.md](ARCHITECTURE.md).

## Terminal and everyday features

- Terminal state, rendering and input stay native. Japanese/CJK, native IME,
  wide/combining characters, font fallback and deterministic resize are core.
- Saved profiles and opt-in credentials in platform secure storage; trusted
  host identities remain pinned.
- Workspace/group/terminal selection and supported create/rename/close actions;
  destructive operations preserve other sessions and use explicit targets.
- Native selection/copy, Ctrl/Alt and navigation keys, bounded local scrollback,
  persisted font size and independently selected app/terminal themes.
- Image attachment: pick/normalize one image, upload over the existing SSH
  connection, explicitly insert its quoted path into the original terminal.
  Upload, insert and model consumption are separate; meeterm never sends Enter.

Implementation and UI detail belong in [ARCHITECTURE.md](ARCHITECTURE.md),
[UI_UX.md](UI_UX.md), [SSH.md](SSH.md) and [HERDR.md](HERDR.md).
Testing frequency is defined only by [TESTING.md](TESTING.md).

## Scope and direction

No browser client, PC-specific meeterm app, hosted relay, file manager, system
monitor, or proprietary replacement for remote session state. No meeterm server
component: ordinary SSH plus the selected existing runtime is sufficient.
Herdr is an external application, not permission to introduce a meeterm daemon.

Prefer the smallest implementation that serves the current requirement.
[Engineering principles](ENGINEERING_PRINCIPLES.md) guide tradeoffs. The meerkat
brand is restrained; the interface stays quiet and focused on terminal work.
