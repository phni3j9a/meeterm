# SSH validation

Issue #3 uses an ordinary OpenSSH server as the remote end of the vertical
slice. `scripts/ssh/fixture.py` starts a disposable `sshd` process as the
current unprivileged account. It creates an Ed25519 host key, an encrypted
Ed25519 client key, an unencrypted client-key counterpart, `authorized_keys`,
the server configuration, and an empty known-hosts file below one private
temporary directory. The server listens only on `127.0.0.1` and on a kernel
assigned port above 1024.

The fixture does not touch `/etc/ssh`, the system sshd service, `~/.ssh`, or a
user's SSH configuration. It does not need `sudo`. The fixture is deliberately
an SSH shell fixture: it does not create tmux state, test tmux Control Mode, or
provide reconnect behavior.

## Prerequisites

The host running the fixture must have:

- `/usr/sbin/sshd` from OpenSSH server;
- `ssh` and `ssh-keygen` from the OpenSSH client;
- Python 3.10 or newer.

The fixture refuses to run as root. A missing `/usr/sbin/sshd` is an environment
setup issue; install it through the host's normal package management outside
this runbook rather than changing the repository or the system SSH service.

## Rust integration runner

Command mode keeps the server alive for exactly one downstream command. The
child inherits the current environment plus the following temporary values:

| Variable | Meaning |
| --- | --- |
| `MEETERM_SSH_HOST` | `127.0.0.1` |
| `MEETERM_SSH_PORT` | Ephemeral high loopback port |
| `MEETERM_SSH_USERNAME` | Current unprivileged account |
| `MEETERM_SSH_PRIVATE_KEY_FILE` | Encrypted Ed25519 private-key path |
| `MEETERM_SSH_PASSPHRASE` | Passphrase for that encrypted key |
| `MEETERM_SSH_FINGERPRINT` | SHA-256 fingerprint of the fixture host key |
| `MEETERM_SSH_KNOWN_HOSTS_FILE` | Empty mode-0600 trust-store path |
| `MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE` | Unencrypted Ed25519 key for unattended OpenSSH CLI checks |
| `MEETERM_SSH_HOST_KEY_FILE` | Fixture host public-key path |

The key and passphrase values are transient test credentials. The script never
prints private key material, the passphrase, or helper command arguments. The
temporary directory is removed after the command exits, including the normal
failure and interrupt paths. A `SIGKILL` or host crash cannot run cleanup, so a
runner should use its ordinary temporary-directory cleanup policy for that
exceptional case.

Invoke the Rust integration target supplied by the shared-core implementation
through the wrapper, for example:

```sh
python3 scripts/ssh/fixture.py -- \
  cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test openssh -- --ignored --nocapture
```

The `openssh` Rust test reads the fixture environment, connects with the
encrypted key, performs an explicit host-key decision, and opens an interactive
PTY. It checks ANSI attributes, `ls`, Japanese committed input and output,
two `stty size` values, normal shell exit, pinned-key reconnect, 32 seconds of
idle time, explicit disconnect, and rejection of input after disconnect. It
uses the shared Rust API and native C input/snapshot contracts without a
developer's SSH agent or persistent credentials.

## OpenSSH CLI smoke

The unencrypted sibling key exists only to make a noninteractive OpenSSH
command possible without putting the encrypted passphrase in a process prompt.
This command exercises real public-key authentication, host-key TOFU into the
fixture's isolated trust file, an allocated PTY, ANSI bytes, UTF-8 text, and a
remote `stty` query:

```sh
python3 scripts/ssh/fixture.py -- sh -c '
  set -eu
  export TERM=xterm-256color
  ssh -tt -p "$MEETERM_SSH_PORT" \
    -F /dev/null \
    -i "$MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE" \
    -o IdentitiesOnly=yes \
    -o BatchMode=yes \
    -o GlobalKnownHostsFile=/dev/null \
    -o UserKnownHostsFile="$MEETERM_SSH_KNOWN_HOSTS_FILE" \
    -o StrictHostKeyChecking=accept-new \
    -o ConnectTimeout=5 \
    "$MEETERM_SSH_USERNAME@$MEETERM_SSH_HOST" \
    '\''stty rows 24 cols 80; printf "MEETERM_FIXTURE_READY\\n"; printf "\\033[31mRED\\033[0m\\n"; printf "日本語\\n"; stty size'\''
'
```

`accept-new` is intentionally limited to this disposable fixture command. The
application's first connection must show `MEETERM_SSH_FINGERPRINT` to the user
and require an explicit trust decision. Once approved, it persists the
fingerprint/host identity in its local trust state. A later connection with a
different host key at the same host and port must fail closed; it must not
silently replace the stored identity. Preserve the trust file while rotating
the server host key (or point the client at an alternate server with the same
logical endpoint) to exercise this changed-key rejection.

The encrypted key can be checked without printing its secret or using an SSH
agent:

```sh
python3 scripts/ssh/fixture.py -- sh -c '
  set -eu
  ssh-keygen -y -P "$MEETERM_SSH_PASSPHRASE" \
    -f "$MEETERM_SSH_PRIVATE_KEY_FILE" >/dev/null
'
```

## Persistent fixture for a mobile run

Persistent mode writes a mode-0600 shell environment file and keeps `sshd`
alive until `SIGINT` or `SIGTERM`. The file contains the encrypted-key
passphrase, so do not print it or upload it as an artifact.

```sh
fixture_env="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/meeterm-ssh.env"
python3 scripts/ssh/fixture.py --env-file "$fixture_env" &
fixture_pid=$!
trap 'kill "$fixture_pid" 2>/dev/null || true; wait "$fixture_pid" 2>/dev/null || true' EXIT INT TERM

while ! test -s "$fixture_env"; do
  kill -0 "$fixture_pid" 2>/dev/null || exit 1
  sleep 0.1
done
set -a
. "$fixture_env"
set +a
```

For an Android device connected over USB, set `MEETERM_ANDROID_DEVICE` to its
serial from `adb devices -l`, forward the fixture port, and keep the app's test
endpoint at `127.0.0.1`:

```sh
adb -s "$MEETERM_ANDROID_DEVICE" reverse tcp:"$MEETERM_SSH_PORT" tcp:"$MEETERM_SSH_PORT"
```

Open **Connect** in the app and supply the fixture host, port, username,
private OpenSSH key contents, and passphrase. Authentication inputs cross the
low-frequency control plane once; the form clears them on submission or
cancellation. The app does not persist credentials. Saving credentials in
platform secure storage is a follow-up; host-key trust is already persisted
separately in app-private storage. Do not copy fixture keys into the repository
or upload them with observability artifacts. On iOS Simulator, use the local
loopback endpoint and the same explicit host-key prompt; simulator success does
not establish physical-device network parity.

## Validation boundary

The implementation was exercised on Ubuntu 24.04 on 2026-09-05 using the
disposable local OpenSSH server and Rust 1.96.0. The real-server integration
test passed with encrypted Ed25519 authentication, explicit trust followed by
pinned reconnect, interactive shell output, ANSI cell attributes, Japanese
committed input and output, two remote PTY sizes, normal shell exit, a 32-second
idle interval, and explicit disconnect. The final local checks passed 23 Rust
unit tests, format/clippy, TypeScript, all 21 Expo Doctor checks, and the fresh
Android CNG debug build with 10 native tests. These local results establish the
shared-core and Android build boundaries; hosted device execution and images
are recorded separately in the pull request's Mobile smoke run.

Current limits are deliberate: authentication uses OpenSSH private keys only;
password, keyboard-interactive, SSH-agent, and saved credentials are deferred.
There is no automatic reconnect, process-death recovery, tmux integration, or
promise of background transport survival. View unmounts retain the terminal
and session while the process is alive. Real Japanese IME composition on a
physical remote-shell session and physical iOS Metal rendering require separate
device validation; the automated Android UI input uses injected key events.

The Android emulator job runs the same fixture through the connection form:

```sh
python3 scripts/ssh/fixture.py -- \
  python3 scripts/ssh/android-smoke.py \
  --artifact-dir artifacts/android-emulator-observability
```

The app must already be installed as a self-contained build. The script starts
the app, supplies the disposable unencrypted key, compares the trust prompt
with the fixture fingerprint, and enters commands through Android's native
input path. It checks a unique one-line marker on the real server to detect
missing or duplicated command execution. It then displays ANSI, Japanese,
`stty size`, and `ls` output for visual inspection. The encrypted-key path is
covered by the Rust integration test. No real credentials or production test
entry point are needed. Use `--serial` or `ANDROID_SERIAL` when multiple devices
are attached; reserve that device for the test while it runs.

`ssh-validation.txt` records the sanitized machine result. `ssh-terminal.png`
and `ssh-logcat.txt` are observability artifacts; a capture failure produces an
explicit unavailable diagnostic and does not become a screenshot gate. The
foundation's `terminal.png` is preserved separately.

This fixture proves that the shared Rust SSH layer can authenticate to a real
OpenSSH server and exchange an interactive shell's raw PTY bytes. It is useful
for Rust integration tests and controlled native/mobile smoke runs. It does
not prove tmux session durability, `tmux -CC`, pane routing, reconnect or
resynchronization, full-screen TUI behavior, physical-device GPU/font/IME
parity, or production credential storage. Those checks belong to later slices
or to separately recorded device validation.

CI may use this fixture because all credentials and host keys are generated at
runtime and discarded. CI must not contain real user SSH credentials, private
keys, passphrases, or a persistent known-hosts file. The mobile job remains a
separate build/install/runtime gate; this fixture does not make an uploaded
screenshot evidence of interactive mobile success.
