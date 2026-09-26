# SSH and selected runtime lifecycle

The native connection authenticates an SSH host first, then discovers and
explicitly selects a runtime. For tmux, that runtime is an arbitrary session on
the remote user's ordinary tmux server; for Herdr, it is a selected running
session. Workspaces are tmux windows or Herdr workspaces, and terminals are
panes. `meeterm` is the suggested name for an explicitly created tmux session
and a legacy last-used hint, not a fixed runtime. A desktop user can continue
with `tmux attach -t <selected-session>` or the ordinary Herdr client.

## Using the session loop

1. Open **サーバーに接続** (accessibility label: **Connect**), enter the SSH endpoint and username, choose **秘密鍵** or
   **パスワード**, enter the selected credential, and submit the form. The key form also accepts an optional passphrase.
2. Verify the displayed SHA-256 host-key fingerprint through a trusted channel
   before choosing **Trust and connect**. A changed trusted key fails closed.
3. After SSH authentication, review the bounded runtime picker. It lists
ordinary tmux sessions and Herdr sessions in separate sections. A legacy or
last-used row may be highlighted, but the user must select a runtime
explicitly, even when there is only one candidate. Discovery does not create,
start, attach, or mutate anything.
4. Once the selected runtime reaches **接続中** (**Connected**), select a workspace and its terminal tabs. Existing
   windows and panes can be created or changed with ordinary remote tmux
   commands; the native core discovers the topology.
5. **切断** (**Disconnect**) closes the mobile connection while the selected runtime and
   its processes continue running. Switching server/runtime releases the current
   controller before acquiring the next one.
6. After a transport failure, the last synchronized terminal remains visible
   but read-only while Rust automatically recovers the same tmux or Herdr
   target. Herdr checks the approved SSH host/key and authentication,
   compatibility, selected runtime, original stable terminal, ordinary
   controller lease without takeover, and authoritative full frame. Missing
   server-instance identity by itself does not require confirmation. An actual
   mismatch, authentication/synchronization failure, missing runtime/terminal,
   incompatibility, or controller conflict keeps the stale work screen
   read-only. Identity mismatch, controller conflict, and retry-exhaustion/
   unknown stops offer **Retry** and **Change connection or runtime**;
   `runtimeMissing`, `terminalMissing`, and `incompatible` offer Change only.
   A changed host key offers **Review key**, and authentication failure offers
   **Connection details**. **Reconnect** in the
   Workspaces list and Server sheet appears while recovery is reconnecting or
   stopped with a retry-eligible reason, and uses `retryRecovery` for retained
   work. The first automatic retry is immediate; subsequent failures use
   bounded exponential backoff, and foreground/network-change events wake a
   sleeping retry. After the app process exits, open **サーバーに接続**
   (**Connect**) and authenticate again; a fresh manual/cold connection always
   shows the picker.

