# SSH and durable tmux sessions

The native connection attaches to or creates the session named `meeterm` on the
remote user's ordinary tmux server. Workspaces are tmux windows and terminals
are panes. A desktop user can continue with `tmux attach -t meeterm`.

## Using the session loop

1. Open **Connect**, enter the SSH endpoint, username, OpenSSH private key, and
   optional passphrase, and submit the form.
2. Verify the displayed SHA-256 host-key fingerprint through a trusted channel
   before choosing **Trust and connect**. A changed trusted key fails closed.
3. After **Connected**, select a workspace and its terminal tabs. Existing
   windows and panes can be created or changed with ordinary remote tmux
   commands; the native core discovers the topology.
4. **Disconnect** closes the mobile connection while the remote session and
   its processes continue running. **Reconnect** resumes that workspace.
5. After a transport failure, use **Reconnect**. After the app process exits,
   enter the connection details and key again with **Connect**; the remote
   tmux session is still the source of truth.

The parsed private key is retained only in Rust process memory for explicit
reconnect. The form clears private-key and passphrase text on submission or
cancellation. Credentials are not saved to disk. Approved host identities are
stored separately in app-private storage and checked again during reconnect.
Password, keyboard-interactive, SSH-agent, server profiles, and saving keys in
platform secure storage remain outside this slice.

## Native data and lifecycle boundary

SSH opens an exec channel for `tmux -C -u new-session -A -s meeterm`. It does not
allocate an outer SSH PTY. `-CC` attempts to configure terminal attributes and
fails without a terminal on tmux 3.4; `-C` supplies the same Control Mode
protocol over pipes. See the measured explanation in
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

Reconnect is explicitly requested, not a React timer or an unbounded background
retry. React polls only low-frequency state. Native reconstruction after a
connection gap must obtain the current remote screen and topology before
accepting terminal input again. Local display contents alone are not proof of
remote reconnection.

The selected pane is temporarily zoomed with tmux's own zoom operation; its
underlying split layout is retained. Recovery uses an allocated pair of
session-scoped `client-detached` / `client-session-changed` hooks so that a
lost mobile transport or desktop handoff can undo the mobile zoom. Existing
user hook entries are preserved. The pair removes itself on recovery; no
global hook or configuration file is installed.

Screen reconstruction captures the current pane with ANSI attributes and
restores its dimensions, cursor, active alternate screen, and exposed input
modes. This is not a serialization of a running application's entire terminal
parser state. In particular, saved primary-screen contents, scroll margins,
and partially emitted escape sequences are not reconstructed. tmux 3.4 does
not expose bracketed-paste mode in its format metadata; the capture defaults
that mode off until the application emits it again. A static alternate-screen
recovery test is useful evidence, but arbitrary full-screen TUI process-death
recovery still needs application-specific validation and may require a redraw.

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

Prerequisites are Python 3.10+, `/usr/sbin/sshd`, `ssh`, `ssh-keygen`, and `tmux`.
The fixture refuses to run as root. Missing prerequisites are environment setup
issues; the script does not install system packages or reconfigure services.

Run the native integration target:

```sh
python3 scripts/ssh/fixture.py -- \
  cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh -- --ignored --nocapture
```

Run the deterministic driver regressions:

```sh
python3 -m unittest discover -s scripts/ssh -p 'test_*.py'
```

The Rust integration target exercises the real SSH/tmux/native-terminal path.
Assertions cover explicit trust and encrypted-key authentication, pane-specific
output and input, remote dimensions, topology changes, disconnect and resume,
and rejection of input while disconnected. Recovery assertions use remote
process state and native snapshots, not just the presence of a connection flag.
Additional handoff and reconstruction boundaries are recorded with the test
results; a fixture passing does not establish physical-device parity.

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
waits for the native session, discovers the workspace/pane identity, and sends
input through Android's native terminal. A one-line server marker detects
missing or duplicate execution. After disconnect/reconnect, the driver checks
the same pane and a retained shell variable, then sends another native command.
ANSI, Japanese, and terminal dimensions are shown for visual inspection.

The prior `private_key_input (ui_timeout)` was a deterministic driver mismatch:
React Native Android joins `accessibilityLabel` and `accessibilityValue.text`
with `, `. The hidden key editor is therefore named
`Private OpenSSH key, Empty` or `Private OpenSSH key, Private key entered`.
The old exact-label lookup never matched it. The driver now accepts only the
known label/value forms, with regression coverage; secret masking and host-key
verification remain enabled.

`ssh-validation.txt` records sanitized stages, while `ssh-terminal.png` and
`ssh-logcat.txt` provide observability. The foundation's `terminal.png` remains
separate. Screenshot capture is not a machine acceptance gate, and unavailable
captures are reported explicitly. See [`CI_MOBILE.md`](CI_MOBILE.md).

The iOS hosted smoke validates the shared library, native adapter, module
readiness, and first frame using the local demo. It does not claim an interactive
iOS SSH flow. A CoreGraphics fallback frame is explicitly different from Metal
execution. Physical-device GPU, Japanese IME, background network behavior, and
font parity still need their own device evidence.
