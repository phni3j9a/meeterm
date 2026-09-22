# Architecture

This document defines the intended architecture of meeterm. Product semantics are defined in `PRODUCT.md`; this document defines the implementation boundaries that preserve them.

## System overview

```text
React Native / Expo
│
│ commands, navigation, low-frequency state snapshots
│
├───────────────────────────────┐
│                               │
▼                               ▼
Control bridge              Native Terminal View

└──────────────┬────────────────┘
               ▼
        Rust native core
        ├── Tokio runtime
        ├── SSH lifecycle / russh
        ├── backend selector
        │   ├── tmux Control Mode
        │   └── Herdr direct stream-local control
        ├── terminal registry
        ├── alacritty_terminal
        └── native GPU renderer
               │
               │ ordinary SSH
═══════════════╪════════════════════
               ▼
          OpenSSH server
          ├── selected ordinary tmux session
          │   ├── window = Workspace
          │   └── pane   = Terminal
          └── selected existing Herdr 0.9.0 session/socket
```

There is no meeterm server-side component in the core architecture.

## Issue #17 backend boundary

Issue #17 adds a backend boundary around the same Rust SSH lifecycle, terminal
registry, `alacritty_terminal::Term`, native snapshot format, and bounded
input/resize transport. Both paths are implemented:

```text
Workspace → TerminalGroup → Terminal

tmux:  window → virtual group → pane
Herdr: workspace → tab           → pane
```

The tmux group is local presentation state and must never create a second
window or mutate the ordinary tmux desktop layout. Herdr groups represent
remote tabs. Herdr group deletion uses `tab.close`; workspace deletion uses
`workspace.close` with `close_group: false`. Because Herdr 0.9.0 can implicitly
close a Git workspace group from its parent when confirmation is disabled,
pane/group closes in a parent with related workspaces are refused after a fresh
snapshot. Normal workspaces and linked children remain operable. Backend/runtime
selection is explicit after authenticated runtime discovery. Legacy backend and
runtime profile fields are a non-authoritative last-used hint, updated only
after `Ready`; missing legacy fields may seed the tmux suggestion but never
bypass the picker. A missing Herdr capability is an explicit error rather than
a silent tmux fallback. Remote identifiers remain opaque and scoped by
connection, backend, and runtime. Local native terminal IDs remain separate
because a Herdr pane ID may change when it moves.

The backend actor may differ, but terminal bytes, ANSI/VT parsing, cells,
scrollback, render frames, and IME composition remain native. Only hierarchy,
selection, lifecycle, group operations, visibility, metadata, and errors cross
the low-frequency control bridge. Herdr connects over ordinary SSH to its
selected existing runtime through direct stream-local public operations; this
does not add a meeterm gateway, daemon, HTTP API, or WebSocket terminal
transport.

The fixed Herdr compatibility target is 0.9.0 / protocol 22 / schema 1. The
live Rust integration has passed; the [mobile acceptance record](evidence/issue-17-herdr-mobile.md)
tracks CI source revisions, actual screen review, and remaining limits. Input adaptation uses Herdr's existing `send_text`,
`send_keys`, and `send_input` operations; a modified Herdr or upstream API
addition is not required. See [`HERDR.md`](HERDR.md) and the
[`Issue #17 evidence record`](evidence/issue-17-herdr-feasibility.md).

## Issue #21 runtime picker and lifecycle

Issue #21 separates an authenticated SSH host connection from runtime
selection. The native lifecycle is conceptually:

```text
Connecting
  → HostKeyPending
  → Authenticating
  → HostAuthenticated / DiscoveringRuntimes
  → RuntimeSelection
  → AttachingSelectedBackend
  → Synchronizing
  → Ready
```

Fresh manual connection and cold start always enter `DiscoveringRuntimes` and
show an explicit picker. The picker exposes only bounded, low-frequency
candidate summaries: a native candidate ID, backend, display name, status,
suggested/last-used state, and backend-local error. It also carries a
connection generation and discovery revision so stale asynchronous results and
double taps can be discarded. The user must explicitly select a candidate even
when the list has only one row. Runtime discovery is read-only and must not
create, start, attach, or mutate a runtime.

