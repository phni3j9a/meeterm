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
          ├── tmux session: meeterm
          │   ├── window = Workspace
          │   └── pane   = Terminal
          └── existing Herdr 0.9.0 session/socket
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
`workspace.close` with `close_group: false`, so final-pane/tab closure and
workspace cascade are distinct protocol operations. Backend/runtime selection
is explicit; missing legacy profile fields default to the existing tmux path,
and a missing Herdr capability is an explicit error rather than a silent tmux
fallback. Remote identifiers remain opaque and scoped by connection, backend,
and runtime. Local native terminal IDs remain separate because a Herdr pane ID
may change when it moves.

The backend actor may differ, but terminal bytes, ANSI/VT parsing, cells,
scrollback, render frames, and IME composition remain native. Only hierarchy,
selection, lifecycle, group operations, visibility, metadata, and errors cross
the low-frequency control bridge. Herdr connects over ordinary SSH to its
selected existing runtime through direct stream-local public operations; this
does not add a meeterm gateway, daemon, HTTP API, or WebSocket terminal
transport.

The fixed Herdr compatibility target is 0.9.0 / protocol 22 / schema 1. The
live Rust integration has passed; CI and mobile evidence remain pending, so
Issue #17 is not marked accepted. Input adaptation uses Herdr's existing `send_text`,
`send_keys`, and `send_input` operations; a modified Herdr or upstream API
addition is not required. See [`HERDR.md`](HERDR.md) and the
[`Issue #17 evidence record`](evidence/issue-17-herdr-feasibility.md).

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
connectServer(...)
disconnectServer(serverId)
workspaceSnapshot(serverId)
createWorkspace(serverId, name)
renameWorkspace(workspaceId, name)
closeWorkspace(workspaceId)
createGroup(workspaceId, name)
renameGroup(groupId, name)
closeGroup(groupId)
selectGroup(groupId)
createTerminal(groupId)
closeTerminal(terminalId)
selectTerminal(terminalId)
setTerminalVisible(serverId, visible)
respondToHostKeyPrompt(...)
```

The exact public API should remain small. Do not expose low-level `russh` or `alacritty_terminal` objects to JavaScript.

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
SSH channel. The direct shell slice above describes the earlier validation
boundary. The managed session remains `meeterm` on the ordinary tmux server;
windows and panes are discovered from tmux rather than invented locally.

The shared Rust core owns the connection profile, decoder, topology snapshots,
pane-to-terminal mapping, and reconnect/resynchronization. JavaScript receives
only window/pane identities, labels, selection, and connection state. The same
native view binds a `native:<id>` borrowed Rust terminal handle when the selected
pane changes. View unmount does not disconnect or destroy the remote pane.

`Reconnect` is an explicit native control command, supplemented by Rust-owned
bounded automatic retry after transient transport loss and foreground return.
Explicit disconnect cancels retry; host-key/authentication failures require user
action. Rust retains the selected parsed key or a `Zeroizing` password buffer
in process memory. The form clears credential inputs after submission. Optional
platform-secure credential storage supports reopening a saved server after
process death without passing its secret back to JavaScript. Approved host
identities remain pinned during every reconnect. See [DAILY_USE.md](DAILY_USE.md)
for profile, preference, native selection and scrollback boundaries.

See [`SSH.md`](SSH.md) for the real fixture, end-to-end tests, and the current
reconstruction and handoff boundaries.

## tmux model

meeterm uses the user's ordinary tmux server and the canonical session name `meeterm`.

```text
session: meeterm
├── window @1 = Workspace
│   ├── pane %1 = Terminal
│   └── pane %2 = Terminal
└── window @2 = Workspace
    ├── pane %3 = Terminal
    └── pane %4 = Terminal
