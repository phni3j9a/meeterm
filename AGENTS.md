# AGENTS.md

meeterm is a smartphone-first SSH client for ordinary tmux and an explicitly
selected existing Herdr runtime. Keep everyday changes small and quick to verify.

## Read only what the task needs

- Product behavior: [docs/PRODUCT.md](docs/PRODUCT.md).
- Native, SSH, lifecycle or architecture changes: the relevant sections of
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), then [docs/SSH.md](docs/SSH.md)
  or [docs/HERDR.md](docs/HERDR.md) when needed.
- Tests and CI: [docs/TESTING.md](docs/TESTING.md) is the single authority for
  **what to run and when**. Read [docs/CI_MOBILE.md](docs/CI_MOBILE.md) only
  when actually running or changing mobile diagnostics.
- Product/architecture decisions: [docs/ENGINEERING_PRINCIPLES.md](docs/ENGINEERING_PRINCIPLES.md).

Do not read all evidence/history or run all suites as a prerequisite to a small
change. Older milestone documents and evidence describe their recorded source;
they do not add requirements to the current testing policy.

## Product and security boundaries

- The selected remote runtime is durable state. Workspace = tmux window or
  Herdr workspace; Terminal = pane. TerminalGroup is virtual for tmux and a
  Herdr tab. Mobile presentation must preserve desktop layout and normal attach.
- Use the ordinary tmux server, never a product-specific `tmux -L` socket.
  `meeterm` is a suggested new-session name, not a fixed runtime.
- Fresh/cold connections use bounded read-only discovery and an explicit picker.
  Last-used hints never attach automatically. Do not create a runtime after a
  selection race, or fall back silently between backends.
- Herdr is external software: use its public interfaces; do not modify, install
  or update it. Compatibility is protocol 22/schema 1/required capabilities,
  not a pinned production version or executable path.
- Keep terminal bytes, cells, continuous scrollback, render frames and IME
  composition native. React Native owns controls and low-frequency snapshots.
  No WebView terminal, meeterm gateway/daemon, or HTTP/WebSocket terminal relay.
- Share one Rust runtime/terminal registry and `alacritty_terminal::Term` across
  thin Android/iOS adapters. Views bind stable terminal IDs; unmount does not
  destroy remote panes. Herdr pane aliases are not stable terminal identity.
- Verify SSH host keys; changed keys require explicit review. Credentials belong
  in platform secure storage and never in logs. Encode remote commands safely.
- Rust owns reconnect. Retained work stays read-only until the original target
  and authoritative screen are verified. Retry preserves that intent; explicit
  Change starts selection. Never replay stale input or retarget missing panes.
- Release controllers before acquiring replacements; keep remote processes alive.
  Destructive topology changes must not affect another session inadvertently.
- Japanese/CJK, native IME, wide/combining characters and font fallback are core
  requirements. Do not move composition into a JS TextInput.

## Development and verification

- Follow the user's Git/Issue/PR instructions. Reuse the current task's branch;
  start independent work from the latest default branch. Preserve unrelated work.
- Default to changed-path checks and a regression that demonstrates the bug.
  Small UI changes do not require both simulators, all screenshots, real SSH,
  or a dependency update. See the matrix in TESTING.md for native/high-risk work.
- Full Android and iOS suites are explicit diagnostics, not automatic per-PR
  acceptance. Do not expand a focused task because an old checklist says so.
- Keep Expo CNG output (`android/`, `ios/`) untracked. Fix source/configuration,
  not generated projects. Rebuild affected native inputs when testing them.
- Keep assertions meaningful. Do not add tests that merely mirror source text
  or implementation shape. Do not mask failures with blind retries or longer waits.
- Report actual scope: source checks are not runtime proof, seeded screens are
  presentation only, and Simulator CoreGraphics is not Metal or physical IME.
  View an image before claiming visual success for that image/platform.
- Record result, relevant checks and remaining limitations in the PR. Keep
  detailed run artifacts in evidence; do not copy the same history into guides.
- Do not add abstractions, states or confirmation UI for hypothetical risks.
  A concrete failure or an explicit product requirement must justify complexity.