The tmux section lists arbitrary sessions from the user's ordinary tmux server.
Only a verified no-server/no-session result is an empty section; other exit
statuses, malformed output, or timeouts remain errors. The native layer retains
the discovered session ID `$N` and its tmux server epoch. `$N` is exact only
within that epoch, while a name is a display and last-used hint. Selecting a
row targets that exact live session. Explicit tmux creation is a separate
detached `new-session` operation: it uses a safely encoded name (suggesting
`meeterm`), verifies the returned identity, and then binds it. Normal selection
must not use an attach-or-create operation that could create after a race.

The Herdr section resolves the compatible Herdr 0.9.0 executable through PATH,
the official `~/.local/bin` installer default, and common package-manager
locations. The resolved path is retained
as a native, connection-scoped capability and reused for runtime listing,
per-session status, controller setup, and any later proof-gated operation. It
is never exposed to JavaScript or ordinary logs, and is re-resolved and
revalidated after transport reconnect. Herdr rows distinguish stopped sessions
from running candidates; a running socket alone does not prove compatibility.
Selection revalidates session identity, protocol 22, schema 1, direct
operations, and stream-local forwarding. This issue does not promise a Herdr
start or create action: stopped rows direct the user to open the session in the
ordinary Herdr client and refresh. meeterm must not silently fall back to tmux
when Herdr discovery or selection fails.

Each backend owns independent bounds and error state, so a missing tmux binary,
missing/incompatible Herdr executable, or one backend's malformed response does
not hide candidates from the other section. Duplicate names remain distinct by
backend. A runtime deleted between list and selection returns a stale-selection
error and refreshes the picker; it never silently creates the missing runtime.

There is one selected runtime actor per authenticated SSH host connection. A
server switch or runtime switch releases the current controller in the actor's
queue, drains/closes the stream as required by the backend, and only then
acquires the next binding. Release, disconnect, or hidden terminal view does not
destroy the remote runtime or its processes. After Ready, transport recovery
keeps the last authoritative native terminal/workspace mounted but revokes its
operation epoch. It may return directly only after host-key/authentication,
backend capability, runtime identity, selected terminal, topology, and
authoritative screen resynchronization are committed. A missing, replaced,
restarted, incompatible, or uncertain target remains fail-closed in that stale
work screen; only an explicit Change action enters a fresh picker.

tmux supplies a server PID/start-time epoch that can prove an unchanged server
for automatic recovery. Herdr 0.9.0 exposes its session name and socket but no
comparable server-instance identity through the selected public interfaces.
That identity is therefore uncertain after a Herdr transport loss. Recovery
authenticates and performs bounded read-only discovery, then waits for an
explicit confirmation inside the retained work screen. Confirmation is scoped
to the freshly validated candidate and does not prove instance continuity.
Only a subsequent compatibility check, original stable-terminal resolution,
ordinary controller acquisition without takeover, and authoritative first full
frame can commit Ready. Failure keeps the stale work visible and never falls
back to tmux.

The saved server profile stores SSH endpoint and authentication metadata.
Legacy backend/runtime fields represent a non-authoritative logical
`lastUsedRuntime` hint. Profile IDs and credentials remain independent, secure
credential identity is not changed by the hint migration, and the hint is
written only after the selected runtime reaches `Ready`.

Before any topology mutation that could affect another tmux session, the same
Rust actor/control queue must check the current linked/shared topology
immediately before execution. Workspace close and terminal close that could
remove the final pane are included. If exact cross-session safety cannot be
proved at execution time, the operation fails closed with a clear error.

## Architectural rule: JavaScript is not the terminal data plane

React Native owns app chrome and product interaction. Rust/native owns terminal transport, terminal state, input composition, and rendering.

### Data that may cross the JS/native boundary

- connect / disconnect commands;
- server configuration identifiers;
- host-key verification requests and responses;
- authentication prompts and responses where appropriate;
- workspace/window snapshots;
- terminal/pane snapshots;
- active workspace and terminal identifiers;
- connection lifecycle state;
- create, rename, close, select, and resize commands;
- low-frequency product events and errors.

### Data that must not use JavaScript as its streaming path

- raw SSH terminal output;
- ANSI/VT escape streams;
- parsed terminal cells;
- scrollback contents as a continuous render source;
- rendering frames;
- cursor blinking;
- high-frequency scroll/render events;
- IME composition updates intended only for the native terminal editor path.

The target path is:

