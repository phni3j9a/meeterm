# Mobile interface

Issue [#19](https://github.com/phni3j9a/meeterm/issues/19) brings the final
[`docs/mock`](mock/README.md) direction into the native app and refines its
English interface. This is work in progress; the verification section below
separates implemented behavior from evidence still to be gathered.

## Visual direction

The app opens in a warm light appearance. Quiet ivory surfaces, dark brown
type, thin separators, and one amber-brown action color follow the mock.
The type hierarchy is a 30 pt workspace title, 18 pt row titles, 15–16 pt body
text, and 12–13 pt supporting information. Layout uses a 4 pt spacing rhythm;
rounded surfaces use 8 or 12 pt corners and native continuous curves.
Icon buttons are at least 44 × 44 pt. Lucide supplies one consistent line
family instead of text characters used as icons. It matches the mock's
stroke vocabulary and renders without loading an icon font.

The active terminal is a separate dark work surface, including its heading
and pane strip. Bright ANSI colors such as cyan lose contrast on pale
backgrounds, and remote editors may assume a dark palette. App appearance
therefore does not recolor the terminal. Existing dark/system app preferences
remain supported; new installations default to light. The shared native
renderer and input path still own terminal output, cells, and composition.

The meerkat is a small companion beside the workspace heading and a larger
illustration before the first connection. It does not appear in the active
terminal. The new PNG has real alpha transparency; no native blend-mode or
white-background removal is required. See [asset provenance](../app/assets/README.md).

## Navigation and state

- Workspaces are the main destination. A server name opens the saved-server
  selector; the adjacent menu opens connection actions. Settings is a helper
  opened from the top toolbar or terminal menu.
- Workspace rows show their terminal names. Selecting a workspace opens its
  selected pane. Pane tabs and Herdr groups retain the remote hierarchy.
- Search and its list offset survive opening and leaving a workspace.
- A two-screen native stack provides platform navigation transitions and the
  iOS edge-back gesture. Switching terminals does not push additional routes.
  Reduced Motion selects a fade; no terminal frames cross JavaScript.
- Server management, naming, connection details, and settings have explicit
  close/cancel boundaries. Unsaved forms ask before discarding changes.
- Connection progress, empty results, empty workspaces, lost connections,
  authentication problems, and host-key changes use English copy. Host-key
  verification and destructive remote actions remain explicit.
- Disconnecting releases the mobile connection while remote work keeps
  running. Computer handoff explains the ordinary tmux/Herdr command.

## Research, 2026-09-13

Fresh Appllama queries retrieved and locally preserved all 19 Termius,
15 Bear, and 9 Ulysses `other-tabs` images (43 images). Six contact sheets
were actually viewed. These revisit some references in the mock's earlier
research and are not claimed as 43 previously unseen screens. Working copies
are in `/tmp/meeterm-ui-research`; this document preserves durable screen IDs.

| Reference | Pattern applied |
| --- | --- |
| Termius `549039908/oth_dgiex`, `oth_18ct8` | Group server fields, keep authentication separate, use explicit form completion |
| Termius `549039908/oth_eudnq`, `oth_1sla4` | Separate terminal readability/input preferences from remote workspace selection |
| Termius `549039908/oth_2yu0u`, `oth_a4yae` | Explain empty states with one clear next action |
| Bear `1016366447/oth_zp1lk`, `oth_dzgyd` | Let the working list occupy the main surface; subordinate metadata to the title |
| Bear `1016366447/oth_5vrqu`, `oth_ikp8k` | Give the active editor/terminal most of the screen and keep its input native |
| Bear `1016366447/oth_hb8jk`, `oth_tzn2s` | Restrained mascot use and grouped settings with an explicit dismissal |
| Ulysses `1225570693/oth_4flur`, `oth_x9rhl`, `oth_agw66` | Workspace-first navigation, contextual settings, predictable overflow actions |

Only their screen structure was observed. These images do not establish the
reference apps' gesture behavior or frame performance. The Appllama watermark
is provenance and is not reproduced in meeterm.

Icon integration follows the [Lucide React Native package](https://lucide.dev/guide/packages/lucide-react-native)
and the [Expo SVG integration](https://docs.expo.dev/versions/latest/sdk/svg/).
Expo selected the SVG version compatible with the repository's SDK.
Navigation uses [React Navigation's native stack](https://reactnavigation.org/docs/native-stack-navigator/)
and Expo-compatible `react-native-screens`; CNG regenerates both platform projects.

Light supporting text and placeholders are checked against both the ivory
background and the darker grouped surface (minimum 4.54:1). The primary
white-on-brown button is 6.06:1. These calculations do not replace visual review.

## Verification and remaining work

The initial implementation passes TypeScript and the existing nine app
selection tests. The Android driver regression suite passes after its visible
labels were updated to English. These checks do not prove rendering or motion.

Still required: fresh Android full and iOS standard runs, actual inspection of
both platforms' screenshots, interaction/back/keyboard review, compact and
large-text layout, reduced-motion behavior, and a record of observed limits.
The existing iOS physical-device/Metal boundary remains separate.

The iOS standard and Android fixture drivers now wait for the actual English
labels. Required assertions and completion checks have not been skipped or
replaced with delays.
