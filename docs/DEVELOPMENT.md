# Development

## Current phase

meeterm is in the architecture-validation phase. The first goal is not to build the full application. It is to prove the native terminal foundation on Android with the smallest vertical slice that exercises the risky boundaries.

Read, in order:

1. `PRODUCT.md`
2. `ARCHITECTURE.md`
3. repository root `AGENTS.md`

## First milestone: Android native terminal vertical slice

The milestone is complete only when a real Android device can run an Expo Development Build containing a native terminal view whose terminal state is owned by Rust.

The slice should prove:

- React Native can mount the custom native terminal view;
- Rust owns an `alacritty_terminal::Term`;
- fixed byte sequences, including ANSI/VT behavior, update that Term;
- a native GPU renderer displays the Term;
- the renderer handles representative Latin and Japanese/CJK text;
- native keyboard focus supports Japanese IME composition and committed text without using JavaScript as the composition stream;
- viewport resize is deterministic.

## Deliberate omissions from the first milestone

Do not add these merely to make the project look more complete:

- SSH;
- tmux;
- server profiles;
- reconnect logic;
- workspace screens;
- production navigation;
- broad settings;
- file browsing;
- backend services.

They come after the terminal foundation is proven.

## Implementation strategy

Use a proven native-terminal implementation as a reference where useful, especially for mobile build glue and Alacritty-derived renderer integration. Preserve applicable license notices when code is reused or adapted.

Prefer a thin vertical implementation over a generalized terminal framework. It is acceptable for the first slice to feed fixed local terminal bytes into the Rust terminal state.

Do not create speculative APIs for SSH/tmux before those layers exist.

## Validation

At minimum, validation should include:

### Rendering

- ASCII text;
- ANSI foreground/background/style changes;
- cursor positioning;
- wrapping;
- enough scrolling to exercise scrollback;
- Japanese wide characters such as `日本語`;
- mixed ASCII/CJK text;
- combining characters;
- representative terminal emoji if supported by the chosen font strategy.

### Input

- ASCII typing;
- Enter, Backspace, Tab, Escape;
- arrow keys;
- Japanese IME composition such as converting `きょう` to `今日` before commit;
- committed Japanese text enters the native terminal input path exactly once;
- composition is not prematurely transmitted through JavaScript.

### Resize

- portrait resize;
- landscape resize;
- software keyboard appearance/disappearance;
- foldable/window-size changes where the available test device permits them.

## Definition of done

The milestone is done when:

- the Android project builds reproducibly from documented commands;
- the Development Build launches on a real device;
- the native terminal view renders from Rust-owned terminal state;
- the input tests above work without a WebView terminal or JS terminal byte stream;
- known limitations are documented explicitly;
- automated tests cover byte/terminal logic that can be tested below the platform renderer;
- CI checks formatting/static/unit-level concerns that are meaningful without pretending to replace device validation.

Do not mark the milestone complete solely because a simulator/emulator renders a Latin-only terminal.
