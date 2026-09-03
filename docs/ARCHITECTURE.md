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
        ├── tmux Control Mode
        ├── terminal registry
        ├── alacritty_terminal
        └── native GPU renderer
               │
               │ SSH
═══════════════╪════════════════════
               ▼
          OpenSSH server
               │
               ▼
       tmux session: meeterm
       ├── window = Workspace
       └── pane   = Terminal
```

There is no meeterm server-side component in the core architecture.

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
SSH bytes
  ↓
Rust tmux decoder
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
listWorkspaces(serverId)
createWorkspace(serverId, name)
renameWorkspace(workspaceId, name)
closeWorkspace(workspaceId)
createTerminal(workspaceId)
closeTerminal(terminalId)
selectTerminal(terminalId)
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

`russh` is the preferred SSH implementation.

The SSH layer owns:

- TCP/SSH connection lifecycle;
- host-key verification;
- authentication;
- keepalive policy;
- reconnect coordination;
- SSH channels used for tmux control;
- transport errors.

Security requirements:

- never silently accept a changed known host key;
- use explicit host-key verification / TOFU behavior for first connection;
- keep secrets in platform secure storage, not ordinary React Native persistence;
- do not log private keys, passwords, passphrases, or raw authentication material.

SSH is transport, not durable application state.

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

## tmux Control Mode

The mobile client should use tmux Control Mode (`-CC`) rather than scrape a normal interactive tmux client screen.

Control Mode provides structured notifications and identifies pane output by pane ID. The Rust core should parse Control Mode as a byte-oriented protocol and route each pane's output to its own terminal state.

Conceptually:

```text
SSH channel
  ↓
tmux -CC ...
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
ServerRuntime
├── SSH connection
├── tmux controller
└── TerminalRegistry
    ├── pane %1 → Term
    ├── pane %2 → Term
    └── pane %3 → Term
```

Each tmux pane maps to its own `alacritty_terminal::Term` state. Switching React Native tabs changes which native Term is displayed; it must not recreate terminal state or reconnect SSH.

While the app process remains alive, hidden panes should retain their terminal state and scrollback.

## Mobile pane presentation and tmux zoom

A phone presents panes in the active window as tabs, but the remote tmux model remains a multi-pane window.

The selected mobile pane should use tmux zoom semantics when necessary so the remote TUI receives a phone-appropriate PTY size. Purely stretching a locally rendered half-width pane is not sufficient because applications such as nvim and Codex react to the actual terminal dimensions.

Requirements:

- selecting a terminal tab selects the corresponding pane;
- the mobile-selected pane is expanded using tmux zoom behavior;
- switching tabs should preserve the window's underlying multi-pane layout;
- graceful mobile detach should restore the normal layout;
- reconnect/desktop handoff logic must recover from mobile termination that occurs while a pane is zoomed.

The exact recovery mechanism must be tested against real tmux behavior before being encoded as a permanent hook/configuration. Do not install global tmux hooks without a demonstrated need and a narrowly scoped design.

## Resize model

The native view computes terminal columns and rows from:

- pixel dimensions;
- font metrics;
- safe-area / terminal chrome constraints.

The Rust tmux controller propagates the resulting logical terminal size to tmux through the Control Mode client-size mechanism and/or pane/window operations required by the final verified design.

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
      tmux target pane
```

