# ADR 0001: Prove native terminal foundations before SSH/tmux integration

## Status

Accepted

## Context

meeterm's product architecture depends on a terminal data plane that stays outside JavaScript and combines Rust-owned terminal state with a native mobile renderer. The highest-risk parts are not SSH protocol mechanics; they are the React Native/native view boundary, mobile GPU rendering, font/CJK behavior, IME composition, and resize/lifecycle behavior.

Building SSH, tmux orchestration, navigation, and product UI first would allow a large amount of code to accumulate before the critical native-terminal assumptions are proven.

## Decision

The first implementation milestone will be a dual-platform native-terminal foundation with local fixed byte input. Shared Rust terminal semantics come first, followed by thin Android and iOS native adapters. GitHub-hosted Android emulator and iOS Simulator jobs are established early alongside the adapters.

It will prove:

- shared Rust-owned `alacritty_terminal::Term` semantics and native-only snapshots;
- Expo Development Build integration and React Native native-view mounting on both platforms;
- thin Android and iOS native adapters without a JavaScript terminal data path;
- native GPU rendering;
- Japanese/CJK font behavior;
- native Japanese IME composition;
- deterministic viewport resize behavior.

The emulator/Simulator jobs machine-gate build, install, launch, native readiness, first native frame, and no crash. Every run uploads an observability bundle with available screenshots/logs or explicit unavailable diagnostics; screenshots are not existence or pixel-diff gates. SSH and tmux integration will follow only after both native adapters demonstrate that this foundation is viable. Physical-device GPU, font, and IME validation remains separate follow-up evidence.

## Consequences

### Positive

- The riskiest shared and platform-specific architectural assumptions fail early if they are wrong.
- WebView/JS terminal fallbacks are not accidentally entrenched.
- SSH/tmux layers can be built on a known terminal/input foundation.
- Japanese input is validated before broad UI work.

### Negative

- The first milestone will not look like a complete SSH application.
- Two native adapters and two hosted mobile jobs add build and dependency coordination before product UI.
- Some throwaway local test plumbing is acceptable.
- Emulator/Simulator evidence does not establish physical-device GPU, font, or IME parity.

## Guardrails

Do not broaden this milestone to include product-complete navigation, SSH, tmux, server profiles, or general remote-management features. Do not make an iOS screenshot from a static React Native placeholder count as native terminal verification.

If the native renderer approach proves unsound, document the concrete failure and evaluate the smallest architecture change rather than silently replacing the terminal with xterm.js/WebView.
