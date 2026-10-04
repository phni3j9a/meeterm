# Architecture

[PRODUCT.md](PRODUCT.md) owns user-visible semantics. This document owns the
implementation boundaries. [TESTING.md](TESTING.md) alone defines when to run
checks; old milestone/evidence records do not add current acceptance gates.

## System overview

```text
React Native / Expo: screens, commands, low-frequency state
        │ typed Expo control bridge / native TerminalView
        ▼
One Rust library, Tokio runtime and terminal registry
├── russh + host authentication and lifecycle
├── tmux Control Mode / Herdr stream-local controller
├── alacritty_terminal::Term per remote terminal
└── native-only snapshots → Android GLES / iOS Metal
        │ ordinary SSH
        ▼
Selected ordinary tmux session / existing compatible Herdr runtime
```

No meeterm server component, HTTP/WebSocket terminal relay, or WebView terminal.
Do not introduce extra crates, registries or frameworks for possible future use.

## React Native and native package boundary

`App.tsx` and `app/` own navigation, controls, dialogs, settings and product state.
`modules/meeterm-terminal/` provides the typed control API and native views;
`native/meeterm-core/` owns terminal and connection state behind C/JNI adapters.
Keep one shared library/runtime/registry. Generated typed bindings remain a
preferred future improvement, not a prerequisite for unrelated work.

Commands, connection phases/errors, hierarchy/selection and low-frequency
metadata may cross JS. Terminal bytes, ANSI/VT streams, cells, continuous
scrollback, render frames, cursor blinking and native IME composition may not.
Never expose russh/Term objects or a resolved Herdr executable path to JS.

A native view binds a stable native terminal ID rather than owning the session.
Unmount/hide cannot destroy a remote pane. Hidden panes retain their Term and
local history while the process lives. Android owns Kotlin/JNI, surface/font
metrics and its input protocol; iOS owns Swift/C, UIKit/CoreText and its surface.
The terminal semantics, registry, snapshots and input contract remain shared.

## Backend identity and topology

```text
Workspace → TerminalGroup → Terminal
tmux: window → virtual mobile group → pane
Herdr: workspace → tab → pane
```

Use ordinary tmux, not a product-specific server/socket. The virtual group never
creates a window or changes the desktop layout. IDs are opaque and scoped to
connection/backend/runtime. tmux window/pane IDs are runtime identities; Herdr
`terminal_id` is stable while `pane_id` may change after a move. Preserve the
native Term when a Herdr alias changes.

Topology mutation is serialized in the selected Rust actor. Before workspace
close or a final-pane close that could affect another tmux session, verify the
current linked/shared topology in that same queue; uncertain safety fails closed.
Herdr `tab.close` and `workspace.close(close_group: false)` use its public API.
A parent with related Git workspaces may close more than requested in Herdr 0.9.0;
refuse pane/group close there after a fresh snapshot. See [HERDR.md](HERDR.md).
Do not install global tmux hooks or modify user configuration without a concrete need.

## Discovery and runtime selection

```text
Connecting → HostKeyPending → Authenticating → DiscoveringRuntimes
→ RuntimeSelection → AttachingSelectedBackend → Synchronizing → Ready
```

Host authentication and runtime selection are separate. Fresh/cold connections
always perform bounded, read-only discovery and show an explicit picker. Candidate
summaries contain native candidate ID, backend, name, status and last-used hint;
connection generation and discovery revision reject stale results and double taps.
Discovery never starts, creates, attaches or mutates a runtime.

For tmux, only verified no-server/no-session results mean an empty section.
Other failures remain errors. Retain exact session `$N`, server PID and start-time
identity. Selecting targets that discovered identity. Explicit detached creation
safely encodes the requested name, verifies the returned identity, then selects it.
A list-to-select race is an error and refresh, never attach-or-create.

Resolve Herdr natively via PATH, `~/.local/bin` and supported package-manager
locations. Retain its executable path as a connection-scoped capability, not
runtime identity or a JS/log field. Selection requires a running session,
protocol 22, API schema 1, required CLI/API capabilities and direct stream-local
operations. The pinned test binaries are fixtures, not production SemVer gates.
Reconnect may resolve a different compatible path. Do not start/install/update
Herdr or silently fall back to tmux. Bounds/errors are independent per backend.

