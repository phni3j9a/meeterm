#!/usr/bin/env python3
"""Run a disposable, unprivileged OpenSSH fixture for integration tests.

The fixture owns every key, configuration file, and trust store under one
temporary directory.  With a command, it starts sshd, injects connection
details into the child's environment, waits for the child, and removes the
fixture.  With ``--env-file`` it stays alive until interrupted so a mobile
smoke process can use the same fixture.

The script deliberately does not print private key material or the encrypted
key passphrase.  It is a test harness, not a second SSH server abstraction.
"""

from __future__ import annotations

import argparse
import getpass
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Sequence


HOST = "127.0.0.1"
SSHD = "/usr/sbin/sshd"
READY_TIMEOUT_SECONDS = 10.0
TMUX = "tmux"
CONTROL_REQUEST_NAME = "sshd-control-request"
CONTROL_STATUS_NAME = "sshd-control-status"
CONTROL_REQUEST_ENV = "MEETERM_SSH_FIXTURE_CONTROL_REQUEST"
CONTROL_STATUS_ENV = "MEETERM_SSH_FIXTURE_CONTROL_STATUS"
CONTROL_TOKEN_RE = re.compile(r"[A-Za-z0-9_-]{8,64}\Z")
CONTROL_POLL_SECONDS = 0.05
CONTROL_TIMEOUT_SECONDS = 20.0
# The disposable sshd must expose the fixture's tmux binary to non-interactive
# remote commands. Keep this allowlist to standard macOS/Linux locations plus
# the directory containing the binary selected by shutil.which; never copy an
# arbitrary caller PATH into the fixture's sshd environment.
SYSTEM_PATH_DIRECTORIES = (
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
)


class FixtureError(RuntimeError):
    """A fixture could not be prepared or started."""