```text
SSH bytes / Herdr terminal.frame
  ↓
Rust selected-backend decoder
  ↓
alacritty_terminal::Term
  ↓
native renderer
  ↓
GPU surface
```

not:

```text
SSH bytes → JS → React Native → WebView
```

## React Native / Expo layer

React Native is responsible for:

- server list and connection screens;
- workspace/window list;
- terminal/pane tabs;
- navigation;
- settings;
- theme and app chrome;
- dialogs and confirmation UI;
- low-frequency connection state;
- product-level error presentation.

Expo Development Builds are expected. Expo Go is not an architectural requirement because meeterm contains custom native code.

The React Native state store must contain product snapshots and identifiers, not live SSH connections or terminal buffers.

## Native package boundary

The mobile app should expose one meeterm native package to React Native. The implementation may internally contain multiple Rust modules/crates, but the mobile process should converge on one native library and one shared runtime/registry.

This prevents duplicate Tokio runtimes and duplicate terminal registries from being accidentally created by independent native packages.

Recommended responsibilities:

```text
react-native-meeterm
├── control bridge
├── native TerminalView
└── meeterm native library
    ├── core/runtime
    ├── ssh
    ├── tmux
    ├── terminal
    └── renderer
```

Do not split crates merely to match this diagram. Start with the smallest maintainable Rust structure and split only when responsibilities genuinely diverge.

### Shared terminal core and platform adapters

The Rust terminal semantics are the cross-platform boundary. The terminal registry, opaque terminal IDs, `alacritty_terminal::Term`, byte-oriented input contract, resize rules, and native-only terminal snapshot format should have one implementation and one source of truth.

Each mobile platform then supplies a thin adapter:

- Android owns the Expo/Kotlin view, JNI conversion, Android text-input protocol, font metrics, and its native surface/renderer integration.
- iOS owns the Expo/Swift or Objective-C view, the Apple bridge to the Rust library, UIKit text-input protocol, font metrics, and its native surface/renderer integration.

The adapters may use different graphics and text APIs, but they must preserve the same terminal ID lifetime, snapshot semantics, committed-input behavior, and resize contract. Do not create a second terminal registry or move terminal bytes/cells/render frames through JavaScript to make the adapters look identical.

## React Native ↔ Rust control bridge

The control plane should use generated typed bindings rather than hand-written ad-hoc JSON messaging. UniFFI with a React Native binding generator is the preferred starting direction for asynchronous commands and low-frequency events.

Example conceptual API:

```text
connectHost(profileId)
listRuntimes(connectionId)
selectRuntime(connectionId, candidateId)
createRuntime(connectionId, backend, name)  # tmux only in this issue
disconnectHost(connectionId)
workspaceSnapshot(runtimeId)
createWorkspace(runtimeId, name)
renameWorkspace(workspaceId, name)
closeWorkspace(workspaceId)
createGroup(workspaceId, name)
renameGroup(groupId, name)
closeGroup(groupId)
selectGroup(groupId)
createTerminal(groupId)
closeTerminal(terminalId)
selectTerminal(terminalId)
setTerminalVisible(connectionId, visible)
respondToHostKeyPrompt(...)
```

`connectHost` ends at authenticated host/discovery state; `selectRuntime` is the
explicit binding transition to `Ready`. The exact public API should remain
small. Do not expose low-level `russh`, resolved Herdr executable paths, or
`alacritty_terminal` objects to JavaScript.

## Native TerminalView

The terminal surface is a native React Native view backed by the Rust terminal registry and native renderer.

A view should bind to stable native identifiers rather than owning the terminal session itself. Unmounting a React Native view must not automatically destroy the corresponding remote pane or native terminal state.

Conceptually:

```tsx
<TerminalView terminalId={terminalId} />
```

The native view is responsible for:

- surface lifecycle;
- font metrics;
- viewport dimensions;
- touch scrolling and selection where applicable;
- keyboard focus;
- platform IME integration;
- scheduling render work;
- forwarding committed terminal input to the Rust core.

A HybridView-style native component is the preferred direction for this view boundary.

## SSH layer

`russh` is the SSH implementation used by the native core.

The SSH layer owns:

- TCP/SSH connection lifecycle;
- host-key verification;
- authentication;
- keepalive policy;
- reconnect coordination;
- backend control channels: tmux Control Mode or Herdr direct stream-local
  public requests;
