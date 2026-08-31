# ADR 0001: Prove the native terminal before SSH/tmux integration

## Status

Accepted

## Context

meeterm's product architecture depends on a terminal data plane that stays outside JavaScript and combines Rust-owned terminal state with a native mobile renderer. The highest-risk parts are not SSH protocol mechanics; they are the React Native/native view boundary, mobile GPU rendering, font/CJK behavior, IME composition, and resize/lifecycle behavior.

Building SSH, tmux orchestration, navigation, and product UI first would allow a large amount of code to accumulate before the critical native-terminal assumptions are proven.

## Decision

The first implementation milestone will be an Android native-terminal vertical slice with local fixed byte input.

It will prove:

- Expo Development Build integration;
- React Native native-view mounting;
- Rust-owned `alacritty_terminal::Term` state;
- native GPU rendering;
- Japanese/CJK font behavior;
- native Japanese IME composition;
- deterministic viewport resize behavior.

SSH and tmux integration will follow only after this slice is demonstrated on a real Android device.

## Consequences

### Positive

- The riskiest architectural assumptions fail early if they are wrong.
- WebView/JS terminal fallbacks are not accidentally entrenched.
- SSH/tmux layers can be built on a known terminal/input foundation.
- Japanese input is validated before broad UI work.

### Negative

- The first milestone will not look like a complete SSH application.
- Some throwaway local test plumbing is acceptable.
- iOS remains unproven until the Android foundation is stable enough to port.

## Guardrails

Do not broaden this milestone to include product-complete navigation, SSH, tmux, server profiles, or general remote-management features.

If the native renderer approach proves unsound, document the concrete failure and evaluate the smallest architecture change rather than silently replacing the terminal with xterm.js/WebView.
