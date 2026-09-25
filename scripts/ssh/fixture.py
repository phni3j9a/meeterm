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
SSHD_STOP_TIMEOUT_SECONDS = 3.0
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


def _choose_port(excluded: set[int] | None = None) -> int:
    """Ask the kernel for a high loopback port before launching sshd."""

    excluded = excluded or set()
    for _ in range(8):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            probe.bind((HOST, 0))
            port = int(probe.getsockname()[1])
        if port <= 1024:
            raise FixtureError("kernel returned a privileged fixture port")
        if port not in excluded:
            return port
    raise FixtureError("kernel did not provide distinct fixture endpoints")


class Fixture:
    """The files and process for one temporary OpenSSH server."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.port = _choose_port()
        self.alternate_port = _choose_port({self.port})
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
        # that directory inside this fixture so the primary remote endpoint
        # and its local helper see an isolated default server.
        # The product itself still uses ordinary tmux; this is only test
        # isolation, and cleanup below addresses this exact socket.
        self.tmux_tmpdir = root / "tmux"
        # The alternate SSH endpoint intentionally owns a separate ordinary
        # tmux server so the endpoint-switch smoke cannot pass by reconnecting
        # to the primary port.
        self.alternate_tmux_tmpdir = root / "tmux-alternate"
        # tmux appends tmux-$UID below TMUX_TMPDIR before creating its
        # default socket. Keep the fully resolved path so cleanup never has
        # to ask tmux for (or guess at) the caller's ordinary socket.
        self.tmux_socket = self.tmux_tmpdir / f"tmux-{os.getuid()}" / "default"
        self.alternate_tmux_socket = (
            self.alternate_tmux_tmpdir / f"tmux-{os.getuid()}" / "default"
        )
        self.encrypted_passphrase = secrets.token_urlsafe(32)
        self.process: subprocess.Popen[str] | None = None
        self.sshd_listener_identity: str | None = None
        self.sshd_descendants: dict[int, str] = {}
        self.tmux_process: subprocess.Popen[str] | None = None
        self.alternate_tmux_process: subprocess.Popen[str] | None = None
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
        self.alternate_tmux_tmpdir.mkdir(mode=0o700)
        self.alternate_tmux_tmpdir.chmod(mode=0o700)
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
                    f"Port {self.alternate_port}",
                    f"ListenAddress {HOST}",
                    f"HostKey {self.host_key}",
                    f"PidFile {self.pid_file}",
                    f"AuthorizedKeysFile {self.authorized_keys}",
                    f"AllowUsers {self.user}",
                    # Route each SSH listener to a separate ordinary tmux
                    # server in the fixture so the alternate-endpoint smoke
                    # must connect to its selected port.
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
                    f"Match LocalPort {self.port}",
                    f"SetEnv TMUX_TMPDIR={self.tmux_tmpdir}",
                    f"Match LocalPort {self.alternate_port}",
                    f"SetEnv TMUX_TMPDIR={self.alternate_tmux_tmpdir}",
                    "Match all",
                )
            )
            + "\n",
            encoding="utf-8",
        )
        self.config.chmod(0o600)
        _run_quietly([SSHD, "-t", "-f", str(self.config)])

    def _start_tmux_server(
        self, tmux_tmpdir: Path, tmux_socket: Path
    ) -> subprocess.Popen[str]:
        # Start an empty private server without loading ~/.tmux.conf. -D
        # keeps the server alive; the smoke driver seeds its Sessions.
        tmux_socket.parent.mkdir(mode=0o700, exist_ok=True)
        tmux_environment = dict(os.environ)
        tmux_environment.pop("TMUX", None)
        tmux_environment.pop("TMUX_PANE", None)
        tmux_environment["TMUX_TMPDIR"] = str(tmux_tmpdir)
        process = subprocess.Popen(
            [TMUX, "-D", "-f", "/dev/null", "-S", str(tmux_socket)],
            env=tmux_environment, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, text=True,
        )
        deadline = time.monotonic() + READY_TIMEOUT_SECONDS
        while not tmux_socket.exists():
            if process.poll() is not None or time.monotonic() >= deadline:
                if process.poll() is None:
                    process.kill()
                    process.wait()
                raise FixtureError("isolated tmux fixture did not start")
            time.sleep(0.05)
        # Keep disposable panes independent of the developer's interactive
        # shell startup (for example an oh-my-zsh update prompt). These options
        # affect only the absolute fixture socket, never the ordinary server.
        _run_quietly([
            TMUX, "-S", str(tmux_socket),
            "set-option", "-g", "default-shell", "/bin/sh", ";",
            "set-option", "-g", "default-command", "exec /bin/sh -i",
        ])

        return process

    def _start_tmux(self) -> None:
        self.tmux_process = self._start_tmux_server(self.tmux_tmpdir, self.tmux_socket)
        self.alternate_tmux_process = self._start_tmux_server(
            self.alternate_tmux_tmpdir, self.alternate_tmux_socket
        )

    def start_sshd(self) -> None:
        """Start only this fixture's sshd on its original endpoint."""

        with self.sshd_lock:
            if self.sshd_descendants or self.sshd_listener_identity is not None:
                raise FixtureError("previous OpenSSH fixture process tree is still owned")
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
                    with socket.create_connection((HOST, self.port), timeout=0.2), \
                         socket.create_connection((HOST, self.alternate_port), timeout=0.2):
                        listener_identity = self._process_identity(self.process.pid)
                        if listener_identity is None:
                            raise FixtureError(
                                "OpenSSH fixture listener identity is unavailable"
                            )
                        for port in (self.port, self.alternate_port):
                            linux_owners = self._linux_local_port_sshd_process_ids(port)
                            if linux_owners is not None and self.process.pid not in linux_owners:
                                raise FixtureError("OpenSSH fixture listener socket ownership is uncertain")
                        self.sshd_listener_identity = listener_identity
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
    def _process_group_member_ids(process_group_id: int) -> list[int]:
        """Return members of the fixture-owned sshd process group."""

        try:
            result = subprocess.run(
                ["ps", "-axo", "pid=,pgid="],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=3,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            return []
        members = []
        for line in result.stdout.splitlines():
            fields = line.split()
            if len(fields) != 2:
                continue
            try:
                process_id, group_id = (int(field) for field in fields)
            except ValueError:
                continue
            if process_id > 0 and process_id != process_group_id and group_id == process_group_id:
                members.append(process_id)
        return members

    @staticmethod
    def _linux_local_port_sshd_process_ids(
        port: int,
        proc_root: Path = Path("/proc"),
    ) -> set[int] | None:
        """Return identity-checkable sshd owners of one Linux local port.

        OpenSSH may move an accepted session into another process group and
        reparent it before the listener exits.  On Linux, bind the stop
        boundary to the fixture's still-live socket owners as well as the
        process tree.  A missing or ambiguous owner fails closed; ownerless
        TIME_WAIT records have inode zero and are deliberately ignored.
        """

        if not sys.platform.startswith("linux"):
            return None
        if type(port) is not int or not 1 <= port <= 65535:
            raise FixtureError("OpenSSH fixture port is invalid")

        socket_uids: dict[int, int] = {}
        for table_name in ("tcp", "tcp6"):
            table = proc_root / "net" / table_name
            try:
                lines = table.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError) as error:
                raise FixtureError(
                    "OpenSSH fixture socket ownership is unavailable"
                ) from error
            for line in lines[1:]:
                fields = line.split()
                if len(fields) < 10:
                    continue
                try:
                    local_port = int(fields[1].rsplit(":", 1)[1], 16)
                    socket_uid = int(fields[7])
                    socket_inode = int(fields[9])
                except (IndexError, ValueError):
                    continue
                if local_port == port and socket_inode > 0:
                    socket_uids[socket_inode] = socket_uid

        if not socket_uids:
            return set()
        current_uid = os.getuid()
        if any(uid != current_uid for uid in socket_uids.values()):
            raise FixtureError("OpenSSH fixture socket owner is outside the fixture user")

        expected_executable = Path(SSHD).resolve()
        owners: dict[int, set[int]] = {inode: set() for inode in socket_uids}
        try:
            process_directories = [
                entry
                for entry in proc_root.iterdir()
                if entry.name.isdecimal() and entry.is_dir()
            ]
        except OSError as error:
            raise FixtureError(
                "OpenSSH fixture socket ownership is unavailable"
            ) from error

        for process_directory in process_directories:
            process_id = int(process_directory.name)
            file_descriptors = process_directory / "fd"
            try:
                descriptor_paths = list(file_descriptors.iterdir())
            except (FileNotFoundError, PermissionError, OSError):
                continue
            owned_inodes = set()
            for descriptor_path in descriptor_paths:
                try:
                    target = os.readlink(descriptor_path)
                except (FileNotFoundError, PermissionError, OSError):
                    continue
                match = re.fullmatch(r"socket:\[(\d+)\]", target)
                if match is not None:
                    inode = int(match.group(1))
                    if inode in owners:
                        owned_inodes.add(inode)
            if not owned_inodes:
                continue

            try:
                status_lines = (process_directory / "status").read_text(
                    encoding="utf-8"
                ).splitlines()
                uid_line = next(line for line in status_lines if line.startswith("Uid:"))
                process_uid = int(uid_line.split()[1])
                executable = (process_directory / "exe").resolve(strict=True)
            except (OSError, UnicodeError, StopIteration, IndexError, ValueError) as error:
                raise FixtureError("OpenSSH fixture socket owner is uncertain") from error
            if process_uid != current_uid or executable != expected_executable:
                raise FixtureError("OpenSSH fixture socket owner is uncertain")
            for inode in owned_inodes:
                owners[inode].add(process_id)

        if any(not process_ids for process_ids in owners.values()):
            raise FixtureError("OpenSSH fixture socket owner is unavailable")
        return {
            process_id
            for process_ids in owners.values()
            for process_id in process_ids
        }

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

    @staticmethod
    def _process_identity(process_id: int) -> str | None:
        """Read a process start identity without trusting a recyclable PID."""

        proc_stat = Path(f"/proc/{process_id}/stat")
        try:
            contents = proc_stat.read_text(encoding="utf-8")
            suffix = contents.rsplit(")", 1)[1].split()
            # /proc stat field 22 is the kernel start tick.  The suffix starts
            # at field 3 because the command in field 2 may contain spaces.
            if len(suffix) > 19:
                return f"proc:{suffix[19]}"
        except (FileNotFoundError, IndexError, OSError, UnicodeError):
            pass
        try:
            result = subprocess.run(
                ["ps", "-o", "lstart=", "-p", str(process_id)],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=1,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            return None
        started = result.stdout.strip()
        return f"ps:{started}" if result.returncode == 0 and started else None

    @staticmethod
    def _process_exists(process_id: int) -> bool:
        try:
            os.kill(process_id, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True
        except OSError:
            return False
        return True

    @classmethod
    def _capture_process_identities(cls, process_ids: Sequence[int]) -> dict[int, str]:
        """Capture descendants, failing closed when a live PID cannot be identified."""

        captured = {}
        for process_id in process_ids:
            identity = cls._process_identity(process_id)
            if identity is not None:
                captured[process_id] = identity
            elif cls._process_exists(process_id):
                raise FixtureError("OpenSSH fixture process identity is unavailable")
        return captured

    @classmethod
    def _live_process_identities(cls, processes: dict[int, str]) -> dict[int, str]:
        """Return still-live owned descendants and reject PID reuse."""

        live = {}
        for process_id, expected_identity in processes.items():
            identity = cls._process_identity(process_id)
            if identity == expected_identity:
                live[process_id] = expected_identity
            elif identity is not None or cls._process_exists(process_id):
                raise FixtureError("OpenSSH fixture process identity changed")
        return live

    @classmethod
    def _signal_process_identities(
        cls,
        processes: dict[int, str],
        signum: int,
    ) -> None:
        """Signal only descendants whose captured start identity still matches."""

        for process_id in cls._live_process_identities(processes):
            try:
                os.kill(process_id, signum)
            except ProcessLookupError:
                pass
            except OSError as error:
                raise FixtureError("OpenSSH fixture process could not be signaled") from error

    @classmethod
    def _wait_for_process_ids_exit(
        cls,
        processes: dict[int, str],
        timeout: float = SSHD_STOP_TIMEOUT_SECONDS,
    ) -> dict[int, str]:
        """Wait boundedly for captured descendants and return any survivors."""

        deadline = time.monotonic() + timeout
        remaining = cls._live_process_identities(processes)
        while remaining and time.monotonic() < deadline:
            time.sleep(CONTROL_POLL_SECONDS)
            remaining = cls._live_process_identities(remaining)
        return remaining

    def stop_sshd(self) -> None:
        """Stop sshd and its accepted-session children, retaining tmux."""

        with self.sshd_lock:
            process = self.process
            if process is None:
                if self.sshd_descendants or self.sshd_listener_identity is not None:
                    raise FixtureError("OpenSSH fixture process ownership is incomplete")
                return
            if process.poll() is not None:
                try:
                    process.communicate(timeout=1)
                except subprocess.TimeoutExpired:
                    pass
                if not self.sshd_descendants:
                    raise FixtureError("OpenSSH fixture exited before its process tree was captured")
            elif (
                self.sshd_listener_identity is None
                or self._process_identity(process.pid) != self.sshd_listener_identity
            ):
                raise FixtureError("OpenSSH fixture listener identity changed")

            # A session child normally inherits the new process group. Keep
            # the descendant list as a portable fallback for sshd variants
            # that move an accepted session to another process group.
            if not self.sshd_descendants:
                owned_process_ids = set(self._process_group_member_ids(process.pid))
                owned_process_ids.update(self._descendant_process_ids(process.pid))
                for port in (self.port, self.alternate_port):
                    linux_owners = self._linux_local_port_sshd_process_ids(port)
                    if linux_owners is not None:
                        owned_process_ids.update(linux_owners)
                owned_process_ids.discard(process.pid)
                self.sshd_descendants = self._capture_process_identities(
                    sorted(owned_process_ids)
                )
            remaining = dict(self.sshd_descendants)
            parent_exited = process.poll() is not None
            if not parent_exited:
                self._signal_process_group(process, signal.SIGTERM)
            self._signal_process_identities(remaining, signal.SIGTERM)
            if not parent_exited:
                try:
                    process.communicate(timeout=SSHD_STOP_TIMEOUT_SECONDS)
                    parent_exited = True
                except subprocess.TimeoutExpired:
                    pass

            remaining = self._wait_for_process_ids_exit(remaining)
            if not parent_exited or remaining:
                # The listener can exit before an accepted-session child on
                # Linux.  A stop acknowledgement is the transport-loss test
                # boundary, so do not publish it while that connection can
                # still carry bytes.
                if not parent_exited:
                    self._signal_process_group(process, signal.SIGKILL)
                self._signal_process_identities(remaining, signal.SIGKILL)
                if not parent_exited:
                    try:
                        process.communicate(timeout=SSHD_STOP_TIMEOUT_SECONDS)
                        parent_exited = True
                    except subprocess.TimeoutExpired:
                        pass
                remaining = self._wait_for_process_ids_exit(remaining)

            if not parent_exited or remaining:
                raise FixtureError("OpenSSH fixture process tree did not stop")

            for port in (self.port, self.alternate_port):
                linux_owners = self._linux_local_port_sshd_process_ids(port)
                if linux_owners:
                    # Do not capture or signal a process discovered only after
                    # the verified listener exited: the port may already have
                    # been reused by another identity. Refuse the stop ACK.
                    raise FixtureError("OpenSSH fixture socket owners did not stop")
            self.process = None
            self.sshd_listener_identity = None
            self.sshd_descendants = {}

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
            contents = self.control_request_path.read_text(encoding="utf-8")
        except (FileNotFoundError, OSError, UnicodeError):
            return None
        # A partial write is left for the sender to finish.
        if not contents.endswith("\n"):
            return None
        request = self._parse_control_request(request_stat, contents)
        if request is None:
            # A malformed complete request is removed so it cannot be retried
            # indefinitely; valid requests always use the exact one-line
            # protocol below.  Decide from this single read: reading again
            # could observe a sender that finished writing after the first
            # read and delete its valid request unprocessed.
            try:
                self.control_request_path.unlink()
            except (FileNotFoundError, OSError):
                pass
        return request

    @staticmethod
    def _parse_control_request(request_stat: os.stat_result, contents: str) -> tuple[str, str] | None:
        if request_stat.st_uid != os.getuid() or request_stat.st_mode & 0o077:
            return None
        if contents.count("\n") != 1:
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
        """Prove both authenticated fixture endpoints reach the same tmux server."""
        public_key = self.host_key.with_name(self.host_key.name + ".pub").read_text().strip()
        self.trust_store.write_text(
            f"[{HOST}]:{self.port} {public_key}\n"
            f"[{HOST}]:{self.alternate_port} {public_key}\n"
        )
        for port in (self.port, self.alternate_port):
            _run_quietly([
                "ssh", "-F", "/dev/null",
                "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
                "-o", "IdentityAgent=none", "-o", "ConnectTimeout=5",
                "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=1",
                "-o", "StrictHostKeyChecking=yes", "-o", "GlobalKnownHostsFile=/dev/null",
                "-o", f"UserKnownHostsFile={self.trust_store}",
                "-i", str(self.client_key), "-p", str(port),
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
            "MEETERM_SSH_ALTERNATE_PORT": str(self.alternate_port),
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
            # endpoint-specific path through Match/SetEnv above.
            "MEETERM_TMUX_TMPDIR": str(self.tmux_tmpdir),
            "MEETERM_TMUX_SOCKET": str(self.tmux_socket),
            "MEETERM_TMUX_ALTERNATE_TMPDIR": str(self.alternate_tmux_tmpdir),
            "MEETERM_TMUX_ALTERNATE_SOCKET": str(self.alternate_tmux_socket),
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
        sshd_error: FixtureError | None = None
        try:
            self.stop_sshd()
        except FixtureError as error:
            # Continue with the independently owned tmux and file cleanup,
            # then preserve the fail-closed sshd diagnostic for the caller.
            sshd_error = error
        # A tmux server outlives the sshd process that created it.  Kill only
        # this fixture's absolute socket before TemporaryDirectory removes
        # the socket directory; never invoke the default client without -S,
        # because that could reach the developer's ordinary tmux server.
        tmux = shutil.which(TMUX)
        for socket_path, process_name in (
            (self.tmux_socket, "tmux_process"),
            (self.alternate_tmux_socket, "alternate_tmux_process"),
        ):
            if tmux is not None:
                try:
                    subprocess.run(
                        [tmux, "-S", str(socket_path), "kill-server"],
                        check=False,
                        stdin=subprocess.DEVNULL,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        timeout=3,
                    )
                except (OSError, subprocess.TimeoutExpired):
                    # Missing sockets or exited servers are harmless on cleanup.
                    pass
            tmux_process = getattr(self, process_name)
            setattr(self, process_name, None)
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
        if sshd_error is not None:
            raise sshd_error


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
