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
Android native tests and TypeScript must pass. Under the user-approved policy
of 2026-09-11, Android full and iOS standard validate a fresh CNG build and
their screenshots must be downloaded and actually viewed. iOS standard combines
production storage/input tests, seeded production-screen images and real native
foundation gates. A separate short SSH round-trip validates connection/input.
The long iOS full is optional; seeded images do not establish end-to-end behavior. An independent
review follows integration. Physical-device-only claims require device evidence.

### Accepted candidate under the revised policy

Commit `b82c226e31c9faf32538c4a11f90570b9aea053f` satisfies the user-approved
standard/short-SSH policy. The nine improvements are implemented, and the
agreed Hosted checks and independent code review have completed.

| Verification | Evidence | Result |
| --- | --- | --- |
| Shared/fast CI | [push](https://github.com/phni3j9a/meeterm/actions/runs/34590304087), [PR](https://github.com/phni3j9a/meeterm/actions/runs/34590307569) | All jobs passed |
| Fresh Android full | [job 103233828148](https://github.com/phni3j9a/meeterm/actions/runs/34590304287/job/103233828148) | All 69 unique completion markers; native frame/process checks passed |
| Fresh iOS standard | [job 103238335453](https://github.com/phni3j9a/meeterm/actions/runs/34590304287/job/103238335453) | Storage 4/4, native input 7/7, all 10 screen routes, fresh foundation passed |
| iOS short SSH | [job 103239523791](https://github.com/phni3j9a/meeterm/actions/runs/34591998968/job/103239523791) | Actual host trust, native input/remote acknowledgment, explicit disconnect passed |

Both mobile paths use the same source commit. iOS SSH reused the pristine
products from fresh build run `34590304287`; it did not perform another CNG/build.
The original commit/toolchain/architecture/hash checks remained in force.
Its metadata retains the existing `reused-exact-source-diagnostic` label;
the policy permits this same-source reuse to validate the separate SSH suite.

Main downloaded the artifacts and actually viewed all ten `standard-*.png`
images plus the fresh iOS `terminal.png`, both SSH checkpoint images, and the
Android foundation, SSH terminal, settings and created-pane images. Screen
presentation, Japanese text and native terminal rendering were visible. The
fixture images establish presentation, not actual save/create/rename operations.
The iOS foundation reported Metal on the same process as native readiness and
survived a 10-second foreground observation. This is Simulator evidence, not
physical iPhone GPU/IME/font parity.

The iOS fresh build took 17m13s. Standard XCTest took 65.4s for production
storage and 288.9s for native input/screens/foundation (about 5m54s total).
The separate SSH XCTest took 522.1s (about 8m42s). Build and Simulator setup
are excluded from those XCTest times. Each new suite stayed within its 900s budget.
The standard run exercised the exact QuickPath prompt/Continue/dismissed branch
before the workspace-name image, which shows the normal keyboard.

The standard artifact also contains an XCTest diagnostic without a source location
(`source_line=0`, `category=unknown`). Its type/cause cannot be recovered from
the sanitized bundle; xcodebuild exited 0 and all required markers passed.
Independent evidence review found no material visual defect or reason to rerun.

The current Android APK and verified checksum are in [FIRST_APP.md](FIRST_APP.md).
The old full clipboard-observer timeout below remains unresolved. Actual iOS OS
clipboard contents, physical-device Japanese IME/font/GPU behavior and TestFlight
remain unverified; the accepted reduced suite does not claim those results.

### Previous checkpoint

The previous user-requested checkpoint followed the
[fresh iOS/full run on `b9c4e1d`](https://github.com/phni3j9a/meeterm/actions/runs/34581965584).
The nine improvements were implemented, but the then-required full iOS acceptance was incomplete.
Both general CI runs and the fresh iOS build passed. No further code changes or
mobile reruns were started after collecting this result.

| Evidence | Result and scope |
| --- | --- |
| [Android full / `516380f`](https://github.com/phni3j9a/meeterm/actions/runs/34570866987/job/103172478780) | Passed all 69 completion markers. Main viewed four images and verified the evaluation APK in `FIRST_APP.md`. Android app/native/workflow inputs are unchanged by subsequent iOS-driver/documentation changes. |
| [iOS names / `bb64bae`](https://github.com/phni3j9a/meeterm/actions/runs/34568232442) | Fresh build and real SSH workspace/pane create, rename and confirmed-close passed, including both Select All branches and empty-field waits; XCTest exit 0. Main viewed five images. This focused result does not establish full acceptance. |
| [iOS full / `b9c4e1d`](https://github.com/phni3j9a/meeterm/actions/runs/34581965584/job/103212576212) | Storage four, native input seven, authentication, real terminal input, disconnect/reconnect and cold restart with saved credentials passed. Copy-result validation failed because its host command timed out. Daily completion and final fresh foundation were not reached. |

The failure was `daily_selection_copy_result_rejected` at source line 612:
`ios-native-copy-validation.txt` reports `reason=command_timeout`. The UI test
exited 65 after 831.5 seconds, within its 1665.9-second execution budget. This is
a clipboard-observer command timeout, not evidence of a copied-text mismatch or
an overall XCTest timeout. Its cause has not been investigated at this checkpoint.
Main viewed the normal terminal keyboard, reconnected terminal and Japanese
selection images. The copy action was tapped and its control disappeared, but
the clipboard result itself was not verified. The daily video was unavailable
because capture failed; the final foundation image was not reached.

Saved metadata again matched all fixture fields and the strict SSH probe passed.
The prior username and reconnect-key failures did not recur in this run. The
QuickPath prompt/Continue/dismissed stages were absent, and the initial image
shows a normal keyboard. Thus the new introduction-dismissal branch was not
exercised; its successful execution is not claimed.

The clipboard observer timeout remains unresolved and belongs to optional
full diagnostics under the revised policy. OS clipboard behavior must not be
reported as verified from this failed observation. The original nine-feature
goal was not marked complete at this checkpoint. OS evidence has separate
source commits and runs; this is not a same-commit both-OS pass.
See [TESTING.md](TESTING.md) for the standard method,
[testing-method history](evidence/testing-method-validation-history.md) for its
measured results, and [earlier daily-use evidence](evidence/daily-use-validation-history.md)
for the preserved investigation history.

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

## Validation limits

Hosted emulator and Simulator results establish the tested native/UI paths;
they do not establish physical-device GPU, font fallback or Japanese IME parity.
Earlier Pixel 3 testing in this milestone verified settings persistence across
restart and light/dark modal appearance. The latest complete daily-use flow has
not been run on that physical device, and no physical iPhone was available.

Same-process background/foreground return was exercised on Android; iOS
Simulator evidence covers cold restart and saved-credential reconnect. The
shared AppState bridge and Rust foreground-aware retry policy have separate
coverage, but iOS same-process return and physical-device background network
behavior have not been demonstrated. Shared profile-management UI is exercised
on Android; iOS covers production storage mutations and the real save/restart/
reconnect boundary, rather than repeating every profile-management UI operation.

Real Vim reconnect and fresh-owner recovery are covered by the shared Rust SSH
integration test. Arbitrary full-screen TUIs are not guaranteed to reconstruct
perfectly after process death; the explicit redraw action is available. Android
emoji use monochrome glyphs, with color emoji and exhaustive coverage still
outside the verified rendering scope. See [the usage guide](FIRST_APP.md) and
[SSH recovery boundaries](SSH.md) for details.

## Investigation history

Earlier build, storage, input and UI-driver findings, focused reproductions,
and their original run links are preserved in the
[validation history](evidence/daily-use-validation-history.md).
