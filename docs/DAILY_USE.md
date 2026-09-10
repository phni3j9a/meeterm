# Daily-use milestone

The user authorized all nine daily-use improvements after validating the PoC.
The canonical product and native data-plane invariants remain unchanged.

## Acceptance

- Saved server profiles: create, edit, remove, switch and reopen after restart.
- Optional credentials stored in platform secure storage, never ordinary settings
  or logs; saved credentials load natively when connecting.
- Create, rename and close tmux windows/panes through explicit typed targets,
  with destructive confirmation in the app.
- Rust-owned bounded automatic reconnect after transport loss and foreground
  return, with explicit disconnect cancelling retries and trust/auth failures
  stopping automatic retries.
- Reconstruct useful TUI screens after reconnect/process restart, exercise real
  full-screen applications, and provide an explicit redraw recovery action.
- Native terminal selection, range adjustment and clipboard copy on both OSes.
- Native Ctrl/Alt combinations and useful navigation keys, including external
  keyboard input, with shared Rust encoding and local IME composition.
- Configurable bounded scrollback and documented reconnect retention semantics.
- Persisted font size, terminal theme and history settings; deterministic resize.

## Control contract for this milestone

One existing connection owner remains active at a time. Profiles are local client
metadata, never a second source of truth for tmux topology. New low-frequency APIs:

```ts
type ServerProfile = {
  id: string; name: string; host: string; port: number; username: string;
  authMethod: 'publicKey' | 'password'; credentialSaved: boolean;
};
type SavedCredential =
  | { authMethod: 'publicKey'; privateKey: string; passphrase: string }
  | { authMethod: 'password'; password: string };
type TerminalPreferences = {
  fontSize: number; // 10..24 points, default 15
  theme: 'system' | 'light' | 'dark';
  scrollbackLines: number; // 1000..50000, default 10000
  automaticReconnect: boolean; // default true
};
getProfiles(): Promise<ServerProfile[]>;
saveProfile(profile: Omit<ServerProfile, 'credentialSaved'>,
  credential: SavedCredential | null, keepCredential: boolean): Promise<ServerProfile>;
deleteProfile(profileId: string): Promise<void>;
connectProfile(terminalId: string, profileId: string): Promise<void>;
getPreferences(): Promise<TerminalPreferences>;
setPreferences(preferences: TerminalPreferences): Promise<void>;
setForeground(terminalId: string, foreground: boolean): Promise<void>;
setAutomaticReconnect(terminalId: string, enabled: boolean): Promise<void>;
createWorkspace(terminalId: string, name: string): Promise<void>;
renameWorkspace(terminalId: string, windowId: string, name: string): Promise<void>;
closeWorkspace(terminalId: string, windowId: string): Promise<void>;
createPane(terminalId: string, windowId: string): Promise<void>;
renamePane(terminalId: string, paneId: string, name: string): Promise<void>;
closePane(terminalId: string, paneId: string): Promise<void>;
refreshTerminal(terminalId: string): Promise<void>;
```

Empty profile IDs request a new native-generated UUID. A null credential with
`keepCredential=false` removes any saved credential; `true` preserves it only
when endpoint, username and auth method still match. No API returns credentials
to JavaScript. `TmuxPane` gains `paneName`. The native terminal view gains
`fontSize`, `theme` (`light` or `dark`) and `scrollbackLines` props. Preferences
are applied to the Rust terminal registry, including hidden panes.

## Verification

Implementation is present. Local deterministic tests, real SSH/tmux integration,
Android native tests and TypeScript must pass. Both Hosted mobile jobs must run
from fresh CNG output; their screenshots must be downloaded and actually viewed.
New workflows need interaction evidence, not just a first frame. An independent
review follows integration. Physical-device-only claims require device evidence.

### Latest candidate status

