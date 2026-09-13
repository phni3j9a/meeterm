# Mobile interface

Issue [#19](https://github.com/phni3j9a/meeterm/issues/19) brings the final
[`docs/mock`](mock/README.md) direction into the native app and refines its
English interface. Implementation and the normal hosted mobile gates are
complete; the verification section below separates reviewed evidence from
remaining diagnostic and physical-device limits.

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
- The iOS history-pan delegate yields horizontal motion to navigation while
  keeping selection dragging away from the left edge. Search lists use native
  keyboard insets; clearing a query is one action in the search field.
- iOS page sheets preserve the presenting screen's status-bar appearance.
  Android native dialogs use the same warm accent through a CNG config plugin.
- Android native shortcut keys have 44 dp targets and native ripple feedback.
  Keys scroll horizontally; Paste and Copy stay fixed. The toolbar remains
  48 dp tall, preserving its terminal sizing and native input contract.
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
Gesture arbitration follows UIKit's
[`gestureRecognizerShouldBegin`](https://developer.apple.com/documentation/uikit/uigesturerecognizerdelegate/gesturerecognizershouldbegin(_:))
boundary. Keyboard spacing uses React Native's
[`automaticallyAdjustKeyboardInsets`](https://reactnative.dev/docs/scrollview#automaticallyadjustkeyboardinsets).

Light supporting text and placeholders are checked against both the ivory
background and the darker grouped surface (minimum 4.54:1). The primary
white-on-brown button is 6.06:1. These calculations do not replace visual review.

## Verification and remaining limits

[Selected reviewed native screens](evidence/issue-19-ui/README.md) show the light
workspace and dark terminal on both platforms. They are actual screenshots,
not generated mockups. Full-size originals remain in the linked CI artifacts.

The latest tested candidate is `946fa98a130b8ad7ec5a03e20bc3bfbde555eb80`.
Its app/native source remains `1657174`: `57d36d2` fixes two Swift test
references and `946fa98` adds the focused navigation diagnostic without
changing the app, native code or the existing standard/SSH test helpers.
Android full, iOS standard and focused SE/XL navigation passed on this candidate;
short SSH passed on the unchanged app/native source at `57d36d2`.
The startup and Paste observations do not change the visual design, renderer,
dependencies or input delivery/cancellation semantics. Main downloaded and
actually viewed both platforms' latest normal images and the SE interaction
images. The final documentation-only commit does not require another mobile build.

| Check | Actual scope | Evidence |
| --- | --- | --- |
| Android full | Native readiness/frame/no crash, real SSH and daily-use flow, settings, selection/copy, lifecycle and desktop handoff | [34776784422](https://github.com/phni3j9a/meeterm/actions/runs/34776784422), job `103776242392`, passed on `946fa98`; all 21 observational states and four actual daily-use images viewed. That run's original iOS standard failed, not the Android job |
| iOS standard | Four storage cases, seven input cases plus one gesture case, original fourteen screens, fresh native readiness/frame/survival | [34779460110](https://github.com/phni3j9a/meeterm/actions/runs/34779460110) passed on `946fa98`, reusing pristine exact-source products from fresh build [34776784422](https://github.com/phni3j9a/meeterm/actions/runs/34776784422); all fourteen screens plus the Metal foundation viewed. This is a controlled revalidation, not a second fresh build or a relabeling of the original failure |
| iOS default polish | Seven public states; real search, native keyboard show/hide, settings, picker, explicit Back and edge Back preserving search | [34761931479](https://github.com/phni3j9a/meeterm/actions/runs/34761931479) passed on `55fb60d`; seven states, two actual interaction screenshots and the Metal foundation viewed. This predates the added diagnostics |
| iOS SE/XL navigation | iPhone SE (3rd generation) Simulator with OS content size verified as extra-large; real search, native keyboard, sheets, Back, edge Back and fresh foundation | Focused [34778274408](https://github.com/phni3j9a/meeterm/actions/runs/34778274408) passed on `946fa98`, reusing the same pristine fresh-build products; actual keyboard/edge-Back screenshots and Metal foundation viewed. This is not the seven-state `polish` suite |
| iOS SE/XL seven-state polish | Public-state presentation followed by the navigation helper | [34773660488](https://github.com/phni3j9a/meeterm/actions/runs/34773660488) on `57d36d2` remains failed at long-workspaces after six captured states; six images and the blank failure screen viewed. Navigation/terminal keyboard was not reached in this run |
| iOS short SSH | Native keyboard prefix, Paste, Return, remote acknowledgment and disconnect | Same-product [34773642992](https://github.com/phni3j9a/meeterm/actions/runs/34773642992) passed on `57d36d2`; native Paste `accepted=1`, matching remote marker, and disconnect verified. Initial/input/disconnected images viewed |
| Shared Rust | 79 unit cases, real OpenSSH, pinned-SHA Herdr 0.9.0 integration through the isolated russh endpoint | [34776698058](https://github.com/phni3j9a/meeterm/actions/runs/34776698058), job `103776001069`, passed on `946fa98`; OpenSSH completed in 15.73 seconds and Herdr integration in 20.69 seconds |

TypeScript, 24 App/Settings and startup-observation tests (including nested
cases), and 160 Python SSH/driver regressions passed locally. CI-processing regressions also
passed (20 tests, with one macOS-only case excluded on Linux). Android CNG
generation passed, including inspection of generated day/night native accent
resources. Generated native directories remain ignored.
General CI on `946fa98` passed for both
[push](https://github.com/phni3j9a/meeterm/actions/runs/34776698058) and
[PR](https://github.com/phni3j9a/meeterm/actions/runs/34776702202).

### Preserved failures and controlled revalidation

The seven-state SE/XL failure is at the long-workspaces presentation guard (Swift
line 604 on `57d36d2`), after six public states were captured. The app is
foreground in the failure observation and its initial smoke URL was accepted.
The final process's AppContent effect was logged about 34 seconds after its
root effect, but this alone does not identify a rendering, startup or
accessibility cause. A white failure image is available; the long-row
existence/hittability state was not recorded. No keyboard or navigation
acceptance is claimed for that run.

Fixed startup and native Paste diagnostics are opt-in and contain no input
text, URLs or remote IDs. Optional connection-entry images are allowed only
before any connection input. The latest successful SSH trace shows provider
completion, native delivery acceptance and the actual remote marker. It does
not establish the cause of earlier intermittent startup/Paste failures.
Those failures remain in the PR history; no timeout, assertion or remote
acknowledgment requirement was relaxed to obtain a pass.

The initial `946fa98` standard job failed during the workspace-name route's
QuickPath tutorial: Continue was recorded, but dismissal completion was not.
The safe diagnostics record source line zero, runner timeout and exit 65; they
do not distinguish a tap, wait, runner or product cause. Storage/input and the
first seven screenshots passed, but the remaining seven and fresh foundation
were not reached. That original [run](https://github.com/phni3j9a/meeterm/actions/runs/34776784422)
remains failed. Read-only triage verified that the app, standard test and
QuickPath helper were unchanged from the preceding successful `57d36d2` run.
Main then authorized one exact-source standard revalidation using the existing
pristine products. It passed, with all fourteen screens and the Metal foundation
actually reviewed. No assertion, timeout or source change was made, and no
automatic retry loop was added. The pass does not establish the original cause
or claim that an intermittent failure was repaired.

Earlier SE/XL standard passed all fourteen screens in
[34756003466](https://github.com/phni3j9a/meeterm/actions/runs/34756003466)
on `d0fafc1`; those images were viewed but are not presented as the newest
source. The actual small-screen terminal keyboard is now separately evidenced
by the successful focused `946fa98` navigation run.

A focused `polish-navigation` entry reuses the existing search, keyboard,
settings, picker, Back and edge-Back helper plus a fresh native foundation.
Its own completion record is separate from the seven-state `polish` suite.
The successful focused execution closes the missing keyboard/navigation
evidence without relabeling the failed long-workspaces run as passed.

### Corrections and evidence limits

Actual image review prompted fixes to iOS sheet status-bar contrast, Android
native accent and 44 dp key targets, wrapping status counts, fixture foreground
handling, native pan arbitration for edge Back, and the Appearance row's spoken
label. Original failed runs remain failures. In particular, both `61050db`
polish suites completed every UI assertion but failed the old strict parser on
new fixed input diagnostics; local revalidation identified Metal without
overwriting their artifacts. The complete sequence is retained in
[PR 20](https://github.com/phni3j9a/meeterm/pull/20).

The normal iOS gate still has fourteen screens. The seven extra states and
navigation belong to the separate `polish` diagnostic with independent
completion markers and the unchanged 900-second ceiling. Fixture screenshots
verify presentation, not the remote actions that normally create that state.
Their terminal content still comes from the Rust/native demo: no terminal
bytes, cells or mock terminal renderings cross JavaScript.

Sampled recording frames from earlier runs were viewed, including native
keyboard, sheets and the SE edge-back transition. The latest focused SE run's
video capture was unavailable; its assertions and actual screenshots remain
separate evidence. Neither is full-speed playback or a frame-rate measurement.
The Reduced Motion regression verifies a mocked preference's
initial state, notifications and cleanup, not the OS setting. Physical-device
GPU, Japanese IME/font parity and rotation remain separate validation work.
Android's documented monochrome-emoji limitation remains. Simulator Metal
execution and native CoreGraphics fallback evidence are kept distinct.