- transport errors.

Security requirements:

- never silently accept a changed known host key;
- use explicit host-key verification / TOFU behavior for first connection;
- keep secrets out of ordinary React Native persistence. Optional saved credentials
  use Android Keystore-backed AES-GCM or iOS Keychain; saved credentials are loaded
  directly by the native connection path. Rust retains the selected credential
  in process memory for reconnect;
- do not log private keys, passwords, passphrases, or raw authentication material.

SSH is transport, not durable application state.

### Herdr direct control

When `Backend::Herdr` is selected, the Rust actor verifies the existing
`herdr --session <runtime>` status contract and opens the public stream-local
socket through SSH. It does not allocate an outer PTY or pass `--takeover`.
Each request uses one channel and the actor serializes requests; subscribe
acknowledgement, repeated snapshots until the pane set agrees, stable events,
and resync remain in the actor; unstable snapshots are not committed to the
low-frequency model.
Terminal frames are ANSI bytes from Herdr, not a raw PTY stream. The actor maps
stable `terminal_id` to the local native terminal and treats a changed
`pane_id` after a move as a metadata update.

Herdr input is semantic at the public API boundary: committed text uses
`pane.send_text`, special keys use `pane.send_keys`, and paste uses
`pane.send_input` with an explicit complete bracketed-paste envelope where
needed. Before input, `pane.scroll` is sent with `offset_from_bottom: 0`.
The 0.9.0 parser has no names for Home/End/Insert/Delete/PageUp/PageDown, so
the native adapter supplies fixed xterm normal-mode bytes. A controller release
closes/EOFs the stream before a later stable-ID reacquire; the remote process
continues to run.

### Issue #3: historical direct SSH shell slice

Before adding tmux, the SSH slice opens one interactive `xterm-256color` PTY
and shell for an existing Rust terminal ID. `russh` feeds channel data directly
into the shared `Term`; native committed input and terminal-generated replies
return through the same channel. Native viewport changes update the remote PTY.
The fixed local demo remains available before connecting, and its input loopback
is disabled when that terminal enters the SSH path.

The initial authentication path is an OpenSSH private key with an optional
passphrase. The current form also supports the SSH `password` authentication
method. Password authentication does not add keyboard-interactive prompts or
MFA. Credentials are supplied transiently and are never saved to disk; Rust
retains the selected credential in process memory for explicit reconnect.
Host-key trust is separate: Rust persists explicitly accepted host identities in
an app-private file supplied by the platform adapter, and a changed identity
fails closed. This slice has explicit connect/disconnect ownership, with no
automatic reconnect or tmux session behavior.

The small control surface extends the existing native package and C/JNI bridge
with typed commands and connection-state fields. Generated bindings remain the
preferred direction as the control surface grows. This step does not introduce
another native library, Tokio runtime, or platform-specific SSH implementation.
See [`SSH.md`](SSH.md) for the real OpenSSH fixture, validation, and limitations.

### Issue #6: durable session loop

The production connection now starts tmux Control Mode over the authenticated
SSH channel after an ordinary session has been explicitly selected. The direct
shell slice above describes the earlier validation boundary. The selected
session remains on the user's ordinary tmux server; windows and panes are
discovered from that session rather than invented locally. A legacy `meeterm`
value may seed the picker as a last-used suggestion, but it is not an automatic
attach target.

The shared Rust core owns the connection profile, decoder, topology snapshots,
pane-to-terminal mapping, and reconnect/resynchronization. JavaScript receives
only window/pane identities, labels, selection, and connection state. The same
native view binds a `native:<id>` borrowed Rust terminal handle when the selected
pane changes. View unmount does not disconnect or destroy the remote pane.

Rust owns bounded automatic retry after transient transport loss and foreground
return. The public recovery control plane distinguishes Retry of the same
retained intent, one-use Herdr confirmation, and explicit Change to a fresh
runtime picker. A connection generation scopes the actor; a separate monotonic
operation epoch invalidates delayed key, paste, resize, terminal-generated
reply, and topology-mutation callbacks. Cached native output can remain visible
while that gate is closed, but only a complete authoritative resynchronization
sets Ready and reopens input. Explicit disconnect/change cancels retry and
confirmation; host-key/authentication failures require user action. Rust
retains the selected parsed key or a `Zeroizing` password buffer in process
memory. The form clears credential inputs after submission. Optional
platform-secure credential storage supports reopening a saved server after
process death without passing its secret back to JavaScript. Approved host
identities remain pinned during every reconnect. See [DAILY_USE.md](DAILY_USE.md)
for profile, preference, native selection and scrollback boundaries.