Saved profiles own endpoint/auth metadata; legacy backend/runtime fields migrate
to non-authoritative `lastUsedRuntime`. Credential identity stays stable, and the
hint updates only after Ready.

## Switch and release boundary

One selected runtime actor owns each host connection. Release/drain the controller
before acquiring a replacement and preserve remote processes. Opening the
Server/Session sheet is presentation only. A server choice invokes `changeRuntime`
for the authenticated owner or release-then-connect for a different endpoint;
only successful explicit runtime selection commits Workspaces/lastUsedRuntime.

The typed `RuntimeBoundaryOutcome` crosses Rust/C/JNI/native/JS unchanged:

| Result | Meaning |
| --- | --- |
| `not_invoked` | Bridge argument validation failed before native invocation |
| `rejected_before_boundary` (`-1..-14`) | Old binding was not retired; retain its current/recovery screen |
| `accepted` (`0`) | Replacement started or release accepted; this does not mean Ready |
| `accepted_after_failure` (`-15`) | Old owner was retired before a later failure; do not restore it as connected |

Rust classifies control flow, not an error-code guess or a subsequent snapshot.
Unknown/thrown bridge outcomes fail closed. Cancel after release invalidates the
provisional generation and disconnects it; the next attempt uses profile/details,
not the retired connection. See the native boundary and `App.tsx` consumers.

## Retained-work recovery

Rust owns reconnect, not React hooks/timers. After Ready, transport loss keeps
the last authoritative workspace, selected Term, local history and selection
visible but read-only. Immediately revoke the operation epoch, input/resize/
mutation gates and transport; publish `stopped` only after the old actor finishes.
Late IME/paste/resize/reply/mutation callbacks cannot replay after recovery.

Reauthenticate to the same approved host and verify backend capability, runtime,
topology, original terminal and authoritative screen before committing Ready.
For tmux require the stored session identity and pane ID. For Herdr require the
same selected running runtime, original stable `terminal_id`, compatible direct
controller acquired without takeover, and first authoritative full frame.
Herdr's missing comparable server-instance identity alone is not a mismatch.
Never fall back, retarget, create a replacement or automatically open the picker.

| Stop reason | Action |
| --- | --- |
| runtime mismatch, controller conflict, exhausted/unknown retry | Retry / Change |
| missing runtime/terminal, incompatible capability | Change |
| changed host key | Review key |
| authentication failure | Connection details |

Security failures take precedence over a backend-staged reason. Workspaces and
Server-sheet Reconnect and recovery Retry use `retryRecovery(id, operationEpoch)`
when work is retained. Show Reconnect during reconnecting or retry-eligible stops;
hide it during resynchronization and Change-only/security stops.
Stopped Retry publishes `reconnecting/manual_retry` before owner handoff, resets
its retry budget, and duplicates at the current epoch are no-ops rather than
`runtime_replaced`. An in-flight attempt is also a no-op; sleeping backoff wakes.
Explicit Disconnect/Change revoke intent even after actor completion. Stale epochs
are rejected. `reconnect`/ManualReconnect and the picker are for fresh selection.

The first automatic attempt is immediate; bounded exponential backoff follows
failures. Foreground return or `network_changed()` wakes sleeping retries only
while foreground and automatic reconnect are enabled, without resetting the
budget or interrupting a healthy connection. Backgrounding alone need not close
a healthy controller. Non-explicit Herdr transport/channel/remote-close failure
abandons the dead controller locally; explicit Disconnect/Change/handoff drains
through closed/EOF before later reacquisition.

Process death loses local Term/scrollback; the remote runtime survives. Rebuild
from authoritative remote state. Full-screen TUIs need concrete testing; capture
of scrollback alone does not prove correct reconstruction.

## tmux Control Mode

Use `tmux -C` over an SSH exec channel without an outer PTY. `-CC` tries terminal
attributes and fails on a pipe (`tcgetattr`); there is no outer echo/line discipline
to disable. Parse Control Mode and escaping as bytes, route `%output` to the pane's
own Term, and keep topology notifications separate from the terminal stream.

Discovery and attach are separate channels. After attach startup succeeds, query
`session_id|pid|start_time` for the exact `$N` on that same control stream before
Ready/input acceptance. Malformed, failed or mismatched replies fail selection;
in retained recovery they stop on cached read-only work. Do not use a display
name as an identity substitute. [SSH.md](SSH.md) owns the detailed transport and
reconstruction contracts.

