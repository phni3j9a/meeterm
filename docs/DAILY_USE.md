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

Candidate `cb69a17` passed [general CI](https://github.com/phni3j9a/meeterm/actions/runs/34548009401)
and Android's complete [fresh-CNG full flow](https://github.com/phni3j9a/meeterm/actions/runs/34550694155).
All 69 Android completion markers were present. Main viewed the foundation,
settings, created-pane and SSH terminal screenshots and verified the APK linked
in `FIRST_APP.md`.

iOS in the same full run passed fresh build, separate-runner artifact restoration,
four production storage cases, seven native input cases, SSH/reconnect, saved
credential restoration, selection/copy, settings persistence, workspace creation
and rename. It stopped during pane rename: the UI driver could not find the
Select All action for a 67-character generated pane name (`source_line=987`).
Main viewed the safe name-field failure screenshot and the settings, cleared
selection and SSH terminal screenshots. The full daily completion and final
fresh-foundation verification were not reached. Full iOS acceptance remains
incomplete; this is an observed test interaction failure, not proof that the
app's rename operation itself is broken.

The user prioritized testing-method improvement and then requested a checkpoint.
That method is implemented and documented in [TESTING.md](TESTING.md), with
Swift/Python/macOS Bash preflight, focused forms/native suites, separate iOS
build/runtime jobs and exact-source diagnostic product reuse. Focused forms
and reused native runs both passed through normal exit and artifact collection.
The standard and its evidence received independent review. Work resumed after
this checkpoint. The recorded video shows Select All already visible before the
driver's unconditional long press dismisses the menu. Candidate `c37b046` uses
the existing menu first and adds a focused names suite. Its fresh build passed,
but the first runtime stopped before XCTest while preparing the SSH fixture.
An exact-source diagnostic reuse on a new runner passed fixture setup and
executed the existing-menu branch. Workspace-name deletion was visibly empty,
but the field-value wait timed out; XCTest also failed to exit within 900 seconds.
Main viewed the safe empty-field failure image. Neither names completion nor
pane rename was reached. Candidate `791bd01` attempted one field snapshot per sample
and immediate exact readback with bounded polling. Focused names also omits
password-form switching and credential-save setup (about 290.6 seconds in that
run), while retaining real key entry, connection and host-key verification. Full
keeps those separate form/save checks. Its general CI and app build passed,
but the snapshot-based read failed at the first Host field with
`initial_value_unavailable` (XCTest exit 65 after 254 seconds). Main viewed the
pre-secret connection-form image; names operations were not reached. The next
correction `e6cd2fe` restored direct attributes and passed public-field entry
and key submission, but connection failed before host trust (XCTest exit 65,
about 530 seconds). Main viewed the failure image: the profile label was "12",
while earlier public-field readbacks had matched. Whether the host changed or
the profile name received unintended input is unproven. Names was not reached.
Candidate `ae54875` confined immediate polling to empty checks and restored
the original nonempty wait. All pre-submit public-field comparisons passed,
and host fingerprint verification/trust succeeded, but authentication then
failed before Connected. Main viewed the safe authentication-error image;
names operations were not reached in that run. Failure-only diagnostics were
added to compare saved profile metadata with the fixture and check ordinary SSH auth,
without uploading metadata/credentials or converting UI failure into success.
Candidate `bb64bae` then passed [fresh names](https://github.com/phni3j9a/meeterm/actions/runs/34568232442):
real SSH authentication and all workspace/pane create, rename and confirmed-close
operations completed. Both existing Select All branches and empty-field waits
passed; `names_complete`, fresh names success and XCTest exit 0 were present
(471.4 seconds including runner overhead). Main viewed all five safe checkpoints.
No authentication/input code changed for this run, so the earlier intermittent
authentication failure is not claimed fixed. Failure-only metadata diagnostics
were not invoked on that successful run.
The subsequent [fresh both/full run on `516380f`](https://github.com/phni3j9a/meeterm/actions/runs/34570866987)
passed Android completely (69 markers; Main viewed four images and verified the
new APK in `FIRST_APP.md`). iOS passed fresh build, storage four and input seven,
but authentication failed again before Connected (XCTest exit 65). This time the
failure-only diagnostics ran successfully: saved metadata matched every expected
field except username, and the strict fixture SSH probe passed. Main also viewed
the authentication-error image and the pre-secret password form showing the
expected username. The mismatch between native field readback and submitted
React state is the strongest explanation; the exact event-loss mechanism is
not established. The next driver-only change paces Username from the first
attempt using the existing character/prefix checks. It does not change the
product, key input, retry count or deadlines. Fresh iOS/full remains required;
Android evidence stays on `516380f`, whose Android inputs are unchanged.
The [fresh iOS/full run on `b94ef2b`](https://github.com/phni3j9a/meeterm/actions/runs/34574408722)
passed authentication, real terminal input, disconnect/reconnect and same-pane
restoration. Failure diagnostics now show every saved metadata field matching,
including username, and the strict SSH probe passed. It then failed waiting for
the first keyboard letter after reconnect (XCTest exit 65, 1000.7 seconds).
Main viewed the initial keyboard, input and disconnected images; no image of
the failing keyboard state was captured, so missing keyboard versus layout or
accessibility-query state remains unresolved. A failure-only driver diagnostic
now records fixed keyboard/surface visibility and hittability flags and captures
a terminal image only when foreground, the connection form is gone and the
terminal exists. Input actions, retries and deadlines remain unchanged. Another
fresh iOS/full run is required; daily completion and final foundation are pending.
Run links and the earlier failure details are in `TESTING.md`.
The nine-feature goal is not marked complete.
Earlier evidence is preserved in the [validation history](evidence/daily-use-validation-history.md).

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
