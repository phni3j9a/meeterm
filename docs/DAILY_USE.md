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

## Local evidence during implementation

- Shared Rust library: 57 tests passed, along with formatting and Clippy.
- Real OpenSSH/tmux integration passed, including Vim reconnect and fresh-owner
  recovery, name encoding, and closing the last pane/window.
- TypeScript and 84 Python regression tests passed, including atlas/selector checks.
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
  The iOS hardware-input test still requires the updated Hosted candidate.

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
The updated selector uses a stable window-ID test identifier. iOS validation
and the next complete Android interaction run remain pending.

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
