#!/usr/bin/env python3
"""Run an isolated real Herdr fixture for the native Rust integration test.

The Rust test owns the test-only russh endpoint.  This process owns only the
real Herdr 0.9.0 server children and their private config/state directories;
the two processes exchange a small JSON manifest and a bounded READY marker.
No OpenSSH daemon or user configuration is touched here.
"""

from __future__ import annotations

import argparse
import json
import fcntl
import pty
import select
import shlex
import struct
import termios
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


TIMEOUT = 15.0
SESSIONS = ("default", "named-probe")


class FixtureError(RuntimeError):
    pass


def run_checked(command: list[str], env: dict[str, str], *, timeout: float = TIMEOUT) -> str:
    try:
        result = subprocess.run(
            command,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
            text=True,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise FixtureError(f"fixture command did not complete: {command[0]}") from error
    if result.returncode != 0:
        raise FixtureError(f"fixture command failed: {command[0]} (exit {result.returncode})")
    return result.stdout


def wait_for(predicate, description: str, deadline: float) -> None:
    while True:
        if predicate():
            return
        if time.monotonic() >= deadline:
            raise FixtureError(f"timed out waiting for {description}")
        time.sleep(0.05)


class HerdrFixture:
    def __init__(self, root: Path, binary: Path) -> None:
        self.root = root
        self.binary = binary
        self.processes: list[subprocess.Popen[bytes]] = []
        self.logs: list[object] = []
        self.host_key = root / "host_ed25519"
        self.client_key = root / "client_ed25519"
        home = root / "home"
        home.mkdir(parents=True, exist_ok=True)
        self.environment = {
            "PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"),
            "LANG": os.environ.get("LANG", "C.UTF-8"),
            "USER": os.environ.get("USER", "fixture"),
            "LOGNAME": os.environ.get("LOGNAME", os.environ.get("USER", "fixture")),
        }
        self.environment.update(
            {
                "HOME": str(home),
                "XDG_CONFIG_HOME": str(root / "config"),
                "XDG_STATE_HOME": str(root / "state"),
                "XDG_CACHE_HOME": str(root / "cache"),
                "HERDR_CONFIG_PATH": str(root / "config.toml"),
            }
        )
        (root / "config.toml").write_text(
            "onboarding = false\n"
            '[terminal]\ndefault_shell = "/bin/sh"\n'
            "[update]\nversion_check = false\nmanifest_check = false\n"
            "[ui.sound]\nenabled = false\n",
            encoding="utf-8",
        )

    def socket_path(self, session: str) -> Path:
        base = self.root / "config" / "herdr"
        return (base if session == "default" else base / "sessions" / session) / "herdr.sock"

    def cli(self, session: str, *args: str) -> dict:
        output = run_checked(
            [str(self.binary), "--session", session, *args],
            self.environment,
        )
        try:
            value = json.loads(output)
        except json.JSONDecodeError as error:
            raise FixtureError(f"fixture CLI returned non-JSON for {args[0]}") from error
        if not isinstance(value, dict):
            raise FixtureError(f"fixture CLI returned a non-object for {args[0]}")
        return value

    def start(self) -> dict:
        self.generate_key(self.host_key)
        self.generate_key(self.client_key)
        for session in SESSIONS:
            log_path = self.root / f"{session}.log"
            log = log_path.open("wb")
            self.logs.append(log)
            process = subprocess.Popen(
                [str(self.binary), "--session", session, "server"],
                env=self.environment,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
            )
            self.processes.append(process)
            endpoint = self.socket_path(session)

            def ready() -> bool:
                return endpoint.exists() and process.poll() is None

            wait_for(ready, f"Herdr {session} socket", time.monotonic() + TIMEOUT)
            def status_ready() -> bool:
                try:
                    return self.status(session).get("server", {}).get("running") is True
                except FixtureError:
                    return False

            wait_for(status_ready, f"Herdr {session} status", time.monotonic() + TIMEOUT)

        sessions: dict[str, dict[str, str]] = {}
        for session in SESSIONS:
            self.cli(session, "workspace", "create", "--cwd", str(self.root), "--label", f"{session}-workspace", "--focus")
            snapshot = self.cli(session, "api", "snapshot").get("result", {}).get("snapshot")
            if not isinstance(snapshot, dict):
                raise FixtureError(f"Herdr {session} snapshot is missing")
            panes = snapshot.get("panes")
            if not isinstance(panes, list) or not panes:
                raise FixtureError(f"Herdr {session} did not create a root pane")
            pane = panes[0]
            if not isinstance(pane, dict) or not all(isinstance(pane.get(key), str) for key in ("pane_id", "terminal_id")):
                raise FixtureError(f"Herdr {session} root pane identity is invalid")
            sessions[session] = {
                "socket": str(self.socket_path(session)),
                "pane_id": pane["pane_id"],
                "terminal_id": pane["terminal_id"],
            }
        return {
            "binary": str(self.binary),
            "root": str(self.root),
            "host_key": str(self.host_key),
            "client_key": str(self.client_key),
            "environment": self.environment,
            "sessions": sessions,
        }

    @staticmethod
    def generate_key(path: Path) -> None:
        try:
            result = subprocess.run(
                [shutil.which("ssh-keygen") or "ssh-keygen", "-q", "-t", "ed25519",
                 "-f", str(path), "-N", "", "-C", "meeterm-herdr-russh-fixture"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=TIMEOUT,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise FixtureError("ssh-keygen did not complete") from error
        if result.returncode != 0:
            raise FixtureError("ssh-keygen failed for the private fixture key")
        path.chmod(0o600)

    def status(self, session: str) -> dict:
        return self.cli(session, "status", "--json")

    def close(self) -> None:
        for session in reversed(SESSIONS):
            try:
                run_checked(
                    [str(self.binary), "--session", session, "server", "stop"],
                    self.environment,
                    timeout=5.0,
                )
            except FixtureError:
                pass
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
        deadline = time.monotonic() + 5.0
        for process in self.processes:
            remaining = max(0.1, deadline - time.monotonic())
            try:
                process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5.0)
        for log in self.logs:
            log.close()


def serve(binary: Path, root: Path, manifest: Path) -> int:
    root.mkdir(parents=True, exist_ok=False)
    fixture = HerdrFixture(root, binary)
    try:
        value = fixture.start()
        manifest.write_text(json.dumps(value, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"READY {manifest}", flush=True)
        # The Rust test sends one explicit stop line. EOF is also a bounded
        # cleanup signal when the test process is interrupted.
        for line in sys.stdin:
            if line.strip() == "stop":
                break
    except Exception as error:
        print(f"ERROR {type(error).__name__}: {error}", flush=True)
        return 1
    finally:
        fixture.close()
    return 0


def pc_handoff(root: Path, manifest_path: Path) -> int:
    manifest = json.loads(manifest_path.read_text())
    if Path(manifest["root"]).resolve() != root.resolve():
        raise FixtureError("PC handoff must use the isolated fixture root")
    environment = dict(manifest["environment"])
    environment.update(TERM="xterm-256color", COLORTERM="truecolor")
    binary = manifest["binary"]

    def snapshot():
        return json.loads(run_checked([binary, "--session", "default", "api", "snapshot"], environment))["result"]["snapshot"]

    def topology(value):
        return sorted((pane["terminal_id"], pane["workspace_id"], pane["tab_id"]) for pane in value["panes"])

    before = snapshot()
    target = manifest["sessions"]["default"]["terminal_id"]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))

    def controlling_tty():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    process = subprocess.Popen([binary, "--session", "default"], env=environment,
        stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_tty, close_fds=True)
    os.close(slave)
    os.set_blocking(master, False)

    def drain():
        while select.select([master], [], [], 0)[0]:
            try:
                if not os.read(master, 65536):
                    break
            except OSError:
                break
        if process.poll() is not None:
            raise FixtureError("ordinary PC Herdr client exited before input")

    try:
        def pc_ready():
            drain()
            value = snapshot()
            pane = next(pane for pane in value["panes"] if pane["terminal_id"] == target)
            return pane.get("scroll", {}).get("viewport_rows") == 39

        wait_for(pc_ready, "ordinary PC pane geometry", time.monotonic() + TIMEOUT)
        result_path = root / "pc-handoff.txt"
        command = "printf '%s%s:%s\\n' 'PC_' 'HANDOFF' \"$MEETERM_NATIVE_STICKY\" > " + shlex.quote(str(result_path)) + "\r"
        os.write(master, command.encode())

        def input_arrived():
            drain()
            return result_path.exists() and result_path.read_text() == "PC_HANDOFF:6F19\n"

        wait_for(input_arrived, "ordinary PC keyboard input to the retained shell", time.monotonic() + TIMEOUT)
        if topology(snapshot()) != topology(before):
            raise FixtureError("ordinary PC handoff changed pane topology")
        print("PC_HANDOFF_OK rows=39 sticky_shell=retained topology=unchanged", flush=True)
        return 0
    finally:
        os.close(master)
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr", default=shutil.which("herdr"))
    parser.add_argument("--serve", action="store_true")
    parser.add_argument("--pc-handoff", action="store_true")
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    args = parser.parse_args()
    if args.pc_handoff:
        return pc_handoff(args.root.resolve(), args.manifest.resolve())
    if not args.serve:
        parser.error("--serve is required")
    if not args.herdr:
        parser.error("supply --herdr or install Herdr separately")
    binary = Path(args.herdr).resolve(strict=True)
    if args.manifest.exists():
        parser.error("manifest already exists")
    return serve(binary, args.root.resolve(), args.manifest.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