def _run_quietly(
    command: Sequence[str],
    *,
    input_text: str | None = None,
    capture_stdout: bool = False,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a helper without echoing its arguments or output."""

    try:
        return subprocess.run(
            list(command),
            check=True,
            input=input_text,
            stdout=subprocess.PIPE if capture_stdout else subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        raise FixtureError(f"helper command timed out: {command[0]}") from error
    except FileNotFoundError as error:
        raise FixtureError(f"required command is unavailable: {command[0]}") from error
    except subprocess.CalledProcessError as error:
        # ssh-keygen and sshd do not receive the private passphrase in their
        # output, but keeping helper diagnostics out of the terminal avoids
        # accidentally exposing paths or future authentication material.
        raise FixtureError(f"helper command failed: {command[0]}") from error


def _generate_ed25519_key(path: Path, passphrase: str) -> None:
    """Generate one key without exposing ssh-keygen output or arguments."""

    _run_quietly(
        [
            shutil.which("ssh-keygen") or "ssh-keygen",
            "-q",
            "-t",
            "ed25519",
            "-f",
            str(path),
            "-N",
            passphrase,
            "-C",
            "meeterm-ssh-fixture",
        ],
    )
    path.chmod(0o600)
    path.with_name(f"{path.name}.pub").chmod(0o644)


def _fingerprint(public_key: Path) -> str:
    result = _run_quietly(
        [
            shutil.which("ssh-keygen") or "ssh-keygen",
            "-lf",
            str(public_key),
            "-E",
            "sha256",
        ],
        capture_stdout=True,
    )
    fields = (result.stdout or "").split()
    for field in fields:
        if field.startswith("SHA256:"):
            return field
    raise FixtureError("ssh-keygen returned no SHA-256 host-key fingerprint")


def _choose_port() -> int:
    """Ask the kernel for a high loopback port before launching sshd."""

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        probe.bind((HOST, 0))
        port = int(probe.getsockname()[1])
    if port <= 1024:
        raise FixtureError("kernel returned a privileged fixture port")
    return port


class Fixture:
    """The files and process for one temporary OpenSSH server."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.port = _choose_port()
        self.user = getpass.getuser()
        if not self.user or any(character.isspace() for character in self.user):
            raise FixtureError("current account has no safe SSH username")

        self.client_key = root / "client_ed25519"
        self.encrypted_client_key = root / "client_ed25519_encrypted"
        self.host_key = root / "host_ed25519"
        # A second, unused host key lets the integration test model a changed
        # server identity against the live fixture endpoint without rotating
        # the key used by the running sshd.
        self.alternate_host_key = root / "alternate_host_ed25519"
        self.authorized_keys = root / "authorized_keys"
        self.trust_store = root / "known_hosts"
        self.config = root / "sshd_config"
        self.pid_file = root / "sshd.pid"
        # tmux uses $TMUX_TMPDIR/default as its ordinary socket path.  Keep
        # that directory inside this fixture so every remote shell and every
        # local helper wrapped by the fixture sees an isolated default server.
        # The product itself still uses ordinary tmux; this is only test
        # isolation, and cleanup below addresses this exact socket.
        self.tmux_tmpdir = root / "tmux"
        # tmux appends tmux-$UID below TMUX_TMPDIR before creating its
        # default socket. Keep the fully resolved path so cleanup never has
        # to ask tmux for (or guess at) the caller's ordinary socket.
        self.tmux_socket = self.tmux_tmpdir / f"tmux-{os.getuid()}" / "default"
        self.encrypted_passphrase = secrets.token_urlsafe(32)
        self.process: subprocess.Popen[str] | None = None
        self.tmux_process: subprocess.Popen[str] | None = None
        self.env_file: Path | None = None
        self.control_request_path = root / CONTROL_REQUEST_NAME
        self.control_status_path = root / CONTROL_STATUS_NAME
        self.control_stop_event = threading.Event()
        self.control_thread: threading.Thread | None = None
        self.sshd_lock = threading.RLock()

    def prepare(self) -> None:
        if os.geteuid() == 0:
            raise FixtureError("run the fixture as an unprivileged account; sudo is not required")
        if not Path(SSHD).is_file() or not os.access(SSHD, os.X_OK):
            raise FixtureError(f"OpenSSH server not found at {SSHD}")
        tmux_command = shutil.which(TMUX)
        if tmux_command is None:
            raise FixtureError("tmux is required for the OpenSSH fixture")
        fixture_path_directories = dict.fromkeys(
            [str(Path(tmux_command).absolute().parent), *SYSTEM_PATH_DIRECTORIES]
        )
        fixture_path = os.pathsep.join(fixture_path_directories)
        if any(character in fixture_path for character in "\r\n\0"):
            raise FixtureError("tmux directory contains an invalid configuration character")
        fixture_path = fixture_path.replace("\\", "\\\\").replace('"', '\\"')

        self.root.chmod(0o700)
        self.tmux_tmpdir.mkdir(mode=0o700)
        self.tmux_tmpdir.chmod(0o700)
        _generate_ed25519_key(self.client_key, "")
        _generate_ed25519_key(self.encrypted_client_key, self.encrypted_passphrase)
        _generate_ed25519_key(self.host_key, "")
        _generate_ed25519_key(self.alternate_host_key, "")

        public_keys = [
            self.client_key.with_name(f"{self.client_key.name}.pub").read_text(
                encoding="utf-8"
            ).strip(),
            self.encrypted_client_key.with_name(
                f"{self.encrypted_client_key.name}.pub"
            ).read_text(encoding="utf-8").strip(),
        ]
        if any(not key or "\n" in key for key in public_keys):
            raise FixtureError("ssh-keygen returned an invalid client public key")
        self.authorized_keys.write_text("\n".join(public_keys) + "\n", encoding="utf-8")
        self.authorized_keys.chmod(0o600)
        self.trust_store.write_text("", encoding="utf-8")
        self.trust_store.chmod(0o600)

        # Use only this file.  No system sshd configuration, user ssh config,
        # or ~/.ssh path is read or modified by the fixture server.
        self.config.write_text(
            "\n".join(
                (
                    f"Port {self.port}",
                    f"ListenAddress {HOST}",
                    f"HostKey {self.host_key}",
                    f"PidFile {self.pid_file}",
                    f"AuthorizedKeysFile {self.authorized_keys}",
                    f"AllowUsers {self.user}",
                    # OpenSSH SetEnv applies to every session created by this
                    # fixture, including commands run through the ordinary
                    # desktop ssh/tmux smoke.  It does not alter the user's
                    # account environment or any system sshd configuration.
                    f"SetEnv TMUX_TMPDIR={self.tmux_tmpdir}",
                    f'SetEnv "PATH={fixture_path}"',
                    "PubkeyAuthentication yes",
                    "AuthenticationMethods publickey",
                    "PasswordAuthentication no",
                    "KbdInteractiveAuthentication no",
                    "ChallengeResponseAuthentication no",
                    "PermitEmptyPasswords no",
                    "PermitRootLogin no",
                    "PermitTTY yes",
                    "UsePAM no",
                    "StrictModes yes",
                    "UseDNS no",
                    "PrintMotd no",
                    "X11Forwarding no",
                    "AllowAgentForwarding no",
                    "AllowTcpForwarding no",
                    "PermitTunnel no",
                    "PermitUserEnvironment no",
                    "LogLevel QUIET",
                )
            )
            + "\n",
            encoding="utf-8",
        )
        self.config.chmod(0o600)
        _run_quietly([SSHD, "-t", "-f", str(self.config)])

    def _start_tmux(self) -> None:
        # Start an empty private server without loading ~/.tmux.conf. -D
        # keeps the empty server alive; the application still creates the
        # managed session itself through its ordinary production command.
        self.tmux_socket.parent.mkdir(mode=0o700, exist_ok=True)
        tmux_environment = dict(os.environ)
        tmux_environment.pop("TMUX", None)
        tmux_environment.pop("TMUX_PANE", None)
        tmux_environment["TMUX_TMPDIR"] = str(self.tmux_tmpdir)
        self.tmux_process = subprocess.Popen(
            [TMUX, "-D", "-f", "/dev/null", "-S", str(self.tmux_socket)],
            env=tmux_environment, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, text=True,
        )
        deadline = time.monotonic() + READY_TIMEOUT_SECONDS
        while not self.tmux_socket.exists():
            if self.tmux_process.poll() is not None or time.monotonic() >= deadline:
                raise FixtureError("isolated tmux fixture did not start")
            time.sleep(0.05)
        # Keep disposable panes independent of the developer's interactive
        # shell startup (for example an oh-my-zsh update prompt). These options
        # affect only the absolute fixture socket, never the ordinary server.
        _run_quietly([
            TMUX, "-S", str(self.tmux_socket),
            "set-option", "-g", "default-shell", "/bin/sh", ";",
            "set-option", "-g", "default-command", "exec /bin/sh -i",
        ])

    def start_sshd(self) -> None:
        """Start only this fixture's sshd on its original endpoint."""

        with self.sshd_lock:
            if self.process is not None and self.process.poll() is None:
                raise FixtureError("OpenSSH fixture is already running")
            self.process = None
            try:
                self.process = subprocess.Popen(
                    [SSHD, "-D", "-e", "-f", str(self.config)],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE,
                    text=True,
                    close_fds=True,
                    # The fixture controller must be able to terminate the
                    # connection tree without touching the wrapper, its tmux
                    # server, or any process outside this sshd instance.
                    start_new_session=True,
                )
            except OSError as error:
                raise FixtureError("could not start OpenSSH fixture") from error

            deadline = time.monotonic() + READY_TIMEOUT_SECONDS
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    self._raise_start_failure()
                try:
                    with socket.create_connection((HOST, self.port), timeout=0.2):
                        return
                except OSError:
                    time.sleep(0.05)
            self._raise_start_failure()

    def start(self) -> None:
        self._start_tmux()
        self.start_sshd()

    @staticmethod
    def _descendant_process_ids(root_pid: int) -> list[int]:
        """Return descendants from one point-in-time process table snapshot."""

        try:
            result = subprocess.run(
                ["ps", "-axo", "pid=,ppid="],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=3,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            return []
        parents: dict[int, int] = {}
        for line in result.stdout.splitlines():
            fields = line.split()
            if len(fields) != 2:
                continue
            try:
                pid, parent = (int(field) for field in fields)
            except ValueError:
                continue
            if pid > 0 and parent >= 0:
                parents[pid] = parent
        descendants: list[int] = []
        pending = [root_pid]
        while pending:
            parent = pending.pop()
            children = [pid for pid, ppid in parents.items() if ppid == parent]
            for child in children:
                if child not in descendants:
                    descendants.append(child)
                    pending.append(child)
        return descendants

    @staticmethod
    def _signal_process_group(process: subprocess.Popen[str], signum: int) -> None:
        """Signal only an sshd process group, with a safe fallback."""

        try:
            os.killpg(process.pid, signum)
            return
        except (AttributeError, OSError):
            pass
        try:
            process.send_signal(signum)
        except ProcessLookupError:
            pass

    def stop_sshd(self) -> None:
        """Stop sshd and its accepted-session children, retaining tmux."""

        with self.sshd_lock:
            process = self.process
            self.process = None
            if process is None:
                return
            if process.poll() is not None:
                try:
                    process.communicate(timeout=1)
                except subprocess.TimeoutExpired:
                    pass
                return

            # A session child normally inherits the new process group. Keep
            # the descendant list as a portable fallback for sshd variants
            # that move an accepted session to another process group.
            descendants = self._descendant_process_ids(process.pid)
            self._signal_process_group(process, signal.SIGTERM)
            for child_pid in descendants:
                try:
                    os.kill(child_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                except OSError:
                    pass
            try:
                process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                self._signal_process_group(process, signal.SIGKILL)
                for child_pid in descendants:
                    try:
                        os.kill(child_pid, signal.SIGKILL)
                    except (ProcessLookupError, OSError):
                        pass
                try:
                    process.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    # The process object is no longer owned by the fixture;
                    # cleanup remains bounded and the OS will reap it later.
                    pass

    def _write_control_status(self, token: str, status: str) -> None:
        if not CONTROL_TOKEN_RE.fullmatch(token) or status not in {"started", "stopped", "error"}:
            return
        contents = f"{token}\tok\t{status}\n" if status != "error" else f"{token}\terror\n"
        temporary = self.root / f".{CONTROL_STATUS_NAME}.{token}.{secrets.token_hex(4)}"
        descriptor: int | None = None
        try:
            descriptor = os.open(
                temporary,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
            )
            os.fchmod(descriptor, 0o600)
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                descriptor = None
                stream.write(contents)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.control_status_path)
        except OSError:
            # The controller is a test-only coordination path.  A sender
            # will time out with a bounded, sanitized error if status cannot
            # be published; never print the path or request contents here.
            pass
        finally:
            if descriptor is not None:
                try:
                    os.close(descriptor)
                except OSError:
                    pass
            try:
                temporary.unlink()
            except (FileNotFoundError, OSError):
                pass

    def _read_control_request(self) -> tuple[str, str] | None:
        try:
            request_stat = self.control_request_path.lstat()
            if not request_stat or not self.control_request_path.is_file():
                return None
            if request_stat.st_uid != os.getuid() or request_stat.st_mode & 0o077:
                return None
            contents = self.control_request_path.read_text(encoding="utf-8")
        except (FileNotFoundError, OSError, UnicodeError):
            return None
        if not contents.endswith("\n") or contents.count("\n") != 1:
            return None
        fields = contents[:-1].split("\t")
        if len(fields) != 2 or not CONTROL_TOKEN_RE.fullmatch(fields[0]):
            return None
        if fields[1] not in {"start", "stop"}:
            return fields[0], "error"
        return fields[0], fields[1]

    def _control_loop(self) -> None:
        while not self.control_stop_event.wait(CONTROL_POLL_SECONDS):
            if not self.control_request_path.exists():
                continue
            request = self._read_control_request()
            if request is None:
                # A partial write is left for the sender to finish.  A
                # malformed complete request is removed so it cannot be
                # retried indefinitely; valid requests always use the exact
                # one-line protocol above.
                try:
                    contents = self.control_request_path.read_text(encoding="utf-8")
                except (FileNotFoundError, OSError, UnicodeError):
                    continue
                if contents.endswith("\n"):
                    try:
                        self.control_request_path.unlink()
                    except (FileNotFoundError, OSError):
                        pass
                continue
            token, action = request
            try:
                self.control_request_path.unlink()
            except (FileNotFoundError, OSError):
                continue
            if action == "error":
                self._write_control_status(token, "error")
                continue
            try:
                if action == "stop":
                    self.stop_sshd()
                else:
                    self.start_sshd()
            except FixtureError:
                self._write_control_status(token, "error")
            else:
                self._write_control_status(token, "stopped" if action == "stop" else "started")

    def start_control(self) -> None:
        if self.control_thread is not None and self.control_thread.is_alive():
            return
        self.control_stop_event.clear()
        self.control_thread = threading.Thread(
            target=self._control_loop,
            name="meeterm-ssh-fixture-control",
            daemon=True,
        )
        self.control_thread.start()

    def stop_control(self) -> None:
        self.control_stop_event.set()
        thread = self.control_thread
        self.control_thread = None
        if thread is not None:
            thread.join(timeout=3)
        for path in (self.control_request_path, self.control_status_path):
            try:
                path.unlink()
            except (FileNotFoundError, OSError):
                pass
        for path in self.root.glob(f".{CONTROL_STATUS_NAME}.*"):
            try:
                path.unlink()
            except (FileNotFoundError, OSError):
                pass

    def _raise_start_failure(self) -> None:
        if self.process is not None:
            try:
                self.process.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.communicate()
        raise FixtureError("OpenSSH fixture exited before listening")

    def check_ssh_tmux(self) -> None:
        """Prove authentication and remote tmux resolution before a mobile build."""
        public_key = self.host_key.with_name(self.host_key.name + ".pub").read_text().strip()
        self.trust_store.write_text(f"[{HOST}]:{self.port} {public_key}\n")
        _run_quietly([
            "ssh", "-F", "/dev/null",
            "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "IdentityAgent=none", "-o", "ConnectTimeout=5",
            "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=1",
            "-o", "StrictHostKeyChecking=yes", "-o", "GlobalKnownHostsFile=/dev/null",
            "-o", f"UserKnownHostsFile={self.trust_store}",
            "-i", str(self.client_key), "-p", str(self.port),
            f"{self.user}@{HOST}", "tmux -V",
        ], timeout=20)

    def environment(self) -> dict[str, str]:
        host_public_key = self.host_key.with_name(f"{self.host_key.name}.pub")
        alternate_host_public_key = self.alternate_host_key.with_name(
            f"{self.alternate_host_key.name}.pub"
        )
        fingerprint = _fingerprint(host_public_key)
        return {
            # These names are the small contract consumed by the Rust
            # integration test and can also be sourced by a mobile smoke
            # process.  The canonical key is encrypted so the test exercises
            # the passphrase-aware russh path; the unencrypted sibling is
            # provided for the OpenSSH CLI smoke, which must stay unattended.
            "MEETERM_SSH_HOST": HOST,
            "MEETERM_SSH_PORT": str(self.port),
            "MEETERM_SSH_USERNAME": self.user,
            "MEETERM_SSH_PRIVATE_KEY_FILE": str(self.encrypted_client_key),
            "MEETERM_SSH_PASSPHRASE": self.encrypted_passphrase,
            "MEETERM_SSH_FINGERPRINT": fingerprint,
            "MEETERM_SSH_KNOWN_HOSTS_FILE": str(self.trust_store),
            "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE": str(self.client_key),
            "MEETERM_SSH_HOST_KEY_FILE": str(host_public_key),
            "MEETERM_SSH_ALTERNATE_HOST_KEY_FILE": str(alternate_host_public_key),
            # These are useful to shell-level integration checks and make the
            # isolation contract explicit.  The SSH server receives the same
            # path through SetEnv above.
            "MEETERM_TMUX_TMPDIR": str(self.tmux_tmpdir),
            "MEETERM_TMUX_SOCKET": str(self.tmux_socket),
            "TMUX_TMPDIR": str(self.tmux_tmpdir),
            # The persistent fixture exposes only these two opaque, fixture
            # owned paths to the mobile smoke driver.  They are not remote
            # SSH inputs and are never forwarded to the host under test.
            CONTROL_REQUEST_ENV: str(self.control_request_path),
            CONTROL_STATUS_ENV: str(self.control_status_path),
        }

    def write_env_file(self, path: Path) -> None:
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
        except OSError as error:
            raise FixtureError(f"could not create environment-file directory: {path.parent}") from error

        # A developer may invoke the persistent fixture from inside an
        # existing tmux client. Clear its routing variables before any
        # wrapped local CLI command can accidentally address that server.
        lines = ["unset TMUX TMUX_PANE"]
        lines.extend(
            f"export {name}={shlex.quote(value)}"
            for name, value in sorted(self.environment().items())
        )
        contents = "\n".join(lines) + "\n"
        descriptor: int | None = None
        created = False
        complete = False
        try:
            # O_EXCL rejects existing regular files and dangling symlinks
            # before any secret-bearing bytes are written.  fchmod keeps the
            # mode exact even when the caller has an unusual umask.
            descriptor = os.open(
                path,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
            )
            created = True
            os.fchmod(descriptor, 0o600)
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                descriptor = None
                stream.write(contents)
                stream.flush()
                os.fsync(stream.fileno())
            complete = True
        except FileExistsError as error:
            raise FixtureError(f"environment file already exists: {path}") from error
        except (OSError, ValueError) as error:
            raise FixtureError(f"could not write environment file: {path}") from error
        finally:
            if descriptor is not None:
                os.close(descriptor)
            if created and not complete:
                try:
                    path.unlink()
                except FileNotFoundError:
                    pass
                except OSError:
                    pass
        self.env_file = path

    def stop(self) -> None:
        self.stop_control()
        self.stop_sshd()
        # A tmux server outlives the sshd process that created it.  Kill only
        # this fixture's absolute socket before TemporaryDirectory removes
        # the socket directory; never invoke the default client without -S,
        # because that could reach the developer's ordinary tmux server.
        tmux = shutil.which(TMUX)
        if tmux is not None:
            try:
                subprocess.run(
                    [tmux, "-S", str(self.tmux_socket), "kill-server"],
                    check=False,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=3,
                )
            except (OSError, subprocess.TimeoutExpired):
                # The fixture is already on its cleanup path.  A missing
                # socket or an exited server is harmless; the enclosing
                # temporary directory remains the ownership boundary.
                pass
        tmux_process = self.tmux_process
        self.tmux_process = None
        if tmux_process is not None:
            try:
                tmux_process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                tmux_process.kill()
                tmux_process.wait()
        if self.env_file is not None:
            try:
                self.env_file.unlink()
            except FileNotFoundError:
                pass
            self.env_file = None


def _run_child(command: Sequence[str], environment: dict[str, str]) -> int:
    child_environment = {**os.environ, **environment}
    # If the fixture wrapper is launched from inside the developer's tmux
    # client, TMUX would override TMUX_TMPDIR for local test commands. The
    # remote sshd never receives this variable because it is not forwarded,
    # while the wrapped Rust/CLI test process must start with a clean client
    # context as well.
    child_environment.pop("TMUX", None)
    child_environment.pop("TMUX_PANE", None)
    child = subprocess.Popen(list(command), env=child_environment)
    interrupted = threading.Event()

    def forward_signal(signum: int, _frame: object) -> None:
        interrupted.set()
        if child.poll() is None:
            try:
                child.send_signal(signum)
            except ProcessLookupError:
                pass

    previous_handlers = {
        signal.SIGINT: signal.signal(signal.SIGINT, forward_signal),
        signal.SIGTERM: signal.signal(signal.SIGTERM, forward_signal),
    }
    try:
        while child.poll() is None:
            time.sleep(0.1)
        exit_code = int(child.returncode)
        return 128 - exit_code if exit_code < 0 else exit_code
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        # The event documents that the parent observed and forwarded a
        # signal.  The child's normalized status is returned above.
        _ = interrupted


def _wait_for_signal() -> None:
    stopped = threading.Event()

    def stop(_signum: int, _frame: object) -> None:
        stopped.set()

    previous_handlers = {
        signal.SIGINT: signal.signal(signal.SIGINT, stop),
        signal.SIGTERM: signal.signal(signal.SIGTERM, stop),
    }
    try:
        while not stopped.wait(0.2):
            pass
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)


def _validated_control_paths() -> tuple[Path, Path]:
    """Return fixture-owned control paths from the inherited environment."""

    values = {
        name: os.environ.get(name, "")
        for name in (CONTROL_REQUEST_ENV, CONTROL_STATUS_ENV)
    }
    if any(not value for value in values.values()):
        raise FixtureError("fixture control is unavailable")
    paths = {name: Path(value) for name, value in values.items()}
    request = paths[CONTROL_REQUEST_ENV]
    status = paths[CONTROL_STATUS_ENV]
    if not request.is_absolute() or not status.is_absolute():
        raise FixtureError("fixture control path is invalid")
    if request.name != CONTROL_REQUEST_NAME or status.name != CONTROL_STATUS_NAME:
        raise FixtureError("fixture control path is invalid")
    if request.parent != status.parent:
        raise FixtureError("fixture control path is invalid")
    root = request.parent
    try:
        root_stat = root.lstat()
    except OSError as error:
        raise FixtureError("fixture control is unavailable") from error
    if (
        root.is_symlink()
        or not root.is_dir()
        or not root.name.startswith("meeterm-ssh-fixture-")
        or root_stat.st_uid != os.getuid()
        or root_stat.st_mode & 0o077
    ):
        raise FixtureError("fixture control path is invalid")
    for path in (request, status):
        try:
            path_stat = path.lstat()
        except FileNotFoundError:
            continue
        except OSError as error:
            raise FixtureError("fixture control is unavailable") from error
        if path.is_symlink() or not path.is_file() or path_stat.st_uid != os.getuid():
            raise FixtureError("fixture control path is invalid")
    return request, status


def _write_control_request(path: Path, contents: str) -> None:
    descriptor: int | None = None
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            descriptor = None
            stream.write(contents)
            stream.flush()
            os.fsync(stream.fileno())
    except FileExistsError as error:
        raise FixtureError("fixture control is busy") from error
    except (OSError, ValueError) as error:
        raise FixtureError("fixture control request could not be written") from error
    finally:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass


def send_control(action: str, timeout: float = CONTROL_TIMEOUT_SECONDS) -> None:
    """Request one bounded sshd stop/start from a persistent fixture."""

    if action not in {"start", "stop"}:
        raise FixtureError("unknown fixture control action")
    request_path, status_path = _validated_control_paths()
    token = f"transport-loss-{secrets.token_hex(8)}"
    request_contents = f"{token}\t{action}\n"
    _write_control_request(request_path, request_contents)
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            try:
                status_contents = status_path.read_text(encoding="utf-8")
            except (FileNotFoundError, OSError, UnicodeError):
                status_contents = ""
            fields = status_contents.rstrip("\n").split("\t")
            if fields and fields[0] == token:
                if fields == [token, "ok", "stopped" if action == "stop" else "started"]:
                    return
                if fields == [token, "error"]:
                    raise FixtureError("fixture control action failed")
                raise FixtureError("fixture control returned an invalid status")
            time.sleep(CONTROL_POLL_SECONDS)
    finally:
        # If the controller has not consumed a request, remove only the exact
        # bytes written by this sender. Never unlink another request.
        try:
            if request_path.read_text(encoding="utf-8") == request_contents:
                request_path.unlink()
        except (FileNotFoundError, OSError, UnicodeError):
            pass
    raise FixtureError("fixture control timed out")


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run an ephemeral unprivileged OpenSSH fixture around a command."
    )
    parser.add_argument(
        "--env-file",
        type=Path,
        help="keep sshd alive and write a mode-0600 shell environment file until interrupted",
    )
    parser.add_argument(
        "--print-fingerprint",
        action="store_true",
        help="print the disposable host fingerprint (never private key material)",
    )
    parser.add_argument(
        "--control",
        choices=("stop", "start"),
        help="request stop/start of the inherited fixture sshd without creating a new fixture",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    parser.add_argument("--check", action="store_true", help="verify real SSH authentication and remote tmux, then clean up")
    args = parser.parse_args(argv)
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if args.control is not None and (
        args.env_file is not None or args.command or args.check or args.print_fingerprint
    ):
        parser.error("--control cannot be combined with fixture startup options")
    if args.control is not None:
        return args
    if args.env_file is not None and args.command:
        parser.error("--env-file is persistent mode and cannot wrap a command")
    if args.check and (args.env_file is not None or args.command):
        parser.error("--check cannot be combined with a command or --env-file")
    if args.env_file is None and not args.command and not args.check:
        parser.error("provide a command, or use --env-file for persistent mode")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(argv if argv is not None else sys.argv[1:])
    try:
        if args.control is not None:
            send_control(args.control)
            return 0
        # Keep the temporary tree below the account's home directory.  An
        # OpenSSH server with StrictModes enabled rejects authorized_keys below
        # a world-writable /tmp directory, while this still avoids ~/.ssh and
        # is removed by TemporaryDirectory on every normal exit path.
        with tempfile.TemporaryDirectory(
            prefix="meeterm-ssh-fixture-", dir=Path.home()
        ) as temporary_root:
            fixture = Fixture(Path(temporary_root))
            try:
                fixture.prepare()
                fixture.start()
                if args.check:
                    fixture.check_ssh_tmux()
                    print("OpenSSH fixture check passed: authenticated SSH and remote tmux")
                    return 0
                environment = fixture.environment()
                if args.print_fingerprint:
                    print(environment["MEETERM_SSH_FINGERPRINT"])

                if args.env_file is not None:
                    fixture.write_env_file(args.env_file)
                    fixture.start_control()
                    print(
                        f"OpenSSH fixture ready on {HOST}:{fixture.port}; "
                        f"environment file: {args.env_file}",
                        file=sys.stderr,
                    )
                    _wait_for_signal()
                    return 0
                fixture.start_control()
                return _run_child(args.command, environment)
            finally:
                fixture.stop()
    except FixtureError as error:
        print(f"ssh fixture: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