## Herdr direct control

Use public SSH stream-local forwarding without an outer PTY or `--takeover`.
The Rust actor owns request serialization, subscription, consistent snapshots,
events and resync. Only stable topology reaches the product model. Herdr terminal
frames contain ANSI data; feed the shared native parser rather than JavaScript.

Committed text uses `pane.send_text`, special keys `pane.send_keys`, and paste
`pane.send_input` with the complete bracketed-paste envelope where needed.
Before input send `pane.scroll(offset_from_bottom: 0)`. For keys missing in the
verified 0.9.0 parser, use the fixed xterm normal-mode encoding in the native
adapter. Hidden-view release preserves the remote process/native Term.
[HERDR.md](HERDR.md) owns capability, frame/input/resize and lease details.

## Resize, input and renderer

Native views compute columns/rows from pixel bounds and font metrics, including
keyboard/safe areas. Rust propagates selected-pane size via tmux zoom/client-size
or Herdr resize. Local stretching of a small remote pane is insufficient.
Switching tabs preserves desktop layout; graceful detach releases mobile sizing,
and ungraceful loss/handoff requires recovery. Rotation, font changes, keyboard
appearance and fold/window resizing must produce deterministic dimensions.

Native IME composition stays local until committed. UTF-8 text, modifiers,
terminal-generated replies, special keys and paste go directly to Rust's bounded
input path. Japanese/CJK wide cells, fallback, combining marks and representative
emoji share the same grid measurements as rendering. Do not concatenate user
input into shell commands.

Android currently uses EGL/OpenGL ES. iOS currently uses MTKView/Metal with
CoreText rasterizing the Rust snapshot into a texture. The Simulator-only native
CoreGraphics fallback is explicitly marked; it is not Metal evidence. Full-frame
CPU raster/upload is a known optimization area, not permission to add a JS path.
Render on demand where possible. A different backend needs measured throughput,
CJK/input/lifecycle behavior and build/maintenance justification.

## Image attachments

Rust `attachment.rs` owns destination intent, bounded SFTP operations and the
`meeterm_attachment_*` ABI. A native key-row button sends a low-frequency
request event to JS, which sequences the Photos picker, normalization, upload
and verified insert. Platform adapters keep the image bytes in app-owned
files; JS receives only a temporary opaque staging token and never image data.
Real native composition blocks the flow without committing or clearing marked
text.

Capture the original stable endpoint/runtime/terminal intent at the tap, then
re-resolve it into a fresh epoch fence for each upload/insert/retry. Same-target recovery
can proceed; destination changes or missing panes fail rather than retargeting.
One live operation per connection and one in-flight job per operation prevent
concurrent work. Attempt IDs reject stale progress/completion; `jobInFlight`
means accepted work has not completed. Cancel/dispose cannot revive a stale job.

Use bounded SFTP work outside the interactive command loop. Resolve the default
base from SFTP `realpath(".")`; reject symlinks/unsafe components and use generated
names with private directory/file modes. Stage then verify CLOSE/metadata before
rename. Verify the recorded remote file again before inserting `'<path>'␠` under
the operation/session fences. Never send Enter or claim model consumption. The
app performs upload and insert as one tap; the native core retains their
asynchronous phases and retry boundaries. Uploaded files remain on the SSH host
until manually removed. The core delete ABI remains available to native
integrations, but the app has no remote-delete or discard control. Detailed
paths, limits, policies and cleanup are in [SSH.md](SSH.md); UI behavior is in
[UI_UX.md](UI_UX.md).

## Security and persistence

Host-key trust is explicit and changed known keys never auto-accept. Optional
credentials load natively from Android Keystore-backed storage or iOS Keychain;
never return saved secrets to JS or log authentication material. Rust retains
reconnect credentials in process memory (passwords in a Zeroizing buffer).
Password authentication does not promise keyboard-interactive/MFA support.

Local persistence holds profiles, approved host keys and preferences, not a
shadow workspace database. Credential identity is independent of backend/runtime
hints. Treat remote command encoding as a security boundary and retain upstream
licenses/attribution. Do not modify external Herdr or user-wide tmux configuration.

Builds use Expo CNG from tracked config/native sources. Simulator validation is
unsigned; real-device/TestFlight distribution has separate credentials and scope.
See [CI_MOBILE.md](CI_MOBILE.md) only when executing mobile diagnostics.
