# Issue 19: reviewed mobile screens

These are actual native screenshots, not mockups or generated terminal images.
The originals are in the Android and iOS observability bundles from
[Mobile smoke 34760570576](https://github.com/phni3j9a/meeterm/actions/runs/34760570576),
app/native source `55fb60d30d439381204f6f331c9b9579f09c09ac`.
The copies here are scaled to 480 px wide and losslessly encoded as WebP for
documentation. The exact full-size PNGs remain in the original artifacts.

| Platform | Workspace | Active terminal |
| --- | --- | --- |
| iOS Simulator | [Workspace](ios-workspaces.webp) | [Terminal](ios-terminal.webp) |
| Android emulator | [Workspace](android-workspaces.webp) | [Terminal](android-terminal.webp) |

Main downloaded and viewed both platforms' screenshots: all fourteen iOS
standard states plus foundation, all twenty-one Android observational states,
and Android's actual daily-use checkpoints. These four images are selected
examples, not the entire acceptance bundle.

The pictured workspaces are seeded presentation fixtures. Their native terminal
content comes from the shared Rust demo; no terminal bytes or cells are sent
through JavaScript. Fixture images do not demonstrate the remote actions that
would ordinarily create those states. Real SSH/daily-use results, source/run
boundaries, and remaining limitations are recorded in [UI_UX.md](../../UI_UX.md)
and [PR 20](https://github.com/phni3j9a/meeterm/pull/20).

The light application surfaces and dark terminal are intentional. Terminal
palette, CJK rendering, GPU execution, and input remain native; these Simulator
and emulator images do not establish physical-device or Japanese IME parity.

## Small-screen terminal interaction

The focused `polish-navigation` run
[34778274408](https://github.com/phni3j9a/meeterm/actions/runs/34778274408)
passed on `946fa98a130b8ad7ec5a03e20bc3bfbde555eb80`, reusing the exact-source
fresh iOS products from
[34776784422](https://github.com/phni3j9a/meeterm/actions/runs/34776784422).
The Simulator is iPhone SE (3rd generation); the OS text-size readback is
extra-large. Main viewed both full-size interaction images and the fresh Metal
foundation, then checked these 480 px documentation copies.

- [Actual native terminal keyboard](ios-se-terminal-keyboard.webp): terminal
  area, accessory keys and Hide keyboard remain visible together.
- [After native edge Back](ios-se-edge-back.webp): the `Main` search and its
  selected workspace are preserved.

This establishes the focused search/keyboard/sheets/Back/edge-Back scope, not
the seven-state `polish` suite, SSH or storage. Its video capture was unavailable;
these screenshots and assertions are not a full-speed playback or FPS result.
The original compact `polish` failure and the separate standard-run failure
remain recorded rather than being replaced with this narrower pass.
