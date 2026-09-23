# AGENTS.md

This repository is a greenfield implementation of **meeterm**, a smartphone-first SSH client with ordinary tmux and an explicitly selected Herdr backend.

Read `docs/PRODUCT.md` and `docs/ARCHITECTURE.md` before making architectural or product changes. Treat them as the current source of truth unless the task explicitly changes the product direction.

## Product invariants

- The selected remote runtime is the durable workspace source of truth.
- The tmux runtime is a user-selected session on the ordinary tmux server. `meeterm` is the suggested name for an explicitly created new session and the legacy last-used hint; it is not a fixed runtime. Herdr uses a selected running `default` or named session.
- **Workspace = tmux window or Herdr workspace.**
- **Terminal = tmux pane or Herdr pane.**
- On mobile, panes are presented as tabs and the selected pane receives a phone-appropriate full-size experience.
- On desktop, tmux remains usable through ordinary `tmux attach -t <selected-session>`, and Herdr remains usable through its normal client for the selected session.
- Smooth phone-to-PC handoff is required. Simultaneous interactive phone+PC use is not an initial requirement.

Do not change the session/window/pane mapping merely because another mapping simplifies mobile implementation.

## Issue #17 common backend contract

The invariants above describe the current, working tmux backend and remain in
force. Issue #17 adds an implemented common model without changing that mapping:

```text
Workspace → TerminalGroup → Terminal
```

For tmux, a Workspace remains a tmux window, a Terminal remains a tmux pane,
and TerminalGroup is one virtual mobile group per window. The virtual group
must not create a remote tmux window or otherwise change the desktop layout.
Herdr maps Workspace to a Herdr workspace, Group to a Herdr tab, and Terminal
to a Herdr pane. Backend selection is explicit after authenticated runtime
discovery. Legacy backend/runtime fields are a non-authoritative last-used
hint, updated only after Ready; a missing legacy backend seeds the tmux
suggestion but does not bypass the picker. The Rust/native path targets Herdr 0.9.0,
protocol 22, schema 1 through its existing direct public API. Keep acceptance
evidence scoped to the tested source and suites in
[`docs/evidence/issue-17-herdr-mobile.md`](docs/evidence/issue-17-herdr-mobile.md). See [`docs/HERDR.md`](docs/HERDR.md) and the historical
record [`docs/evidence/issue-17-herdr-feasibility.md`](docs/evidence/issue-17-herdr-feasibility.md).

An additional backend connects to a user-selected remote runtime over ordinary
SSH. The prohibition on a meeterm gateway or daemon prohibits meeterm's own
relay; it does not prohibit the selected remote Herdr server. Herdr is an
existing external application. Integrate with its existing public interfaces;
do not modify, fork, install, or update Herdr, and do not make an upstream API
addition a prerequisite of this issue. Keep compatibility limits explicit.

## Architecture invariants

The intended data path is:

```text
React Native / Expo
        │ commands / state snapshots only
        ▼
Rust native core
├── russh
├── backend selector
│   ├── tmux Control Mode
│   └── Herdr direct stream-local control
├── terminal lifecycle/registry
├── alacritty_terminal
└── native GPU renderer
        │
        ▼
ordinary SSH
├── selected ordinary tmux session
└── selected existing Herdr session/socket
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

The remote host should require ordinary SSH access and the selected runtime:
tmux for the default backend, or an existing Herdr session/socket for Herdr.
This still must not introduce a meeterm gateway, daemon, HTTP terminal
transport, WebSocket terminal transport, or hosted relay.

### No WebView terminal fallback

Do not replace the native terminal architecture with xterm.js/WebView as a shortcut unless a task explicitly changes the architecture after documenting the tradeoff.

The target is `alacritty_terminal` plus a native GPU renderer.

### Use ordinary tmux

Do not isolate meeterm into a separate tmux server/socket such as `tmux -L meeterm` for the normal product path.

A desktop user must be able to run the ordinary client against the selected
runtime, for example:

```bash
tmux attach -t <selected-session>
```

without meeterm-specific desktop software. `meeterm` may be substituted when
it is the selected session or the name chosen by the explicit tmux create
action; it is not assumed when listing or reconnecting.

## tmux integration

Use the selected backend's structured public boundary as the mobile integration
boundary: tmux Control Mode for tmux and Herdr's direct stream-local API for
Herdr.

- Treat tmux pane IDs (`%...`) and window IDs (`@...`) as stable runtime identities where appropriate; Herdr pane IDs are mutable aliases and stable `terminal_id` is the remote identity.
- Decode Control Mode output as bytes; do not assume pane output is ordinary UTF-8 text.
- Route each pane's output to its own native terminal state.
- Preserve the underlying tmux window/pane layout while adapting presentation for mobile.
- Mobile pane selection/zoom must not permanently destroy the desktop layout.
- Do not install global tmux hooks or mutate user configuration without a demonstrated need and narrowly scoped design.

Avoid shell command construction from untrusted or user-visible names. Prefer typed command/argument encoding and explicit tmux targets.

## Runtime discovery and selection

An authenticated SSH host connection and a selected runtime are separate
native lifecycle stages. A fresh manual connection and a cold start always
perform bounded, read-only discovery and show a picker grouped into tmux and
Herdr sections. A last-used hint may highlight a row, but it never selects or
attaches by itself. Discovery must not create, start, attach, or otherwise
mutate a runtime.

The tmux section lists arbitrary sessions from the ordinary server. A verified
empty server/session result is an empty section; other command failures remain
errors. Selecting a row targets its exact live session identity. Creating a
tmux runtime is a separate detached action, suggests `meeterm` as its name,
verifies the returned identity, and then selects it. A list-to-select race is a
stale-selection error followed by refresh, never an implicit create.

The Herdr section resolves the compatible 0.9.0 executable natively using PATH,
the official `~/.local/bin` installer default, and common package-manager
locations, and keeps that resolved path as a
connection-scoped capability for list, status, controller setup, and later
proof-gated operations. The path is not exposed to JavaScript or ordinary
logs. Rows distinguish running candidates from stopped sessions; only running
rows may be selected, and selection revalidates protocol 22, schema 1, and the
direct stream-local contract. Herdr start/create is not promised by this
issue, and meeterm never silently falls back to tmux when Herdr discovery or
selection fails.

Each backend has bounded, independent discovery and error state. A missing or
incompatible tmux/Herdr capability is local to its section unless the user
selects that backend. There is one selected runtime actor per SSH host
connection. Switching or releasing it drains/closes the backend controller
and preserves the remote process before another selection is acquired.

After a runtime has reached Ready, same-process transport recovery preserves
the last authoritative workspace, selected terminal, native Term, and local
history as a stale read-only work screen. Input, resize, and remote mutations
remain blocked until the host identity/authentication, backend capability,
runtime identity, topology, selected terminal, and authoritative screen are
verified and committed. tmux recovery requires the exact stored session
identity and pane ID. Herdr recovery requires an in-work explicit confirmation
because 0.9.0 has no comparable server-instance identity, followed by fresh
candidate/compatibility/stable-terminal/full-frame validation without
takeover. A missing, replaced, restarted, incompatible, or uncertain target
stays fail-closed on the stale screen with Retry and Change actions; it does not
open the picker, create, retarget, or fall back automatically. A fresh manual
connection and cold start never skip the picker.

Saved server profiles own SSH endpoint and authentication metadata. Existing
backend/runtime fields represent a non-authoritative logical `lastUsedRuntime`
hint;
profile IDs and credentials remain independent, and the hint is updated only
after the selected runtime reaches `Ready`. Credential secure-storage identity
must not change merely because the backend or runtime hint changes.

The common backend target must keep remote identifiers opaque and scoped by
connection, backend, and runtime. A Herdr pane identifier may change when a
pane moves between workspaces; do not use it as an immutable local terminal
handle. Reuse the shared Rust registry, `alacritty_terminal::Term`, native
snapshot format, and bounded input/resize transport for every backend.

## Rust / native structure

Prefer simple module boundaries first. Do not create crates, abstraction layers, traits, registries, or generalized frameworks solely because they may be useful later.

One mobile native package and one shared native library/runtime/terminal registry are preferred. Avoid independent native libraries that accidentally duplicate Tokio runtimes or live terminal registries.

Generated typed bindings are preferred for the low-frequency React Native ↔ Rust control plane. The native terminal view should bind to stable terminal IDs instead of owning the remote session lifetime.

The terminal semantics, opaque IDs, registry ownership, input contract, and native-only snapshot format should be shared across Android and iOS. Android and iOS should remain thin adapters for their own view/surface lifecycle, font metrics, text-input protocol, renderer backend, and native build integration. Do not duplicate the Rust terminal state or create a platform-specific second source of truth merely to make one adapter convenient.

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

- SSH is transport; the selected remote runtime is durable state. tmux uses the
  selected ordinary session; Herdr uses its selected running `default` or
  named session. `meeterm` is only a suggested new-session name and legacy
  hint.
- Each backend owns reconnect and resynchronization in the Rust core. Herdr
  release closes/EOFs the direct controller stream before a later stable-ID
  reacquire, while the remote process remains alive.
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

## Current engineering sequence and acceptance

The first implementation milestone is a dual-platform native terminal foundation. Prove shared Rust terminal semantics first, keep Android and iOS as thin native adapters, and establish both hosted mobile validation paths early. An environment-only iOS check must not be reported as iOS terminal verification.

The dual-platform native foundation and the selected-backend control boundary
are implemented together. Keep verifying:

1. Shared Rust-owned `alacritty_terminal::Term` semantics, deterministic resize, input encoding, and native-only snapshot fixtures.
2. One native package contract in which both platform views bind stable terminal IDs and retain one shared registry/runtime.
3. Expo Development Build/CNG generation for Android and iOS, with generated native directories remaining untracked.
4. Thin Android and iOS native `TerminalView` adapters that keep terminal bytes, cells, render frames, and IME composition out of JavaScript.
5. A real native GPU surface on each platform consuming the shared terminal snapshot. The iOS backend choice remains an evidence-driven implementation decision; see `docs/ARCHITECTURE.md`.
6. Japanese/CJK/font behavior, native Japanese IME composition and committed input, and deterministic resize behavior on the applicable platform paths.
7. Devin Cloud Android emulator and iOS Simulator validation sessions that build, install, launch, signal native readiness and a first frame, and detect crashes while always pushing an observability bundle to an evidence branch.

The Herdr live case is an opt-in ignored Rust integration test because it needs a
real Herdr 0.9.0 binary. It uses an isolated russh test endpoint, not the older
OpenSSH fixture. Mobile validation is tiered: fast source checks and a short
native core smoke provide continuous feedback, while Android full and iOS
`standard` provide final acceptance once the relevant source is stable and
before merge. Do not spend full-matrix runner time on every intermediate push.
The short core smoke must still build the generated app when required, install
and launch it, observe native readiness and a first terminal frame, and check
that the process does not crash. It is not a substitute for final full and
`standard` acceptance.

The iOS `standard` source-level manifest has 45 screens: its previous 25 plus
the ten `session-switcher-*` states and a `-dark` variant of each. Android's
observational `SCREEN_NAMES` has 51 routes: its previous 31 plus those same 20
light/dark switcher fixtures. The switcher states are `current`, `loading`,
`partial-error`, `stopped-herdr`, `credentials`, `host-key`, `create`, `pending`,
`failure`, and `long-names`. The existing `runtime-picker` and related
`runtime-*` routes remain fresh selection states, now rendered through the
unified `SessionSwitcher`; `herdr-connection` shows the Herdr `default`
candidate's non-authoritative `Last used` hint. These are source-level scopes,
not remote CI or visual-review results.

The explicit iOS `polish` diagnostic adds seven presentation states and native
navigation/keyboard/back checks. `polish-navigation` independently exercises the
same navigation helper and a fresh native foundation; it does not validate the
seven states or replace a failed `polish` result. Android's seeded presentation
routes are observational evidence and do not replace the machine gate. No test
result may claim acceptance without the applicable integration/CI evidence and
required visual review.

## Standard testing workflow

Read `docs/TESTING.md` before changing tests, CI, or native code. Use it for the
current suite contents and commands; the tier and trigger policy below controls
when those suites run. Drive the Devin Cloud mobile sessions with
`scripts/ci/devin-cloud.py`, which creates and messages SWE-2 sessions through
`devin acp --cloud` without the Web UI. See `docs/CI_MOBILE.md`. Do not run a
validation under a different model when SWE-2 is unavailable. Use these test
tiers:

- On every relevant push, run the fast Rust, JavaScript, Swift/Kotlin, native
  bridge, and build-contract checks selected for the changed paths.
- The validation path should provide a short Android emulator and iOS Simulator
  core smoke for changes that affect the mobile runtime, generated projects,
  native bridge, renderer, lifecycle, or user flow. Once available, run it as a
  blocking check before merge. Keep this suite focused on CNG or product
  restoration, install, launch, native readiness, first frame, and no-crash
  evidence, plus only the smallest representative UI state needed to prove the
  path.
- Once the relevant source is stable and before merge, run Android full and iOS
  `standard` on the exact candidate commit. These acceptance runs cover the full
  interaction and presentation matrices and are required for applicable mobile
  changes, but they do not need to run after every intermediate push. Run them
  again after any later source change that affects their product or assertions.
- Use the small `ssh` round-trip for connection, authentication, runtime
  selection, or native input changes and before distribution. Do not run it for
  unrelated UI or documentation changes.

Until the short core smoke exists in the validation path, keep using the current
Android full and iOS `standard` suites for final acceptance; a source-only check
must not be presented as the missing runtime gate. The long iOS `full` flow and
`forms`/`native`/`names` suites remain explicit diagnostics and are not required
for every change. Keep UI fixtures behind the smoke build flag and an explicit
test launch route; screenshots of seeded state verify presentation, not the user
actions that would ordinarily create that state. Preserve real native terminal
rendering and never send fixture terminal bytes/cells through JS.

For runtime selection and the session switcher, the applicable Rust/native checks also cover bounded
side-effect-free discovery, tmux list/create/select and exact identity,
Herdr executable resolution and running-session list/select, independent
backend failures, profile migration, reconnect identity, switch/release, and
fail-closed linked/shared tmux topology mutations. Mobile evidence must cover
picker loading, duplicate-name, stale-selection, asynchronous refresh, and
explicit selection/create state transitions in focused app/native tests. The
45-screen iOS source manifest and 51-route Android observational `SCREEN_NAMES`
include the four recovery visual routes `recovery-progress`,
`recovery-exhausted`, `recovery-mismatch`, and `herdr-recovery-confirm` in
addition to the fresh-selection routes, the two `layout-restore-unconfirmed`
warning fixtures, and all ten light/dark session-switcher fixture pairs. Android full and iOS `standard` plus `ssh`
remain the required mobile paths for this connection-lifecycle change; both
platform screenshots from the applicable exact-source acceptance runs must be
downloaded and actually viewed before the corresponding evidence is reported.

The iOS UI/input Swift preflight runs before CNG/app compilation. Report each
suite by its actual scope; never rename an old full failure into a passing result.
For another suite or diagnosis on identical source, reuse pristine iOS test
products only through the same-session commit/toolchain match described in
docs/CI_MOBILE.md. Record the
original fresh build and the reuse run. Changes to app, native, test, or build
inputs require a new build.
Investigate the first failed stage before rerunning; retain bounded state waits,
exact completion evidence, and sanitized artifacts. Do not fix failures by
silently skipping assertions, adding blind retries, or extending deadlines.

## CI and visual evidence boundary

For both mobile validations, the machine-gated acceptance boundary is: generated project/build succeeds, the app installs, the app launches, the expected native module is ready, a first native terminal frame is reported, and the process does not crash. These gates do not claim physical-device GPU, font fallback, rotation, or IME parity.

The iOS validation must distinguish a Metal first-frame marker from the Simulator-only native CoreGraphics fallback marker. The fallback still validates the Rust snapshot, CoreText, view, and input boundary, but it is not evidence that Metal executed.

The observability bundle is pushed to an evidence branch on every run, including failed runs. After app launch it should contain a screenshot and sanitized native log; if launch or capture was not reached, it must contain an explicit unavailable diagnostic rather than a fake image. Do not add a screenshot-existence or pixel-difference gate at this stage. For every native UI change, Main must fetch the evidence branch and actually view both the Android emulator and iOS Simulator screenshots from the final exact-source acceptance runs before reporting visual success; a pushed bundle or a passing process check is not visual review. Intermediate short-smoke screenshots are diagnostic and do not require a full presentation review unless they are the evidence being used for a visual claim. The general Rust CI downloads the official Herdr 0.9.0 binary only into `RUNNER_TEMP`, verifies its pinned SHA-256, and runs the ignored russh integration; record the exact run and result in the acceptance evidence.

The iOS Simulator validation is an unsigned simulator build/install boundary and must not require distribution certificates, provisioning profiles, or Apple signing secrets. Physical-device validation and TestFlight distribution are later, separate signed workflows with their own credentials and acceptance criteria. Simulator Keychain tests run in an app-hosted unit-test target with isolated app entitlements embedded in the Mach-O XML and DER sections; these are generated only for the disposable Simulator app while code signing remains disabled. Entitlement sections in a UI test bundle do not establish entitlement availability in its separate XCTest runner process. See `docs/DAILY_USE.md` for the focused reproduction and actual validation scope.

Treat updates to Expo/React Native, the Rust terminal stack, Android SDK/NDK/Gradle, Xcode/SDK/CocoaPods, fonts, or the chosen iOS renderer backend as cross-platform native dependency changes. Regenerate CNG output on a fresh checkout, run the short core smoke while iterating, and complete fresh Android full and iOS `standard` acceptance on the final candidate commit; do not patch ignored generated directories to accommodate a dependency update.

## Change policy

If an implementation task reveals that one of these invariants is technically unsound, do not silently work around it. Document the observed constraint, reproduce it with a focused test/PoC, and propose the smallest architecture change needed.
