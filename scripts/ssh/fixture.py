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


class FixtureError(RuntimeError):
    """A fixture could not be prepared or started."""


def _run_quietly(
    command: Sequence[str],
    *,
    input_text: str | None = None,
    capture_stdout: bool = False,
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
        )
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

    def prepare(self) -> None:
        if os.geteuid() == 0:
            raise FixtureError("run the fixture as an unprivileged account; sudo is not required")
        if not Path(SSHD).is_file() or not os.access(SSHD, os.X_OK):
            raise FixtureError(f"OpenSSH server not found at {SSHD}")
        if shutil.which(TMUX) is None:
            raise FixtureError("tmux is required for the OpenSSH fixture")

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

    def start(self) -> None:
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
        try:
            self.process = subprocess.Popen(
                [SSHD, "-D", "-e", "-f", str(self.config)],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                close_fds=True,
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

    def _raise_start_failure(self) -> None:
        if self.process is not None:
            try:
                self.process.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.communicate()
        raise FixtureError("OpenSSH fixture exited before listening")

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
        process = self.process
        self.process = None
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
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
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if args.env_file is not None and args.command:
        parser.error("--env-file is persistent mode and cannot wrap a command")
    if args.env_file is None and not args.command:
        parser.error("provide a command, or use --env-file for persistent mode")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(argv if argv is not None else sys.argv[1:])
    try:
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
                environment = fixture.environment()
                if args.print_fingerprint:
                    print(environment["MEETERM_SSH_FINGERPRINT"])

                if args.env_file is not None:
                    fixture.write_env_file(args.env_file)
                    print(
                        f"OpenSSH fixture ready on {HOST}:{fixture.port}; "
                        f"environment file: {args.env_file}",
                        file=sys.stderr,
                    )
                    _wait_for_signal()
                    return 0
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