Special keys, modifiers, bracketed paste, Unicode text, and terminal-generated responses must be modeled explicitly. Avoid shell-string concatenation for user input.

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
→ AttachingTmux
→ Synchronizing
→ Ready
→ Reconnecting
```

React Native observes a low-frequency snapshot of this state; React Native must not implement the reconnect state machine with timers and effects.

## Backgrounding and process death

The durable state is remote tmux, not the SSH socket.

When the app backgrounds or loses transport, meeterm may reconnect and resynchronize rather than attempt to keep a fragile mobile connection alive indefinitely.

If the mobile process is killed, in-memory `Term` scrollback disappears while tmux continues running. Reconstructing a useful terminal view after reconnect may require tmux capture/resynchronization. Alternate-screen applications such as nvim and full-screen TUIs must be explicitly tested because naive scrollback reconstruction may not reproduce their current state accurately.

This is an early technical-risk item and must be validated before broad feature work.

## Desktop handoff

Desktop handoff is a first-class requirement; simultaneous interactive multi-client use is not.

A normal PC attach must be able to use:

```bash
tmux attach -t meeterm
```

and see the same windows/panes in their ordinary layout.

The mobile client must avoid leaving the session in a phone-specific layout state after graceful detach and should have a recovery strategy for ungraceful termination.

Do not add meeterm-specific software to the PC path merely to make handoff work.

## Persistence

Remote workspace state comes from tmux.

Local persistence is for client concerns only, such as:

- saved server profiles;
- trusted host-key fingerprints;
- user preferences;
- non-secret UI settings.

Secrets belong in platform secure storage. Do not introduce a local database as a shadow source of truth for windows/panes unless a concrete later requirement demands it.

## Initial technical milestone

Before broad product UI, prove a dual-platform native-terminal foundation while keeping the terminal data plane native:

1. Shared Rust `alacritty_terminal::Term` semantics, fixed ANSI/VT fixtures, input encoding, snapshots, and deterministic resize behavior are covered below the renderer.
2. Expo Development Builds mount one native `TerminalView` contract on both Android and iOS, with each view binding a stable terminal ID.
3. The Android and iOS adapters each obtain and render a real Rust-owned terminal snapshot on a native GPU surface. The iOS graphics backend is selected from the evidence described above.
4. Latin text, Japanese/CJK text, wide characters, combining marks, and representative emoji have an explicit validation path on each platform; known font limitations remain documented.
5. Each platform's native text-input protocol keeps composition local and commits UTF-8 exactly once into the Rust input path.
6. Portrait/landscape, keyboard, and relevant window-size changes produce deterministic terminal resize behavior where the platform permits the check.
7. GitHub-hosted Android emulator and iOS Simulator jobs provide machine gates for build, install, launch, native readiness, first native frame, and no crash. Each job always uploads an observability bundle containing the available screenshot/log evidence or explicit capture-unavailable diagnostics; screenshots are not pixel-diff assertions.

SSH and tmux should be added only after both native adapters demonstrate that the shared renderer/input foundation is viable. A simulator/emulator smoke result does not replace physical-device GPU, font, or IME validation.

## CI and mobile evidence

The mobile CI contract is intentionally split between machine gates and human inspection. A job passes its machine portion only when the generated CNG project builds, the app installs, launches, reports the expected native module as ready, reports a first native terminal frame, and does not crash. The job always uploads its evidence bundle; a failure before capture is represented by an unavailable diagnostic, not a fake screenshot.

On a standard hosted macOS runner, Metal availability is recorded rather than assumed. If Metal is unavailable, the iOS Simulator may render the same Rust snapshot and CoreText raster through an explicitly marked native CoreGraphics fallback. That validates the non-JavaScript terminal path and yields reviewable CI evidence, but it does not satisfy the outstanding iOS Metal execution check. A physical device or GPU-capable runner must supply that evidence later.

There is no pixel-difference gate at this stage. For a native UI change, visual success is reported only after Codex downloads and actually views both the Android emulator screenshot and the iOS Simulator screenshot. Artifact existence, screenshot dimensions, or a successful process exit is not visual review. See [`docs/CI_MOBILE.md`](CI_MOBILE.md) for the runner, signing, CNG, and staged-job guide.

iOS Simulator builds are unsigned simulator validation and must not require distribution certificates, provisioning profiles, or Apple secrets. Physical iOS devices and TestFlight are later signed workflows with separate credentials and acceptance criteria.

## Architectural non-goals

Do not add these as shortcuts without an explicit architecture decision:

- meeterm gateway/daemon;
- HTTP/WebSocket backend for terminal transport;
- WebView/xterm.js terminal data plane;
- terminal buffers stored in JavaScript;
- proprietary replacement for tmux session state;
- separate meeterm tmux server/socket;
- PC companion application required for normal handoff.
