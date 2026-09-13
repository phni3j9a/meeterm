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

TypeScript, sixteen app selection/presentation tests, and the Python driver
regressions pass locally. These checks do not prove rendering or motion.
The Reduced Motion hook test mocks the platform preference boundary; it verifies
initial state, change notifications, and cleanup, not the actual OS setting.
Android CNG generation also passed locally; its generated day/night colors and
`AppTheme` references were inspected. Generated native directories stay ignored.

The first source (`89a3113`) ran in [Mobile smoke 34750219346](https://github.com/phni3j9a/meeterm/actions/runs/34750219346).
iOS `standard` passed its original fourteen screens, storage/input cases, and
fresh foundation with a Metal frame. All fourteen images plus the foundation
were actually viewed. Android built and reached real SSH, saved-credential
reconnect, and foreground/restart checks, then failed at `daily_settings_theme`:
the native dialog displayed `LIGHT`, while the test expected `Light`. Its
recording and screenshots were inspected. That run is not an Android full pass.
The Herdr workspace observational capture also still expected removed count
subtitles; the updated check requires the exact two workspace rows instead.

Image review found the iOS dark-presenter/light-sheet status-bar mismatch and
crowded Android shortcut targets. The subsequent fixes require new mobile
evidence. Android monochrome emoji remain the documented renderer limitation;
this UI work does not change terminal rasterization or claim color-emoji parity.

The next source (`d6da311`) passed the fresh iOS `standard` job in
[34752962390](https://github.com/phni3j9a/meeterm/actions/runs/34752962390), again
with Metal. All fourteen images and the foundation were viewed; the sheet's
status bar now remains legible over its dark presenter. Its short SSH run
[34753879182](https://github.com/phni3j9a/meeterm/actions/runs/34753879182) passed
real native input, remote acknowledgment, and disconnect; both images were viewed.
General CI [34752962383](https://github.com/phni3j9a/meeterm/actions/runs/34752962383)
passed, including SHA-verified Herdr 0.9.0 over the isolated russh endpoint.

The exact-source SE/XL `polish` diagnostic
[34753879206](https://github.com/phni3j9a/meeterm/actions/runs/34753879206) failed
at edge-back (Swift line 600). Its seven states, search, native keyboard,
settings, picker, and explicit Back checks ran first. All seven images and
sampled navigation frames were inspected. The terminal's unrestricted pan
recognizer consumed horizontal motion; the following source adds a narrow
native gesture policy and an eighth native regression without removing the
existing seven input checks. The new gesture policy still needs mobile evidence.
The partly keyboard-covered duplicate clear action was removed, and status
counts now use a nonbreaking space to prevent an orphaned number.

Source `d0fafc1` passed Android full in
[34755069513](https://github.com/phni3j9a/meeterm/actions/runs/34755069513) and
the real iOS SSH round trip in
[34756002275](https://github.com/phni3j9a/meeterm/actions/runs/34756002275).
General CI [34755069516](https://github.com/phni3j9a/meeterm/actions/runs/34755069516)
passed, including 79 Rust unit tests, real OpenSSH, and the SHA-verified Herdr
0.9.0 integration (20.58 seconds).
Its iOS standard storage four and native input/gesture eight passed, but the
screen loop stopped at terminal readiness after five captures (Swift line 508;
the foreground assertion at line 506 passed). This is not a standard pass.
A focused regression reproduced a fixture mounted while iOS is inactive never
following the later foreground notification. The subsequent fix keeps UI
lifecycle observation active without invoking native connection effects;
normal production events still reach Rust in order. This demonstrates that
specific defect, not the cause of every preceding runner failure.

The same-source SE/XL polish run
[34755999408](https://github.com/phni3j9a/meeterm/actions/runs/34755999408)
captured seven states, then failed waiting for the keyboard at Swift line 569.
Sampled recording frames show the accessory appearing briefly and disappearing;
edge-back was not reached. Follow-up public-screen diagnostics capture the
failed screen and fixed existence/hittability/geometry fields. An explicit test
launch argument enables native focus/window/binding booleans only: no typed
text, composition, clipboard data, or remote identity is logged. Real SSH/forms
failures do not enter the public-screen capture path.

App source `61050db` passed Android full in
[34757079947](https://github.com/phni3j9a/meeterm/actions/runs/34757079947).
All 21 presentation images and real daily-use/input/selection images were viewed.
General CI [34757079945](https://github.com/phni3j9a/meeterm/actions/runs/34757079945)
passed, including the SHA-verified live Herdr test (20.57 seconds).
The fresh iOS standard and same-source short SSH
[34758750427](https://github.com/phni3j9a/meeterm/actions/runs/34758750427)
ended before their UI setup/stage records. Storage four passed in standard;
these runs do not establish UI/input acceptance. Their quiet raw-log classifiers
did not identify a cause. The next driver keeps normal private runner output and
extracts only fixed classifications/counts/system codes from a local result summary.

Both same-source polish diagnostics completed every UI assertion: the default
[34758749180](https://github.com/phni3j9a/meeterm/actions/runs/34758749180) and
SE/XL [34758747669](https://github.com/phni3j9a/meeterm/actions/runs/34758747669).
These cover search, native keyboard show/hide, settings, picker, explicit Back,
edge Back with preserved search, and fresh-process survival. All seven states
and the foundation were viewed on each size. Sampled SE recording frames show
the sheet and back transitions, but recording started after the keyboard check;
it is not full-flow playback or frame-rate measurement.

Both jobs then failed the foundation log parser: it treated the newly added
fixed input diagnostics as malformed readiness/frame markers. A regression
reproduced this failure before the parser fix. The updated parser accepts only
the four exact diagnostic shapes without counting them as foundation evidence;
missing frames, malformed values, and unknown markers still fail. Read-only
revalidation of both original bundles identifies Metal; their original failed
CI reports remain unchanged. The next polish driver also captures the actual
keyboard and completed edge-back states so those visual checkpoints do not
depend on successful video startup.

The subsequent accessibility review found the Appearance row still announced
the obsolete "Terminal theme" label. The real Settings form regression
reproduced it; the label and Android driver now use "Appearance". This changes
app source, so the in-progress diagnostic-only build was cancelled and the
next source requires both mobile jobs again, plus iOS SSH/polish diagnostics.
That source, `55fb60d`, passed both normal gates in
[34760570576](https://github.com/phni3j9a/meeterm/actions/runs/34760570576):
Android full and iOS standard (four storage, seven input plus one gesture case,
fourteen screens, fresh Metal foundation). All 21 Android states, its actual
daily-use checkpoints, and all fourteen iOS screens plus foundation were viewed.
The same-source [default polish](https://github.com/phni3j9a/meeterm/actions/runs/34761931479)
also passed all interactions and the corrected strict foundation parser.

The [SE/XL polish](https://github.com/phni3j9a/meeterm/actions/runs/34761930612)
failed before UI setup; its new sanitized result summary identifies a runner
initialization timeout. No image or UI acceptance is claimed for that run.
The [short SSH run](https://github.com/phni3j9a/meeterm/actions/runs/34761932309)
reached the connected terminal, typed the keyboard prefix, and tapped Paste,
but failed waiting for the native completion value. Return was not reached.
The test now addresses the existing `terminal-paste` identifier instead of an
arbitrary same-label action, verifies its initial state, and retains the same
completion timeout and remote acknowledgment. Fixed-state diagnostics and
post-auth fixture echo booleans now also cover a Paste timeout. The underlying
cause remains unconfirmed until the next fresh iOS test build supplies evidence;
no app or native input behavior was changed for this diagnostic.

Physical-device GPU/IME parity, OS Reduced Motion behavior, and measured frame
performance remain unverified; no Simulator result replaces those boundaries.

The normal `standard` gate remains fourteen screens. Seven additional states
and navigation are a separate explicit `polish` diagnostic with independent
completion markers and the unchanged 900-second ceiling. The first run used
786 seconds for the original standard/storage scope; additional UI diagnostics
must not consume its remaining headroom or remove its assertions.

The iOS standard and Android fixture drivers now wait for the actual English
labels. Required assertions and completion checks have not been skipped or
replaced with delays.