See [`SSH.md`](SSH.md) for the real fixture, end-to-end tests, and the current
reconstruction and handoff boundaries.

## tmux model

meeterm uses the user's ordinary tmux server and the session selected in the
runtime picker. `meeterm` is the suggested name for explicit creation and a
legacy last-used hint, not a canonical or required session name.

```text
selected session: <session-name>
├── window @1 = Workspace
│   ├── pane %1 = Terminal
│   └── pane %2 = Terminal
└── window @2 = Workspace
    ├── pane %3 = Terminal
    └── pane %4 = Terminal
```

Do not create a separate tmux server/socket with `tmux -L meeterm` for the core product. A PC must be able to continue with the ordinary command for the selected session:

```bash
tmux attach -t <selected-session>
```

The tmux mapping above is the current implementation contract. The common
TerminalGroup is virtual for tmux and is not a new remote object. The selected
session identity is passed to the control actor; normal selection does not use
an attach-or-create command.

## tmux Control Mode

The mobile client uses tmux Control Mode over an SSH exec channel without an
outer PTY, targeting the exact session selected by the picker. Explicit tmux
creation uses a separate detached `new-session` operation and verifies its
returned identity before the Control Mode actor binds it. The normal selection
path never creates a session as a side effect. The original `-CC` direction
assumed a terminal-backed channel. On tmux 3.4, running `-CC` with stdin/stdout
pipes fails with `tcgetattr failed: Inappropriate ioctl for device`; the same
bounded test using `-C` emits `%begin` and detaches successfully. There is no
outer terminal echo or line discipline to disable on this channel. `-C`
preserves the structured Control Mode boundary; pane contents remain bytes.
See the [tmux Control Mode documentation](https://github.com/tmux/tmux/wiki/Control-Mode#entering-control-mode)
for the terminal-attribute difference between the two flags.

The shell discovery/preflight channel and the later Control Mode attach channel
are separate, so the actor closes that replacement race before synchronization:
after the attach startup block succeeds and before `Ready` or input acceptance,
it issues a read-only `display-message` format query on the same Control Mode
stream for the exact `$N` target. A bounded byte parser reads
`session_id|pid|start_time` and compares all three values with the selected
`SessionIdentity`. During fresh selection, a mismatch, malformed reply, command
error, or uncertain result fails as `TmuxRuntimeMissing` and returns the actor
to the picker. During retained-work recovery, the same failure instead leaves
the cached terminal visible and fail-closed; only the explicit Change action
starts a new picker flow.

Control Mode provides structured notifications and identifies pane output by pane ID. The Rust core should parse Control Mode as a byte-oriented protocol and route each pane's output to its own terminal state.

Conceptually:

```text
SSH channel
  ↓
tmux -C ...
  ↓
Control Mode decoder
  ├── %output %1 → terminal registry %1
  ├── %output %2 → terminal registry %2
  └── lifecycle/window/pane events → tmux model
```

Control Mode escaping must be decoded into bytes before terminal parsing. Do not assume pane output is a UTF-8 application string.

## Terminal registry

The Rust core is the source of truth for live terminal objects.

Conceptually:

```text
ConnectionRuntime (SSH + backend + runtime)
├── SSH connection
├── selected backend controller
└── TerminalRegistry
    ├── opaque remote identity → Term
    ├── tmux pane/window aliases → Term
    └── Herdr stable terminal_id / mutable pane_id → Term
```

Each remote terminal maps to its own `alacritty_terminal::Term` state. Local
IDs are scoped by SSH connection, backend, and runtime. Switching React Native
tabs changes which native Term is displayed; it must not recreate terminal
state or reconnect SSH. Herdr move events update the pane alias while retaining
the stable terminal ID and native registry entry.

While the app process remains alive, hidden panes should retain their terminal state and scrollback.

## Mobile pane presentation and backend resize

A phone presents panes in the selected group as tabs. Tmux remains a
multi-pane window; Herdr remains its workspace/tab/pane hierarchy.

The selected mobile pane should receive a phone-appropriate remote size. The
tmux backend uses its zoom/client-size operations; the Herdr backend sends its
direct resize operation. Purely stretching a locally rendered half-width pane
is not sufficient because applications such as nvim and Codex react to actual
terminal dimensions.

Requirements:

- selecting a terminal tab selects the corresponding remote terminal;
- the tmux mobile-selected pane is expanded using tmux zoom behavior;
- the Herdr selected pane is resized through its direct controller;
- switching tabs preserves the remote layout and each native Term;
- graceful mobile detach releases the controller and restores the normal layout;
- reconnect/desktop handoff recovers from mobile termination during either
  backend's phone-sized view.

The exact recovery mechanism must be tested against real tmux behavior before being encoded as a permanent hook/configuration. Do not install global tmux hooks without a demonstrated need and a narrowly scoped design.

## Resize model

The native view computes terminal columns and rows from:

- pixel dimensions;
- font metrics;
- safe-area / terminal chrome constraints.

The selected Rust controller propagates the resulting logical terminal size to
tmux through Control Mode or to Herdr through its direct resize operation.

Rotation, fold/unfold, keyboard appearance, and font-size changes must trigger deterministic terminal resize behavior.

## Input and IME

Japanese/CJK IME handling is native-terminal responsibility, not a JavaScript `TextInput` streaming problem.

Platform integration should use the native text-input protocols appropriate to each OS. Composition text should remain local until committed; committed text and terminal key events then flow directly into the Rust input path.

Conceptually:

```text
OS IME
  ↓
native TerminalView
  ├── composition / preedit stays local
  └── committed input
          ↓
      Rust input encoder
          ↓
      selected backend target terminal
```

Special keys, modifiers, bracketed paste, Unicode text, and terminal-generated responses must be modeled explicitly. Avoid shell-string concatenation for user input.

For Herdr, the adapter sends committed text, special keys, and paste through
`pane.send_text`, `pane.send_keys`, and `pane.send_input`. It sends
`pane.scroll` with offset zero before input so a stale remote viewport does not
consume the operation. The adapter uses fixed xterm normal-mode bytes for
Home/End/Insert/Delete/PageUp/PageDown because Herdr 0.9.0 does not expose
those names in its parser. A hidden view releases the controller through the
shared visibility operation; it does not destroy the native terminal or remote
process.

## Terminal core

`alacritty_terminal` is the preferred terminal state engine. It owns VT parsing and terminal state such as grid, cursor, modes, selection, scrollback, and renderable content.

It is not itself the complete mobile renderer.

The integration should retain raw byte semantics from tmux output into the terminal parser.

## Native renderer

The target renderer is native GPU rendering derived from the proven Alacritty mobile rendering approach rather than a WebView/xterm.js data path.

Target direction:

```text
Android: native view/surface → EGL → OpenGL ES

iOS: native view/layer → ANGLE/OpenGL ES compatibility → Metal
```

The iOS line is a current compatibility hypothesis, not a settled implementation choice. Direct Metal versus ANGLE/OpenGL ES compatibility remains unresolved until a small native prototype supplies evidence about snapshot throughput, text/CJK coverage, IME and lifecycle behavior, build/dependency cost, and maintainability. Record that tradeoff when choosing the backend; do not silently replace the native GPU path with a WebView or JavaScript renderer.

The initial iOS vertical slice is that prototype and deliberately uses a direct `MTKView`/Metal backend. CoreText rasterizes the native snapshot into a full-frame texture, and Metal submits it on demand. This proves the shared Rust/C ABI, UIKit input, resize, and native GPU-surface boundaries without adding ANGLE before there is reusable OpenGL renderer code. It is not yet a final performance decision: full-frame CPU raster/upload, glyph-atlas work, damage tracking, and the possible value of a shared ANGLE path must be measured before the production renderer is selected.

A proven open-source implementation such as Fressh may be used as a reference or fork basis for the difficult renderer/build glue, subject to license attribution and code review. meeterm should own the architecture boundary and be capable of maintaining the renderer it ships.

Rendering should be demand-driven when practical. Terminal damage, cursor changes, scrolling, resize, and animations should schedule frames; an idle terminal should not require a permanent high-frequency render loop.

## Fonts and CJK

CJK support must be designed into the renderer early.

Requirements include:

- bundled or otherwise deterministic monospace font behavior;
- Japanese glyph fallback;
- correct wide-cell measurement;
- combining marks;
- emoji behavior sufficient for terminal use;
- consistent glyph metrics between terminal grid calculation and rendering.

A renderer that only works with a Latin monospace font is not sufficient for the first complete terminal milestone.

## Connection lifecycle

The Rust core should model connection state explicitly, for example:

```text
Disconnected
→ Connecting
→ HostKeyPending
→ Authenticating
→ SSHConnected / DiscoveringRuntimes
→ RuntimeSelection
→ AttachingSelectedBackend
→ Synchronizing
→ Ready
→ Reconnecting
→ AwaitingRecoveryConfirmation / Resynchronizing / RecoveryStopped
```

React Native observes a low-frequency snapshot of this state; React Native must not implement the reconnect state machine with timers and effects.

## Backgrounding and process death

The durable state is the selected remote runtime, not the SSH socket. This is
the selected ordinary tmux session for tmux and the selected running
`default`/named session for Herdr. `meeterm` is only a suggested new-session
name and legacy hint.

Backgrounding alone does not tear down a healthy controller; native input is
closed while the view is inactive and the same live connection may continue on
foreground return. If transport is actually lost, reconnect waits for the app
to return to the foreground and then resynchronizes. During same-process
recovery the last authoritative native view remains mounted as cached read-only
output. A retained terminal is not live until backend identity/topology and an
authoritative selected-terminal frame have been committed and its new operation
epoch is ready.

If the mobile process is killed, in-memory `Term` scrollback disappears while
the selected remote runtime continues running. Reconstructing a useful
terminal view after reconnect requires backend snapshot/resynchronization.
Alternate-screen applications such as nvim and full-screen TUIs must be
explicitly tested because naive scrollback reconstruction may not reproduce
their current state accurately. Herdr controller streams are closed/EOFed on
release and reacquired by stable `terminal_id`.

This is an early technical-risk item and must be validated before broad feature work.

## Desktop handoff

Desktop handoff is a first-class requirement; simultaneous interactive multi-client use is not.

A normal PC attach must be able to use the selected backend's ordinary client.
For tmux, use the selected session:

```bash
tmux attach -t <selected-session>
```

and see the same windows/panes in their ordinary layout. For Herdr, the normal
Herdr client opens the selected session. `meeterm` may be used in the command
when it is the selected session. meeterm does not require a PC companion
application.

The mobile client must avoid leaving the session in a phone-specific layout state after graceful detach and should have a recovery strategy for ungraceful termination.

Do not add meeterm-specific software to the PC path merely to make handoff work.

## Persistence

Remote workspace state comes from the selected backend runtime.

Local persistence is for client concerns only, such as:

- saved server profiles, including the non-authoritative legacy backend/runtime
  last-used hint;
- trusted host-key fingerprints;
- user preferences;
- non-secret UI settings.

Secrets belong in platform secure storage. Credential identity remains scoped to
SSH/auth/profile; backend and runtime are connection selectors rather than
credential AAD. The hint is written only after `Ready`. Do not introduce a
local database as a shadow source of truth
for windows/panes unless a concrete later requirement demands it.

## Current native milestone and verification boundary

The dual-platform native-terminal foundation and backend control paths keep the
terminal data plane native. Continue to verify:

1. Shared Rust `alacritty_terminal::Term` semantics, fixed ANSI/VT fixtures, input encoding, snapshots, and deterministic resize behavior are covered below the renderer.
2. Expo Development Builds mount one native `TerminalView` contract on both Android and iOS, with each view binding a stable terminal ID.
3. The Android and iOS adapters each obtain and render a real Rust-owned terminal snapshot on a native GPU surface. The iOS graphics backend is selected from the evidence described above.
4. Latin text, Japanese/CJK text, wide characters, combining marks, and representative emoji have an explicit validation path on each platform; known font limitations remain documented.
5. Each platform's native text-input protocol keeps composition local and commits UTF-8 exactly once into the Rust input path.
6. Portrait/landscape, keyboard, and relevant window-size changes produce deterministic terminal resize behavior where the platform permits the check.
7. GitHub-hosted Android emulator and iOS Simulator jobs provide machine gates for build, install, launch, native readiness, first native frame, and no crash. Each job always uploads an observability bundle containing the available screenshot/log evidence or explicit capture-unavailable diagnostics; screenshots are not pixel-diff assertions.

8. The ignored real Herdr integration test runs through an isolated russh
   endpoint with an official Herdr 0.9.0 binary. Its OpenSSH/tmux integration
   remains a separate check; one does not substitute for the other.
9. Runtime-picker changes add bounded no-side-effect discovery tests, tmux
   list/create/select and identity-race tests, Herdr executable PATH/list/status
   and running-selection tests, independent backend-failure tests, profile
   migration and switch/release tests, reconnect identity tests, and fail-closed
   linked/shared tmux topology-mutation tests.
10. Android full and iOS `standard` plus the short `ssh` suite cover the
    applicable mobile connection lifecycle. The iOS `standard` source-level
    manifest has 25 screens: the previous 18 plus `recovery-progress`,
    `recovery-exhausted`, `recovery-mismatch`, `herdr-recovery-confirm`,
    `layout-restore-unconfirmed`, `runtime-layout-restore-unconfirmed`, and
    `connection-error`; its
    `herdr-connection` route is the picker with the Herdr `default` candidate's
    non-authoritative `Last used` hint. Android's observational `SCREEN_NAMES`
    has 31 routes: the previous 25 plus those four recovery routes and the two
    layout-restore warning fixtures. These counts define source scope only; they
    do not claim remote CI or visual
    review. Both platform screenshots must be downloaded and actually viewed
    before visual success is reported.
11. Retained-work recovery changes additionally verify native Term/selection
    preservation, operation-epoch invalidation, no queued input/mutation replay,
    strict tmux runtime/pane identity, Herdr in-work confirmation before normal
    lease acquisition, authoritative selected-terminal resynchronization before
    Ready, and fail-closed retry exhaustion without an automatic picker.

A simulator/emulator smoke result does not replace physical-device GPU, font,
or IME validation. The [live native Herdr test](evidence/issue-17-herdr-native.md)
has passed locally. The [mobile acceptance record](evidence/issue-17-herdr-mobile.md)
records CI results and actual screen review for each source revision.

## CI and mobile evidence

The mobile CI contract is intentionally split between machine gates and human inspection. A job passes its machine portion only when the generated CNG project builds, the app installs, launches, reports the expected native module as ready, reports a first native terminal frame, and does not crash. The job always uploads its evidence bundle; a failure before capture is represented by an unavailable diagnostic, not a fake screenshot.

On a standard hosted macOS runner, Metal availability is recorded rather than assumed. If Metal is unavailable, the iOS Simulator may render the same Rust snapshot and CoreText raster through an explicitly marked native CoreGraphics fallback. That validates the non-JavaScript terminal path and yields reviewable CI evidence, but it does not satisfy the outstanding iOS Metal execution check. A physical device or GPU-capable runner must supply that evidence later.

There is no pixel-difference gate at this stage. For a native UI change, visual success is reported only after Codex downloads and actually views both the Android emulator screenshot and the iOS Simulator screenshot. Artifact existence, screenshot dimensions, or a successful process exit is not visual review. The general Rust workflow downloads the official Herdr 0.9.0 binary only into `RUNNER_TEMP`, verifies SHA-256 `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`, and runs the ignored russh integration; record the workflow result with its source revision in the acceptance evidence. See [`docs/CI_MOBILE.md`](CI_MOBILE.md) for the runner, signing, CNG, and staged-job guide.

iOS Simulator builds are unsigned simulator validation and must not require distribution certificates, provisioning profiles, or Apple secrets. Physical iOS devices and TestFlight are later signed workflows with separate credentials and acceptance criteria. Simulator-only app Keychain entitlements are embedded in Mach-O XML/DER sections while signing remains disabled. Storage tests import the production pod in an app-hosted unit-test target; the separate UI test runner is not the Keychain test host. See [DAILY_USE.md](DAILY_USE.md) for the focused reproduction and validation scope.

## Architectural non-goals

Do not add these as shortcuts without an explicit architecture decision:

- meeterm gateway/daemon;
- HTTP/WebSocket backend for terminal transport;
- WebView/xterm.js terminal data plane;
- terminal buffers stored in JavaScript;
- proprietary replacement for tmux session state;
- separate meeterm tmux server/socket;
- PC companion application required for normal handoff.