```

Do not create a separate tmux server/socket with `tmux -L meeterm` for the core product. A PC must be able to continue with the ordinary command:

```bash
tmux attach -t meeterm
```

The tmux mapping above is the current implementation contract. The common
TerminalGroup is virtual for tmux and is not a new remote object.

## tmux Control Mode

The mobile client uses tmux Control Mode over an SSH exec channel without an
outer PTY: `tmux -C -u new-session -A -s meeterm`. The original `-CC` direction
assumed a terminal-backed channel. On tmux 3.4, running `-CC` with stdin/stdout
pipes fails with `tcgetattr failed: Inappropriate ioctl for device`; the same
bounded test using `-C` emits `%begin` and detaches successfully. There is no
outer terminal echo or line discipline to disable on this channel. `-C`
preserves the structured Control Mode boundary; pane contents remain bytes.
See the [tmux Control Mode documentation](https://github.com/tmux/tmux/wiki/Control-Mode#entering-control-mode)
for the terminal-attribute difference between the two flags.

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
→ SSHConnected
→ AttachingSelectedBackend
→ Synchronizing
→ Ready
→ Reconnecting
```

React Native observes a low-frequency snapshot of this state; React Native must not implement the reconnect state machine with timers and effects.

## Backgrounding and process death

The durable state is the selected remote runtime, not the SSH socket. This is
tmux session `meeterm` for the default backend and Herdr `default`/named
session for Herdr.

When the app backgrounds or loses transport, meeterm may reconnect and resynchronize rather than attempt to keep a fragile mobile connection alive indefinitely.

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
For tmux:

```bash
tmux attach -t meeterm
```

and see the same windows/panes in their ordinary layout. For Herdr, the normal
Herdr client opens the selected session; meeterm does not require a PC
companion application.

The mobile client must avoid leaving the session in a phone-specific layout state after graceful detach and should have a recovery strategy for ungraceful termination.

Do not add meeterm-specific software to the PC path merely to make handoff work.

## Persistence

Remote workspace state comes from the selected backend runtime.

Local persistence is for client concerns only, such as:

- saved server profiles, including backend and optional runtime;
- trusted host-key fingerprints;
- user preferences;
- non-secret UI settings.

Secrets belong in platform secure storage. Credential identity remains scoped to
SSH/auth/profile; backend and runtime are connection selectors rather than
credential AAD. Do not introduce a local database as a shadow source of truth
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
9. iOS `standard` includes 14 direct screen fixtures (four Herdr routes) and
   Android captures the same four Herdr routes as observational evidence.

A simulator/emulator smoke result does not replace physical-device GPU, font,
or IME validation. The [live native Herdr test](evidence/issue-17-herdr-native.md)
has passed locally. CI and mobile visual evidence remain pending.

## CI and mobile evidence

The mobile CI contract is intentionally split between machine gates and human inspection. A job passes its machine portion only when the generated CNG project builds, the app installs, launches, reports the expected native module as ready, reports a first native terminal frame, and does not crash. The job always uploads its evidence bundle; a failure before capture is represented by an unavailable diagnostic, not a fake screenshot.

On a standard hosted macOS runner, Metal availability is recorded rather than assumed. If Metal is unavailable, the iOS Simulator may render the same Rust snapshot and CoreText raster through an explicitly marked native CoreGraphics fallback. That validates the non-JavaScript terminal path and yields reviewable CI evidence, but it does not satisfy the outstanding iOS Metal execution check. A physical device or GPU-capable runner must supply that evidence later.

There is no pixel-difference gate at this stage. For a native UI change, visual success is reported only after Codex downloads and actually views both the Android emulator screenshot and the iOS Simulator screenshot. Artifact existence, screenshot dimensions, or a successful process exit is not visual review. The general Rust workflow downloads the official Herdr 0.9.0 binary only into `RUNNER_TEMP`, verifies SHA-256 `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f`, and runs the ignored russh integration; no result is recorded here until that workflow runs. See [`docs/CI_MOBILE.md`](CI_MOBILE.md) for the runner, signing, CNG, and staged-job guide.

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