Candidate `f8f77b0` passed all [general CI jobs](https://github.com/phni3j9a/meeterm/actions/runs/34472077011)
and the complete Android job in [fresh-CNG Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34472071201).
Main downloaded and viewed its Android foundation, settings, selection and
SSH-terminal screenshots, and verified the APK bundle and checksum linked in
`FIRST_APP.md`. iOS passed its unsigned native build, entitlement/source-isolation
checks, four production storage cases and seven native-input cases. The host-side
native-copy observer passed; Main viewed the actual cleared selection, settings
(font size 18, history 20000) and resized light terminal screenshots. The test also
verified saved settings after reopening, created and renamed a workspace, and
created another pane. It then failed during the pane-name field's clear operation
(`MeetermSmokeUITests.swift:868`, last stage `fill_workspace_or_terminal_name_clear`).
The failure diagnostic observed an empty field after its five-second wait had
returned unsuccessful. Main viewed the interaction recording: the long original
pane name was still being erased between 150 and 165 seconds and was empty by
170 seconds. The test sent one Delete for every character; the recording shows
that long sequence being processed, but does not isolate an internal RN delay.
The next driver revision uses Select All and one Delete for the name field while
retaining the bounded exact-empty check.
The remaining pane rename/close, workspace close and final fresh-foundation gates
are still pending. The recorded Metal markers belong to earlier runtime activity;
this failed run does not establish the final fresh-foundation acceptance gate.

### Previous candidates

Candidate `b2e2b81` passed all [general CI jobs](https://github.com/phni3j9a/meeterm/actions/runs/34465426353)
and the complete Android job in [fresh-CNG Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34465422908).
Main downloaded and viewed its foundation, settings, selection and SSH-terminal
screenshots, and verified the APK bundle and checksum now linked in `FIRST_APP.md`.
iOS passed the unsigned build, entitlement checks, all four production storage
cases, host trust, SSH input/reconnect and saved-credential cold restart. Main
viewed its saved-server, reconnected-terminal and native Japanese range-selection
screenshots. The UI suite then exhausted its shared 30-minute deadline near the
copy verification, after `daily-selection.png` was captured. The last stage
marker is `daily_selection_content_await_remote_marker` (390,541 ms); that helper
only records a stage after Return and does not itself wait for a remote marker.
The later selection screenshot proves progress beyond that marker. Main also viewed the 135.94-second recording: selection is visible at 21–22
seconds, the highlight and Copy bar are cleared at 23 seconds, and at 24 seconds
iOS asks whether `meetermTests-Runner` may paste from `meeterm`. That alert remains
through the final frame. The UI runner's synchronous `UIPasteboard.general.string`
read after Copy is therefore the blocking boundary. Native Copy and selection
clear are observed; the runner's clipboard-content assertion did not finish.
The next test revision observes the Simulator clipboard from the host, compares
it in memory and returns only a per-run fixed result to XCTest. No product
clipboard or permission behavior changes. iOS settings/CRUD/final foundation
gates remain pending. This timed-out run did not produce the native-input
validation file, so it does not establish the seven input-case results from
prior runs.

Candidate `180c525` passed all [general CI jobs](https://github.com/phni3j9a/meeterm/actions/runs/34459894012)
and the complete Android job in [fresh-CNG Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34459888911).
Main downloaded and viewed its foundation, settings, post-Copy and SSH-terminal
screenshots. Its evaluation APK has since been superseded by the candidate above.
The iOS unsigned build, four production storage cases and seven native-input
cases passed again. This time the UI test stopped earlier, at `await_connected`
after the host-trust tap; Main viewed the actual host-trust screenshot. The
connection-state artifact was captured before the tap and does not establish
the failure-time state. The cause is not established by this artifact bundle.
The next driver revision adds a bounded trust-button hittability check and
sanitized post-trust failure observations, retaining the connection success gate.
The cold saved-profile lookup also uses the existing modal-row test identifier
to avoid ambiguity with the home quick-connect row. Complete iOS daily-use and
final foundation runtime acceptance remain pending.

The [complete Android daily-use fixture](https://github.com/phni3j9a/meeterm/actions/runs/34453281079)
passed using the `8c35463` APK and the reviewed `54cb4f4` driver (diagnostic
checkout `928e3eb`). This includes profile edit, a second saved server, switching
in both directions, delete cancellation and confirmation, HOME/foreground return
with the same app PID and remote shell, cold credential restoration, settings,
workspace/pane CRUD, exact selection/copy/paste, PC handoff and final reconnect.
Both foundation and full fixture exited successfully; the final input counters
reported zero unobserved bytes and zero rejected commits. Main downloaded and
actually viewed the saved-server, PC-handoff, selection, post-Copy, settings,
SSH terminal and native foundation screenshots. Immediate post-Copy capture can
still precede the clearing frame; the focused settled-capture evidence below
establishes clearing on the same APK.

The 178-second Android recording was also inspected: it covers settings edits
and reopening, light terminal/glyph stress, workspace creation/rename and pane
creation/rename. It ends while preparing the selection fixture, before selection,
copy/paste and confirmed closing. Profile management and HOME/cold restart occur
before recording starts. Those later/earlier operations are supported by the
interaction gates and available screenshots, not by this video. No obvious
layout, keyboard overlap or transition defect was observed in the recorded range.

Candidate `c16821c` passed all [general CI jobs](https://github.com/phni3j9a/meeterm/actions/runs/34455886338).
Its [fresh-CNG Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34455886343)
passed the complete Android job, including all the daily-use gates above. Main
viewed eight new Android screenshots: foundation, saved servers, settings, SSH
terminal, selection, post-Copy, PC handoff and CJK atlas stress. The evaluation
APK from this successful build has since been superseded by the candidate above.

On candidate `c16821c`, iOS passed CocoaPods integration, source/link isolation, the unsigned app/test
build and both Keychain entitlement-section checks. All four production storage
cases and all seven native-input cases actually passed. The UI flow also passed
host trust, SSH connection, workspace/pane switching, native input and reconnect
with input to the original shell. Main downloaded and viewed the workspaces,
terminal input and reconnected screenshots; sanitized logs report Metal frames
in this real SSH interval. The final fresh-foundation gate was not reached.

The iOS UI test then failed at `daily_cold_restart` with `multiple_matching`.
The cold home exposes two buttons labelled `Saved servers`; a query created
before those buttons load can become ambiguous. This is consistent with the
failure category, although the exception did not include an exact source line.
The updated driver targets a single matching button and adds stage-specific
diagnostics. Cold
credential restoration, iOS daily settings/CRUD/copy and the final foundation
runtime gate remain pending. This is not a full iOS or release acceptance claim.

Candidate `8c35463` passed all jobs in [general CI](https://github.com/phni3j9a/meeterm/actions/runs/34450226608).
Its [Android mobile job](https://github.com/phni3j9a/meeterm/actions/runs/34450222434)
passed native readiness/frame, saved-credential cold reconnect, settings,
CJK atlas stress, workspace/pane create/rename/confirmed close, and the exact
`COPY29F7` native selection/copy/paste gate. Main downloaded and viewed the
selection, post-Copy, created-pane, settings, servers, CJK atlas, foundation and
SSH keyboard screenshots. The post-Copy screenshot still shows the highlight;
clipboard success alone does not establish immediate visual selection clearing.
A [focused follow-up](https://github.com/phni3j9a/meeterm/actions/runs/34453461196)
using the same APK then passed. Main viewed the selected frame, the post-Copy
frame with its clipboard overlay, and a further frame after five seconds without
input. Both later frames show the selection cleared. This supports screenshot/
render timing as the source of the earlier observation, not a persistent clear
failure. No native implementation change was needed for this result.
A [separate focus probe](https://github.com/phni3j9a/meeterm/actions/runs/34452697597)
passed Esc followed by 19 literal input bytes without retapping the terminal:
all 19 were accepted with zero unobserved bytes or rejected commits, and Main viewed the
exact `FOCUS_ASCII_7B` frame with unchanged terminal bounds.
The later PC-handoff interaction stopped at `handoff_action (ui_timeout)`.
The failure image shows the longer server menu with the handoff item below the
viewport; the driver had not scrolled that menu. This requires a driver fix,
not a claim that the complete Android smoke passed.

iOS passed actual CocoaPods integration, app/storage source isolation and storage
Swift compilation, then failed when linking the test bundle: CocoaPods inherited
`-l"meeterm_core"` without the production Pod library path. The app itself linked
successfully. A CI-only helper now removes that exact inherited Rust link from
the two generated storage configurations; the app host retains ownership of the
production module and Rust library. The source/link gate verifies both boundaries.
All six local iOS CI-helper regressions pass; production storage and UI execution
still require a new Hosted run. A separate acceptance audit identified two missing mobile
interaction paths: profile edit/delete/active switch and OS background/foreground
return. Both have been added before credential-free recording starts, together with
the handoff scroll fix. Main's 102 Python regressions and independent review
pass; [the reused-APK full fixture](https://github.com/phni3j9a/meeterm/actions/runs/34453281079)
has now passed these paths, as recorded above. Completion still requires the new iOS mobile gates and actual image review.

#### Previous candidate and its focused reproductions

Candidate `5795dfa` passed all jobs in [general CI](https://github.com/phni3j9a/meeterm/actions/runs/34444868633).
Its [Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34444865747)
again passed Android native readiness/frame, saved-credential cold reconnect,
persisted preferences, CJK atlas stress/reset, and workspace/pane creation and
rename. The copy equality gate failed at `daily_terminal_selection`.

Main downloaded and viewed the Android selection/cleared, created-pane, atlas,
settings, server and foundation screenshots. The updated native surface locator
and long press now produce the blue selection rectangle over the first eight
characters. However, the displayed row is `/bin/sh: 2: print: not found`, not the
expected `COPY29F7`: the generated setup command contains `printf`, but the remote
shell received `print`. The same selection rectangle remains after the Copy
attempt. Input diagnostics show 603 attempted bytes, 582 observed accepted bytes
and zero explicit native rejections. These observations do not identify whether
IME handling, input delivery or tap routing caused the remaining failures.
A [focused local-terminal Android probe](https://github.com/phni3j9a/meeterm/actions/runs/34448989559)
subsequently showed a real Android clipboard overlay containing `scrollback-hist`
and pasted exactly that selected text into the terminal; Main downloaded and
viewed both images. Its Python sequence completed, but the workflow wrapper
failed afterwards, so the overall job is not reported as passing. The selected
row was existing demo history, not the intended marker: clearing a viewport does
not clear scrollback, and expanding the viewport can reveal older rows.
The probe's three literal-input cases observed all attempted bytes accepted.
These observations do not establish the cause of the earlier missing `f`.

Static review found that four toolbar listeners requested focus on their own
TextView instead of the native terminal editor. The fix explicitly targets the
outer terminal view. The daily driver keeps the focused IME open while preparing
the marker and retrieves Copy's bounds after the screenshot. Independent review,
the Android arm64 Release build and 24 native unit tests passed locally. The
updated APK subsequently passed the focused Hosted input check and full daily
fixture recorded above. The separate
fixture-preparation gate now requires a fresh command-success acknowledgment
and an exact visible row before selection; its three new regression cases pass
within the full 95-test Python suite, and independent review is clear.

The iOS job passed actual CocoaPods integration but failed while compiling
`ClientStoreTests.swift`: its implicit `@testable import MeetermTerminal` conflicts
with an explicit `internal import` in the Expo-generated storage-target provider.
The four production storage cases and UI/SSH interaction were not executed; no
new iOS app screenshot is available from this build failure. The local fix uses
`@testable internal import` and disables the inherited Expo autolinking manager
only on this disposable storage target. CocoaPods search-path inheritance stays
in place, and the app retains its provider. A post-pod-install gate requires
only the storage test source in the child target, one provider in the app, and
no direct native-module linker flag in the storage target. This scoped override
uses the pinned Expo target extension; actual generated-project verification
and the full storage/UI run remain required.
A previous [minimal app-hosted XCTest probe](https://github.com/phni3j9a/meeterm/actions/runs/34443153451)
passed real Keychain add/delete with status `0`; this is not a substitute for the
production app/pod tests. The integrated candidate passed independent static
review, 92 Python driver tests, three generation tests and exact-script fresh CNG
source-membership checks. The milestone and PR remain a draft while mobile
acceptance is incomplete.

## User behavior and storage boundary

The server list manages local profiles; only one server is interactive at a time.
Connecting to another saved server asks before disconnecting the current transport.
A profile can be saved without a credential. Credential saving is opt-in, and an
endpoint, username or authentication-method change invalidates the old credential.
Renaming a profile preserves its credential. Removing the profile also removes
its saved credential; it does not close the remote tmux workspace.

Android stores metadata and authenticated ciphertext in one atomic file under
`noBackupFilesDir`, with an AES-GCM key held by Android Keystore. The final file
is bounded to 16 MiB before replacing existing data. iOS stores only metadata
and opaque IDs in Application Support, with secrets in this-device-only Keychain
items. A small pending-deletion journal makes interrupted Keychain/metadata
updates recoverable. A failed replacement can require credential re-entry;
it must not leave an unreferenced secret that cannot later be removed.

The terminal's Ctrl and Alt controls apply to one committed key/text action.
Composition stays native; changing the bound pane cancels pending composition
and modifiers. Long press begins selection; drag adjusts the range, then the
native Copy action writes to the system clipboard. Explicit application color
sequences continue to take precedence over the default light/dark palette.

History is held by Rust in memory, with a configured limit per terminal. The
setting applies to hidden panes too. Reducing the limit discards the oldest
rows only after the preference has been saved. Process death loses the local
buffer; recovery uses the history still available in tmux, currently captured
up to 2,000 preceding lines. The app does not silently change the user's tmux
history-limit or persist terminal output to its own disk files.

## Historical evidence during implementation

The following candidate timeline is historical; the latest status above takes
precedence over its earlier pending or in-progress statements.

#### Candidate 4 and preparation for candidate 5

Remote candidate `0a11c02` passed [general CI](https://github.com/phni3j9a/meeterm/actions/runs/34436390119).
Its [Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34436387115)
passed the Android build and native first frame, saved-credential cold profile
restore, persisted preferences, CJK atlas reset, and workspace/pane create and
rename. The Android copy exact-marker gate failed, and no selection highlight
was observed in the captured image; selection/copy is therefore still open. Main viewed the latest
Android settings, server, foundation, atlas and selection-cleared PNGs and the
177.95-second video contact sheet; that video ends before the copy attempt.
The iOS build, app/test-bundle entitlement-section gate, and seven native input
cases passed. All four storage cases still failed with Keychain status -34018:
the test code runs in a separate prebuilt XCTest runner, whose executable was
not covered by that section gate. The UI test stopped at `fill_server_name_focus`;
Main viewed both captured empty connection-form images. Storage and the iOS
daily flow remain open. A minimal
[app-hosted XCTest probe](https://github.com/phni3j9a/meeterm/actions/runs/34443153451)
subsequently passed actual Keychain add and delete with status `0` in the
unsigned app process. The next candidate uses a separate `meetermStorageTests`
unit-test target hosted by the real app, importing the production
`MeetermTerminal` pod through a nested CocoaPods `inherit! :search_paths` target.
The four storage cases must pass before the UI suite starts. The minimal probe
does not establish that the production-pod integration or full daily flow passes.

Local follow-up `228d3fc` contains the exact Android native terminal surface
locator, drag-and-drop and structural diagnostics. Its local checks are 89 passing
Python tests and a successful arm64 Release build (45 seconds), with independent
review clear. Follow-up `4987cb6` moves the iOS form gesture into the outer scroll
gutter, requires the control to fit above the keyboard, and records separate
hittable/tap stages. Its focused review passed; Hosted Swift compilation and UI
interaction remain pending. The combined Python regression suite now passes
92 tests, including sequential storage/UI execution and rejection of stale or
incomplete storage success markers. Three generation tests also pass. The exact
CI injection script was run against fresh Expo CNG output and its parsed source
lists retain the seven native-input cases in the UI target while placing only
`ClientStoreTests.swift` in the app-hosted storage target. Actual CocoaPods
integration and Swift compilation still require Hosted macOS. These local
checks do not replace the Hosted mobile run.
The milestone remains a draft until the open mobile gates and both screenshot
reviews finish.

#### Earlier checks

- Shared Rust library: 57 tests passed, along with formatting and Clippy.
- Real OpenSSH/tmux integration passed, including Vim reconnect and fresh-owner
  recovery, name encoding, and closing the last pane/window.
- TypeScript and 85 Python regression tests passed, including atlas/selector checks.
- Three Simulator configuration-generation tests passed. A freshly regenerated
  Expo iOS project was also injected and parsed, confirming that the four app/test
  configurations preserve their existing linker flags and other configurations
  remain unchanged. Actual DER conversion and Keychain execution require Hosted
  macOS; the local generation tests use a stand-in conversion command.
- Android native JVM tests: 24 passed.
- Android arm64 Release built successfully and installed on the connected Pixel 3.
- Pixel 3: changed font to 18 pt, history to 20,000 and theme to light through
  the actual settings UI, saved, terminated and relaunched the app, then verified
  the persisted values. Captured a 43-second interaction recording and viewed
  its extracted frame sequence. This revealed a stale Android dialog status-bar
  appearance when switching themes. Applying the appearance after Android
  registers each modal window corrected it; the light settings screen and both
  light/dark server sheets were captured and actually viewed on the latest APK.
- Early independent storage/UI review found three material issues (interrupted
  iOS Keychain updates, destructive history application before durable settings,
  and Android store size overflow). All three were fixed and re-reviewed.
- Integration review also corrected Android modal appearance, modifier lifetime
  during local IME edits, modified F1–F4 encoding, and iOS Shift combinations.
  Android atlas exhaustion now resets bounded packing and uses region uploads;
  copy, ordinary taps and input clear the native selection. Regression tests
  cover atlas rollover and restoration of the unselected snapshot colors.
  Seven iOS native-input cases passed in both later Hosted candidates.

General [Hosted CI](https://github.com/phni3j9a/meeterm/actions/runs/34423153261)
passed on candidate `2f54449` (Rust, real OpenSSH, JavaScript, Expo and Android
native build/tests). The first [Mobile smoke](https://github.com/phni3j9a/meeterm/actions/runs/34423153361)
failed at Android's `daily_profile_save_toggle` UI lookup and at the iOS
`commitModified` Swift wrapper's `Int32`/`Bool` return mismatch. Android's native
first frame and empty password-form screenshots were downloaded and viewed;
iOS correctly emitted unavailable diagnostics because the app build failed.
The Swift wrapper is corrected. The Android driver now configures persistence before secret entry and scrolls
in the observed outer form padding. Pixel 3 public-field probes verified the
settings switches and return to the key editor. Japanese Gboard conversion is
handled before the unchanged exact-value/prefix readback gates.

The second [Hosted CI](https://github.com/phni3j9a/meeterm/actions/runs/34426022716)
passed on candidate `7246ede`. Its [Android mobile run](https://github.com/phni3j9a/meeterm/actions/runs/34426022773)
built and launched successfully, saved a profile and its optional credential,
terminated/relaunched the app, connected with the native saved credential, and
saved/reopened 18 pt, light theme and 20,000 history lines. The downloaded server,
settings and light-terminal screenshots were actually viewed. Its interaction
recording showed successful creation of the third workspace; the UI driver then
failed because it counted the new workspace-options buttons as workspace rows.
The updated selector uses a stable window-ID test identifier.

The second iOS job built the app and test bundle successfully. Seven native-input
cases passed, including hardware Shift combinations and marked-text commit.
The UI test stopped at `form_not_dismissed` after submitting the credential-saving
connection form. Its fixed diagnostics did not yet cover profile-name validation
or submission errors, so they do not establish the cause. The two empty connection/
password form screenshots were downloaded and actually viewed. This run does not
verify an iOS terminal frame or Metal execution. The next candidate adds exact
public profile-name checks, fixed submission diagnostics and separate native
storage test markers. Complete daily-use interaction runs remain pending.

The third [general CI](https://github.com/phni3j9a/meeterm/actions/runs/34430487992)
passed on candidate `747ad81`. Its Android mobile run again passed saved-credential
cold reconnect and persisted settings. The updated server/settings screens and
native CJK foundation frame were downloaded and viewed, including the explicit
2,000-line cold-history limit. The atlas smoke then stopped in the test driver:
a new call supplied a `timeout` keyword that the existing marker helper does not
accept. A regression through the actual stress orchestration reproduced the
`TypeError`; removing that keyword made the full 85-test Python suite pass.
At that point the native atlas stress and remaining CRUD/copy interaction gates
still needed a complete run. The latest Android full-fixture result above supersedes both the
atlas and copy pending claims. The third iOS job failed
at native storage; the focused reproduction below identifies missing Simulator
Keychain entitlements.

Physical full-flow Android attempts stopped before submitting the test profile
when foreground/editor observations were unavailable. They do not establish
credential restoration, selection or atlas stress on the physical device. The
public-input/settings probes above are the physical-device evidence currently
available. No physical iPhone is connected.

The Android evidence driver now discards recordings after detected foreground
loss and checks foreground before and after each screenshot. These are sampled
checks; a leave-and-return entirely between observations is not detected. The
recording starts only after authentication and cold-profile restoration, so no
credential-entry UI is intentionally recorded.

The independent integration review has no unresolved material code findings.
Updated Hosted mobile checks and complete daily-use interaction evidence are
still pending. This record is not yet a release acceptance declaration.

### iOS Simulator Keychain reproduction

The third iOS run passed all seven native-input cases but failed all four
ClientStore tests and rejected the credential-saving form submission. A
[focused unsigned reproduction](https://github.com/phni3j9a/meeterm/actions/runs/34433692618)
on the Hosted Simulator passed the same Application Support / atomic protected
file write / JSON roundtrip, but returned `-34018` from both `SecItemAdd` and
`SecItemDelete`. This is Apple's
[`errSecMissingEntitlement`](https://developer.apple.com/documentation/security/errsecmissingentitlement).
Because an interrupted credential write leaves a pending-delete journal, the
same unavailable Keychain cleanup also explains subsequent preference-read
failures. The generic UI rejection alone does not distinguish save, connection
preparation and SSH-start errors.

A [second reproduction](https://github.com/phni3j9a/meeterm/actions/runs/34434314312)
embedded Simulator entitlement XML and DER in the Mach-O `__TEXT,__entitlements`
and `__TEXT,__ents_der` sections. The app remained unsigned, launched, and
returned success (`0`) for both Keychain operations. The reproduction source is
[the isolated probe workflow](https://github.com/phni3j9a/meeterm/blob/a61c44b/.github/workflows/probe.yml).
The attempted host ad-hoc signature with iOS entitlements was launch-rejected;
that approach is not used. No distribution certificate, provisioning profile,
Apple account or signing secret is needed for the section-embedding approach.
A later app-hosted XCTest probe also returned `0` for Keychain add and delete
while remaining unsigned. The production app and pod integration still require
a complete Hosted run; entitlement sections in a UI test bundle alone do not
grant entitlements to the separate XCTest runner process.
