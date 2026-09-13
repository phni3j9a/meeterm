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

## Verification and remaining work

[Selected reviewed native screens](evidence/issue-19-ui/README.md) show the light
workspace and dark terminal on both platforms. They are actual screenshots,
not generated mockups. Full-size originals remain in the linked CI artifacts.

The latest app/native implementation with completed normal mobile evidence is
`55fb60d30d439381204f6f331c9b9579f09c09ac`. Both normal mobile gates passed in
[34760570576](https://github.com/phni3j9a/meeterm/actions/runs/34760570576).
The subsequent `fb48d0f` changes only test scripts and documentation. The current
candidate adds smoke-only startup and Paste observations; its new hosted
build/runtime verification is pending. It does not change the visual design,
renderer, dependencies or input delivery/cancellation semantics.

| Check | Actual scope | Evidence |
| --- | --- | --- |
| Android full | Native readiness/frame/no crash, real SSH and daily-use flow, settings, selection/copy, lifecycle and desktop handoff | Passed in the fresh mobile run above; all 21 observational states and actual daily-use images viewed |
| iOS standard | Four storage cases, seven input cases plus one gesture case, original fourteen screens, fresh native readiness/frame/survival | Passed in the same fresh run; all fourteen screens plus the Metal foundation viewed |
| iOS default polish | Seven public states; real search, native keyboard show/hide, settings, picker, explicit Back and edge Back preserving search | [34761931479](https://github.com/phni3j9a/meeterm/actions/runs/34761931479) passed; seven states, two actual interaction screenshots and the Metal foundation viewed |
| iOS SE/XL polish | An actual iPhone SE (3rd generation), with OS content size verified as extra-large | [34761930612](https://github.com/phni3j9a/meeterm/actions/runs/34761930612) failed before UI setup: sanitized result summary identifies a runner initialization timeout; no screenshots or UI acceptance for that run |
| iOS short SSH | Native keyboard prefix, Paste, Return, remote acknowledgment and disconnect | [34761932309](https://github.com/phni3j9a/meeterm/actions/runs/34761932309) failed at Paste completion; fresh test-only build [34763474371](https://github.com/phni3j9a/meeterm/actions/runs/34763474371) succeeded but its SSH test failed before the connection form |
| Shared Rust | 79 unit cases, real OpenSSH, pinned-SHA Herdr 0.9.0 integration through the isolated russh endpoint | [34760570515](https://github.com/phni3j9a/meeterm/actions/runs/34760570515) passed; Herdr integration completed in 20.96 seconds |

TypeScript, 24 App/Settings and startup-observation tests (including nested
cases), and 152 Python SSH/driver regressions passed locally. CI-processing regressions also
passed (20 tests, with one macOS-only case excluded on Linux). Android CNG
generation passed, including inspection of generated day/night native accent
resources. Generated native directories remain ignored.

### Open diagnostic

The failed short SSH run reached the connected terminal, typed its keyboard
prefix and tapped Paste, then timed out waiting for the native completion
value (Swift line 2040 on `55fb60d`). Return was never reached. The cause is
not yet established. The test now selects the existing `terminal-paste`
identifier rather than an arbitrary same-label action, verifies its initial
Ready state, and records only whitelisted completion/keyboard/target states.
A post-auth Paste timeout also produces read-only fixture echo booleans.
The new regression reproduced the missing diagnostic before the fix.
The completion timeout and remote acknowledgment are unchanged; no app input
behavior was modified for this investigation.

That fresh test-only run reached the app's foreground, but neither connection
entry point appeared within the existing waits (Swift line 1395 on
`fb48d0f`). No connection data had been entered and the Paste changes were
never exercised. The artifact contains no screenshot of that initial screen;
the new diagnostic candidate distinguishes startup, loading, and accessibility
state before attributing this failure to the app or the Simulator. Both general
CI runs on `fb48d0f` passed.

The candidate records only fixed startup phases and native Paste lifecycle /
accepted booleans. Optional initial and entry-failure app screenshots are
allowed only before any connection input. Fixed element flags, strict marker
validation and UTC-bounded sanitized log collection complete this diagnostic;
no timeout, assertion or remote-acknowledgment requirement is relaxed.
Local regressions cover these contracts, not actual UIKit/XCTest execution.
The fresh cross-platform build, short SSH and compact-keyboard observations
remain to be run on this source.

The newest SE/XL run stopped before the test body, not at an app interaction.
Earlier SE/XL standard passed all fourteen screens in
[34756003466](https://github.com/phni3j9a/meeterm/actions/runs/34756003466)
on `d0fafc1`; those images were viewed but are not presented as the newest
source. A small-screen screenshot with the actual terminal keyboard remains
to be gathered.

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

Sampled recording frames were viewed, including native keyboard, sheets and
the SE edge-back transition. This is not full-speed playback or a frame-rate
measurement. The Reduced Motion regression verifies a mocked preference's
initial state, notifications and cleanup, not the OS setting. Physical-device
GPU, Japanese IME/font parity and rotation remain separate validation work.
Android's documented monochrome-emoji limitation remains. Simulator Metal
execution and native CoreGraphics fallback evidence are kept distinct.