The form accepts the complete `BEGIN OPENSSH PRIVATE KEY` / `END OPENSSH PRIVATE KEY`
block, not a `.pub` key or legacy PEM block. See the [first-app setup guide](FIRST_APP.md#接続先の準備と使い方)
for key preparation and a server-side host-fingerprint check.

The disposable fixture described below is configured for public-key
authentication. To exercise password mode manually, use an OpenSSH server with
`PasswordAuthentication yes` and a password-enabled account; the app does not
turn keyboard-interactive or MFA prompts into a password flow.

The password acceptance run uses a separately provisioned disposable Docker
OpenSSH/tmux server; `fixture.py` does not provision it. Its mode-0600
`connection.env` supplies `MEETERM_SSH_AUTH=password`, `MEETERM_SSH_HOST`,
`MEETERM_SSH_PORT` (above 1024), `MEETERM_SSH_USERNAME`, `MEETERM_SSH_PASSWORD`,
`MEETERM_SSH_FINGERPRINT`, and `MEETERM_SSH_KNOWN_HOSTS_FILE`. The last file pins
the fixture's verified host key in OpenSSH known-hosts format. Source the env
file without printing it, then run the focused ignored test against this
disposable server:

```sh
set -a; . /path/to/connection.env; set +a
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh real_openssh_password_auth_reconnect_and_host_key_gate \
  -- --ignored --nocapture
```

The test covers password success, wrong-password rejection, in-process
password reconnect, and changed-host-key rejection before authentication.

The selected credential is retained in Rust process memory for reconnect. The form clears private-key, passphrase, and password text on
submission, cancellation, unmount, or authentication-method changes. Credentials
may optionally be saved through Android Keystore-backed encryption or iOS
Keychain. Saved profiles load credentials natively; no secret getter is exposed
to JavaScript. Approved host identities are stored separately in app-private
storage and checked again during reconnect. Password authentication uses only
the SSH `password` method; keyboard-interactive prompts, MFA and SSH-agent remain
outside this slice. See [DAILY_USE.md](DAILY_USE.md) for the daily-use additions.

Saved server profiles contain SSH endpoint/authentication metadata. Any legacy
backend/runtime fields represent a non-authoritative logical `lastUsedRuntime`
hint;
profile IDs and credentials remain independent, and the hint is written only
after the selected runtime reaches `Ready`. Changing or switching the hint
must not invalidate the secure credential identity.

For Herdr-specific executable resolution, stopped-session behavior, and the
running-only selection boundary, see [HERDR.md](HERDR.md).

## Native data and lifecycle boundary

After the user selects a tmux runtime, SSH opens an exec channel for tmux
Control Mode targeting that exact ordinary session. It does not allocate an
outer SSH PTY. Explicit session creation is a separate detached operation;
normal discovery and selection never use an attach-or-create command. `-CC`
attempts to configure terminal attributes and fails without a terminal on tmux
3.4; `-C` supplies the same Control Mode protocol over pipes. See the measured explanation in
[`ARCHITECTURE.md`](ARCHITECTURE.md#tmux-control-mode).

Rust parses protocol framing and octal escaping as bytes. Each `%output` pane
ID routes to one Rust-owned terminal. Native input is encoded for an explicit
numeric tmux pane target, and native view dimensions feed the Rust controller.
Terminal bytes, cells, frames, and IME composition never pass through JavaScript.

The small control API exposes connection state, topology, pane selection, and
reconnect through the existing native package and C/JNI ABI. A
`native:<terminal-id>` view identity borrows a handle from the shared Rust
registry. Platform adapters do not create a second copy of pane state. Removing
a view leaves the remote pane running; removing a remote pane invalidates its
borrowed local handle safely.

Retained **Reconnect** and **Retry** commands use the native `retryRecovery`
same-intent entry. The native `reconnect`/`ManualReconnect` entry is reserved
for fresh selection with no retained work. Automatic retry is native-owned,
not a React timer or an unbounded background retry. The first attempt is
immediate; failures use bounded exponential backoff. Foreground return and the
native network-change notification wake a sleeping retry only while foreground
and automatic reconnect is enabled; they do not interrupt a healthy connection
or reset the retry budget. React polls only low-frequency state. Native
reconstruction after a connection gap must obtain the current remote screen
and topology before accepting terminal input again. Local display contents
alone are not proof of remote reconnection.

The selected pane is temporarily zoomed with tmux's own zoom operation; its
underlying split layout is retained. Recovery uses an allocated pair of
session-scoped `client-detached` / `client-session-changed` hooks so that a
lost mobile transport or desktop handoff can undo the mobile zoom. Existing
user hook entries are preserved. The pair removes itself on recovery; no
global hook or configuration file is installed.

Zoom cleanup ownership is keyed to the selected tmux window and the current
connection generation, with the pane ID retained only as the current target.
Switching panes inside the same window therefore cannot reclassify a meeterm
zoom as a desktop-owned zoom; switching windows restores the old owned window
before acquiring ownership of the new one. A pre-existing desktop zoom is
never claimed or undone. Explicit Disconnect performs a bounded topology
readback, cleanup command, same-Control-Mode response marker, final zoom
readback, and meeterm-hook check. If the target has vanished, it is treated as
not-needed only after that authoritative readback; malformed/failed/timeout
cleanup is exposed as `layout_restore_unconfirmed` in the existing connection
error fields, while local shutdown still converges to `Disconnected` and does
not start automatic reconnect or replay input.

Screen reconstruction captures the current pane with ANSI attributes and
restores its dimensions, cursor, active alternate screen, and exposed input
modes. This is not a serialization of a running application's entire terminal
parser state. In particular, saved primary-screen contents, scroll margins,
and partially emitted escape sequences are not reconstructed. tmux 3.4 does
not expose bracketed-paste mode in its format metadata; the capture defaults
that mode off until the application emits it again. A static alternate-screen
recovery test is useful evidence, but arbitrary full-screen TUI process-death
recovery still needs application-specific validation and may require a redraw.

## Image attachment over SFTP (Issue #28)

`attachment_begin(terminal_id, local_path, display_name, size_bytes,
remote_dir)` starts one Rust-owned attachment operation against the
*currently selected* pane — at most one live operation per connection
(`pending`/`uploading`/`uploaded`/`inserted` count; `failed`/`cancelled`
records do not). The picked file stays adapter-owned and read-only for
the core; it is re-validated immediately before streaming.

The upload multiplexes a second SSH **session channel** running the `sftp`
subsystem on the already-authenticated connection — no second TCP session,
no credentials, no meeterm daemon. Channel open and the subsystem handshake
run inside the actor's serialized command queue with a bounded timeout; the
definitive `CHANNEL_SUCCESS`/`CHANNEL_FAILURE` reply is awaited on the
channel itself (`request_subsystem` only *sends* the request). Byte
streaming then moves to a detached task so a large image cannot stall the
interactive tmux/Herdr loop, and every failure is folded into the
operation's snapshot rather than failing the connection.

Remote layout: `<realpath(".")>/.local/share/meeterm/attachments/` — the
SFTP start directory is resolved server-side (no client-side `~`
assumption), every component lstat-checked and created `0700` with
symlinks rejected. A `.partial-att-*` staging file written `0600` is
published by a plain SFTP v3 rename (never an overwrite) as the generated
name `att-<id>-<millis-hex><ext>` only after `lstat` verifies the staged
bytes; the picked filename never appears remotely. An explicit absolute
`remote_dir` may override the base; it gets the same per-component lstat
walk but keeps its existing modes. Cancellation and failures remove the
partial best-effort; a verified same-endpoint final file short-circuits a
later retry without re-sending bytes.

Remote files persist until `attachment_remove_remote` runs: it deletes
only the operation's generated names (the published file, its
`.partial-*` remnant, and the app-private attachments directory when
empty) on the same authenticated endpoint, then sets the snapshot's
`remote_removed` flag while keeping the phase — an `inserted` reference
is not revoked. A user-specified `remote_dir` is never removed, only the
generated names inside it. Nothing is auto-deleted on insert, cancel,
dispose, or process exit.

`attachment_insert` is a separate explicit step: exactly one single-quoted
remote-path line through the existing `paste_utf8_at_epoch` fence. Enter is
never sent and no shell command is constructed; the user reviews and
submits the line to whatever is running in the pane. `inserted` in the
snapshot means only "the native input queue accepted the line" — it is not
a CLI or model delivery acknowledgement.

## Disposable fixture

`scripts/ssh/fixture.py` starts a temporary OpenSSH server as the current
unprivileged account, listening only on `127.0.0.1` at an ephemeral high port.
It creates disposable host keys, encrypted and unencrypted Ed25519 client keys,
authorized keys, server configuration, and a private trust store under one
mode-0700 temporary directory. It does not change `/etc/ssh`, the system sshd,
`~/.ssh`, or the user's SSH configuration, and needs no sudo.

The fixture also owns an isolated tmux default socket. `sshd` supplies a private
`TMUX_TMPDIR` to its remote commands, and wrapped local test commands receive
the same environment with inherited `TMUX` and `TMUX_PANE` removed. Cleanup
addresses only the fixture's absolute `tmux-<uid>/default` socket. This is test
isolation: the application command has no `-L` or `-S` and uses an ordinary
server on a real host.

An empty foreground `tmux -D -f /dev/null` server is started by the fixture so
tests do not load the developer's tmux configuration, key bindings, or hooks.
Only this isolated server sets `default-shell` to `/bin/sh` and
`default-command` to `exec /bin/sh -i`, preventing developer shell startup
prompts from interfering with fixture input. No managed session exists
initially: runtime discovery must report an empty tmux section without creating
anything. Tests that need a session explicitly pre-create one or exercise the
detached tmux create operation, then verify the returned identity. The fixture
does not turn a normal connect into an implicit `meeterm` session.

Prerequisites are Python 3.10+, `/usr/sbin/sshd`, `ssh`, `ssh-keygen`, and `tmux`.
The fixture refuses to run as root. Missing prerequisites are environment setup
issues; the script does not install system packages or reconfigure services.

Run the native integration target:

```sh
python3 scripts/ssh/fixture.py -- \
  cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh real_openssh_tmux_session_loop -- --ignored --nocapture
```

Run the deterministic driver regressions:

```sh
python3 -m unittest discover -s scripts/ssh -p 'test_*.py'
```

The fixture disables SFTP by default so negative-path tests exercise a
server that rejects the subsystem. `--sftp` adds `Subsystem sftp
internal-sftp` and exports `MEETERM_SSH_SFTP=1`; the two attachment
integration targets each require one mode:

```sh
python3 scripts/ssh/fixture.py --sftp -- \
  cargo test --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh real_openssh_sftp_attachment_upload_and_insert -- --ignored

python3 scripts/ssh/fixture.py -- \
  cargo test --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh real_openssh_no_sftp_attachment_fails_visibly -- --ignored
```

The positive target verifies the full upload (byte equality, `0600` file /
`0700` directory modes, generated names under
`.local/share/meeterm/attachments`, rename publish), the one-live-op
rejection, stale-destination insert rejection and reselect-then-insert
recovery, single-quoted no-Enter input, explicit remote deletion with the
phase kept, and cancel/dispose cleanup. The negative target verifies the
`sftp_unavailable` failure is an attachment-level state that leaves the
interactive connection `Ready`.

The Rust integration target exercises the real SSH/tmux/native-terminal path.
Assertions cover explicit trust and encrypted-key authentication, pane-specific
output and input, remote dimensions, topology changes, disconnect and resume,
and rejection of input while disconnected. Recovery assertions use remote
process state and native snapshots, not just the presence of a connection flag.
The four-pane test also covers ordinary desktop attach/detach, preservation of
preexisting indexed user hooks, remote pane removal and borrowed-handle
invalidation, alternate-screen capture, and post-reconnect no-wrap output at
the right margin. It checks wrong-passphrase and changed-host-key rejection.
A fixture passing does not establish physical-device parity.

Issue #21 adds a separate lifecycle boundary to this fixture coverage:
discovery must be read-only and bounded; tmux tests cover no-session and
multiple-session listing, explicit detached create/select, exact identity, and
selection races; Herdr tests cover PATH/known-location resolution, running and
stopped rows, running selection revalidation, and backend-local errors. The
same source must also test profile hint migration, verified reconnect and
same-name replacement, switch/release, and fail-closed linked/shared tmux
workspace/final-pane mutations. These are acceptance expectations, not claims
that the existing legacy integration command above already covers them.

## Mobile fixture

To keep the fixture running for manual use, create a fresh environment file:

```sh
fixture_env="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/meeterm-ssh.env"
python3 scripts/ssh/fixture.py --env-file "$fixture_env" &
fixture_pid=$!
trap 'kill "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true' EXIT INT TERM

while ! test -s "$fixture_env"; do
  kill -0 "$fixture_pid" 2>/dev/null || exit 1
  sleep 0.1
done
. "$fixture_env"
```

The environment file contains transient test credentials. Do not print or upload
it, the key files, or an unsanitized authentication form. The fixture refuses to
overwrite an existing environment file and removes its own file during cleanup.
SIGKILL or host failure cannot execute cleanup; these exceptional cases require
ordinary temporary-directory cleanup by the owner.

For an attached Android device, forward the fixture's loopback port:

```sh
adb -s "$MEETERM_ANDROID_DEVICE" reverse tcp:"$MEETERM_SSH_PORT" tcp:"$MEETERM_SSH_PORT"
```

Enter the fixture values in **Connect**. iOS Simulator can use the host's
loopback endpoint directly. Host fingerprints still require explicit approval.
The environment exposes `MEETERM_SSH_PRIVATE_KEY_FILE` and
`MEETERM_SSH_PASSPHRASE` for encrypted-key tests, and
`MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE` for unattended Android UI input. It
also supplies `MEETERM_TMUX_SOCKET` for narrowly scoped test inspection.

The self-contained installed Android app is exercised through its actual UI:

```sh
python3 scripts/ssh/fixture.py -- \
  python3 scripts/ssh/android-smoke.py \
  --artifact-dir artifacts/android-emulator-observability
```

The driver verifies the displayed fingerprint, submits the disposable key,
waits for the authenticated runtime picker, explicitly selects the test tmux
session (or exercises the explicit detached-create path), discovers the
workspace/pane identity, and sends input through Android's native terminal. A
one-line server marker detects missing or duplicate execution. After
disconnect/reconnect, the driver checks the same verified runtime/pane and a
retained shell variable, then sends another native command. ANSI, Japanese,
and terminal dimensions are shown for visual inspection.

The prior `private_key_input (ui_timeout)` was a deterministic driver mismatch:
React Native Android joins `accessibilityLabel` and `accessibilityValue.text`
with `, `. The hidden key editor is therefore named
`Private OpenSSH key, Empty` or `Private OpenSSH key, Private key entered`.
The old exact-label lookup never matched it. The driver now accepts only the
known label/value forms, with regression coverage; secret masking and host-key
verification remain enabled.

The driver also requires keyboard focus before entering the key and verifies
short input batches and line breaks against the editor in memory. Two identical
readbacks separated by a quiet interval must confirm the value, even for a full
match. If that settled value is an exact prefix of the intended input, only the
missing suffix (or newline) is sent. Non-prefix corruption, an absent editor,
and lost keyboard focus fail immediately. Each intended prefix permits at most
four input attempts; readback has a 15-second budget and the whole key has a
ten-minute budget. In-flight adb commands retain their own finite timeouts;
once they return, an expired budget cannot authorize further input.

Accessibility hierarchy acquisition retries at most three times without
replaying input. Missing/invalid hierarchy and known UIAutomator idle/root failures are
reported as fixed diagnostic identifiers; neither raw XML nor key readback is
written to logs or artifacts. Recovery diagnostics contain only operation type,
attempt count, lengths, and newline counts. Identical native metadata snapshots retain their
React state references, avoiding needless accessibility property updates from
the one-second metadata poll.

`ssh-validation.txt` records sanitized stages, while `ssh-terminal.png` and
`ssh-logcat.txt` provide observability. The foundation's `terminal.png` remains
separate. Screenshot capture is not a machine acceptance gate, and unavailable
captures are reported explicitly. See [`CI_MOBILE.md`](CI_MOBILE.md).

The iOS hosted smoke now runs `scripts/ssh/ios-smoke.py` with a generated
XCUITest target. It drives the real app against the disposable SSH fixture,
including host-key approval, runtime discovery/selection, workspace/pane
selection, input, disconnect/reconnect, and desktop handoff. It separately
launches the explicit foundation preview for native
readiness, first frame, and process survival. Implementing this driver is not
proof that the hosted run passed; see `FIRST_APP.md` for current evidence.
A CoreGraphics fallback frame is explicitly different from Metal execution.
Physical-device GPU, Japanese IME, background network behavior, and font parity
still need their own device evidence.

The Android full and iOS `ssh` real-connection loss cases exercise tmux. Herdr's
zero-tap recovery is verified by the opt-in ignored Rust integration using the
real Herdr 0.9.0 binary over its isolated russh endpoint; mobile Herdr fixture
screenshots are presentation evidence only.
