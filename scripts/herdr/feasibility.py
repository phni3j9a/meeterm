#!/usr/bin/env python3
"""Probe an installed Herdr through disposable, host-key-verified OpenSSH.

This is an opt-in integration diagnostic, not the meeterm Herdr backend. It
never starts, stops, installs, or updates a user's Herdr session. All server
state, SSH keys, sockets and child processes belong to a temporary fixture.
Only synthetic terminal output and assertion results leave that fixture.
"""

from __future__ import annotations

import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts" / "ssh"))
from fixture import Fixture  # noqa: E402

TIMEOUT = 10.0
MAX_RECORD = 4 * 1024 * 1024


class ProbeError(RuntimeError):
    pass


class Stream:
    """A bounded NDJSON reader; never turns EOF or a timeout into success."""

    def __init__(self, process: subprocess.Popen[bytes]):
        self.process = process
        self.buffer = bytearray()
        self.frames: list[dict] = []
        self.selector = selectors.DefaultSelector()
        self.selector.register(process.stdout, selectors.EVENT_READ)

    def record(self, deadline: float | None = None) -> dict:
        deadline = deadline if deadline is not None else time.monotonic() + TIMEOUT
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                raise ProbeError("terminal record deadline exceeded")
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise ProbeError("terminal stream ended before expected record")
            self.buffer.extend(chunk)
            if len(self.buffer) > MAX_RECORD:
                raise ProbeError("terminal record exceeds diagnostic limit")
        line, _, rest = self.buffer.partition(b"\n")
        self.buffer = bytearray(rest)
        result = json.loads(line)
        if result.get("type") == "terminal.frame":
            if result.get("encoding") != "ansi":
                raise ProbeError("unsupported terminal frame encoding")
            base64.b64decode(result["bytes"], validate=True)
            self.frames.append(result)
        return result

    def until(self, marker: str) -> dict:
        deadline = time.monotonic() + TIMEOUT
        while True:
            record = self.record(deadline)
            if record.get("type") == "terminal.closed":
                raise ProbeError("terminal closed before expected frame")
            # A render delta can split a word across cursor-position commands
            # and omit unchanged cells. Search the real native snapshot, never
            # raw ANSI or regex-stripped text. Each replay follows a new frame;
            # this is not periodic remote pane.read polling.
            with tempfile.TemporaryDirectory(prefix="mh17-native-") as temporary:
                directory = Path(temporary) / "replay"
                replay_native(self.frames, directory, timeout=max(0.1, deadline - time.monotonic()))
                if marker in snapshot_text(directory / "snapshot.bin"):
                    return record

    def send(self, command: dict) -> None:
        self.process.stdin.write((json.dumps(command) + "\n").encode())
        self.process.stdin.flush()

    def input(self, data: bytes) -> None:
        self.send({"type": "terminal.input", "bytes": base64.b64encode(data).decode()})

    def release(self) -> None:
        self.send({"type": "terminal.release"})
        if self.process.wait(timeout=TIMEOUT) != 0:
            raise ProbeError("terminal release returned nonzero exit")
        self.selector.close()

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=TIMEOUT)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=TIMEOUT)
        self.selector.close()
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            if pipe:
                pipe.close()


def replay_native(frames: list[dict], destination: Path, timeout: float = 30.0) -> dict:
    destination.mkdir()
    with (destination / "frames.bin").open("wb") as output:
        for frame in frames:
            payload = base64.b64decode(frame["bytes"], validate=True)
            output.write(struct.pack("<HHI", frame["width"], frame["height"], len(payload)))
            output.write(payload)
    env = dict(os.environ, MEETERM_HERDR_REPLAY_DIR=str(destination))
    with (destination / "cargo.log").open("wb") as log:
        result = subprocess.run(
            ["cargo", "test", "--locked", "--lib", "herdr_probe_tests::replay_live_frames",
             "--", "--ignored", "--exact"],
            cwd=REPO / "native" / "meeterm-core", env=env,
            stdout=log, stderr=subprocess.STDOUT, timeout=timeout,
        )
    if result.returncode:
        raise ProbeError("native replay failed; see its cargo.log")
    # Requiring the result file also catches a misspelled filter / zero tests.
    return json.loads((destination / "native.json").read_text())


def snapshot_text(path: Path) -> str:
    """Read the existing native-only MTRM snapshot, outside JavaScript."""
    data = path.read_bytes()
    if data[:4] != b"MTRM" or len(data) < 28:
        raise ProbeError("invalid native snapshot")
    cells = []
    offset = 28
    while offset < len(data):
        if len(data) - offset < 28:
            raise ProbeError("truncated native cell")
        # Metadata: row/column u32, width u8, padding u8, flags u16,
        # foreground/background RGBA, base/combining byte lengths u32.
        base_len, combining_len = struct.unpack_from("<II", data, offset + 20)
        offset += 28
        end = offset + base_len + combining_len
        if end > len(data):
            raise ProbeError("truncated native cell text")
        cells.append(data[offset:end].decode("utf-8"))
        offset = end
    return "".join(cells)


class HerdrFixture:
    def __init__(self, root: Path, binary: str):
        self.root, self.binary = root, binary
        ssh_root = root / "ssh"
        ssh_root.mkdir()
        self.sshd = Fixture(ssh_root)
        self.processes: list[subprocess.Popen] = []
        self.streams: list[Stream] = []
        self.logs = []
        self.environment = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        self.overrides = {
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_STATE_HOME": str(root / "state"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "HERDR_CONFIG_PATH": str(root / "config.toml"),
        }
        self.environment.update(self.overrides)
        (root / "config.toml").write_text(
            'onboarding = false\n[terminal]\ndefault_shell = "/bin/sh"\n'
            '[update]\nversion_check = false\nmanifest_check = false\n'
            '[ui.sound]\nenabled = false\n'
        )

    def start(self) -> None:
        self.sshd.prepare()
        self.sshd.start()
        self.sshd.check_ssh_tmux()
        for session in ("default", "named-probe"):
            log = (self.root / f"{session}.log").open("wb")
            self.logs.append(log)
            process = subprocess.Popen(
                [self.binary, "--session", session, "server"], env=self.environment,
                stdin=subprocess.DEVNULL, stdout=log, stderr=log,
            )
            self.processes.append(process)
            endpoint = self.api_path(session)
            deadline = time.monotonic() + TIMEOUT
            while not endpoint.exists():
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise ProbeError("isolated Herdr server did not become ready")
                time.sleep(0.05)
            # Socket existence is followed by a real SSH/API command below.

    def api_path(self, session: str) -> Path:
        base = self.root / "config" / "herdr"
        return (base if session == "default" else base / "sessions" / session) / "herdr.sock"

    def ssh(self) -> list[str]:
        f = self.sshd
        return [
            "ssh", "-F", "/dev/null", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
            "-o", "IdentityAgent=none", "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=yes", "-o", "GlobalKnownHostsFile=/dev/null",
            "-o", f"UserKnownHostsFile={f.trust_store}",
            "-i", str(f.client_key), "-p", str(f.port),
        ]

    def command(self, session: str, *args: str) -> list[str]:
        # OpenSSH exec is a shell boundary: quote every argv element. These
        # overrides isolate the fixture, not a proposed product requirement.
        command = ["env"]
        for key in ("HERDR_SESSION", "HERDR_SOCKET_PATH", "HERDR_CLIENT_SOCKET_PATH"):
            command += ["-u", key]
        command += [f"{key}={value}" for key, value in self.overrides.items()]
        command += [self.binary, "--session", session, *args]
        return [*self.ssh(), f"{self.sshd.user}@127.0.0.1", shlex.join(command)]

    def cli(self, session: str, *args: str) -> dict:
        result = subprocess.run(self.command(session, *args), capture_output=True, timeout=TIMEOUT)
        if result.returncode:
            raise ProbeError(f"Herdr CLI failed: {args[0]} (exit {result.returncode})")
        return json.loads(result.stdout)

    def stream(self, session: str, pane: str, mode: str = "control", cols=40, rows=16) -> Stream:
        process = subprocess.Popen(
            self.command(session, "terminal", "session", mode, pane,
                         "--cols", str(cols), "--rows", str(rows)),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        stream = Stream(process)
        self.streams.append(stream)
        return stream

    def close(self) -> None:
        for stream in reversed(self.streams):
            stream.close()
        # Only the two exact test sessions in this isolated config directory.
        for session in ("default", "named-probe"):
            try:
                subprocess.run(
                    [self.binary, "--session", session, "server", "stop"],
                    env=self.environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    timeout=TIMEOUT,
                )
            except subprocess.TimeoutExpired:
                pass
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=TIMEOUT)
        for log in self.logs:
            log.close()
        self.sshd.stop()


def probe(fixture: HerdrFixture, output: Path, report: dict) -> None:
    def check(name: str, passed: bool, **details) -> None:
        report["checks"].append({"name": name, "passed": passed, **details})
        print(f"{'PASS' if passed else 'FAIL'} {name}", flush=True)

    baseline = output / "raw-mode-baseline"
    replay_native([{"width": 52, "height": 20, "bytes": base64.b64encode(
        b"\x1b[?1049h\x1b[?1h\x1b[?2004h\x1b[2J\x1b[HDECCKM_READY"
    ).decode()}], baseline)
    check("native_raw_mode_baseline", (baseline / "up.bin").read_bytes() == b"\x1bOA"
          and (baseline / "paste.bin").read_bytes().startswith(b"\x1b[200~"))
    default = fixture.cli("default", "workspace", "create", "--cwd", str(fixture.root),
                          "--label", "default-probe", "--no-focus")["result"]
    named = fixture.cli("named-probe", "workspace", "create", "--cwd", str(fixture.root),
                        "--label", "named-probe", "--no-focus")["result"]
    pane = default["root_pane"]["pane_id"]
    snapshot = fixture.cli("default", "api", "snapshot")["result"]["snapshot"]
    other = fixture.cli("named-probe", "api", "snapshot")["result"]["snapshot"]
    report["herdr_version"], report["protocol"] = snapshot["version"], snapshot["protocol"]
    check("default_and_named_runtime_isolation",
          snapshot["workspaces"][0]["label"] == "default-probe"
          and other["workspaces"][0]["label"] == "named-probe"
          and pane == named["root_pane"]["pane_id"], colliding_public_pane_id=pane)
    stream = fixture.stream("default", pane)
    first = stream.record()
    check("initial_native_frame", first.get("type") == "terminal.frame" and first["full"]
          and (first["width"], first["height"]) == (40, 16))
    # Avoid matching the echoed command itself: completion is composed remotely.
    stream.input(b"printf '\\033[2J\\033[H'; printf 'SHELL_%s\\n' READY; stty size\n")
    stream.until("SHELL_READY")
    stream.send({"type": "terminal.resize", "cols": 52, "rows": 20})
    deadline = time.monotonic() + TIMEOUT
    while True:
        frame = stream.record(deadline)
        if frame.get("width") == 52 and frame.get("height") == 20:
            break
    stream.input("printf '\\033[2J\\033[H'; printf '日本語 é 😀\\n'; stty size; printf 'RESIZE_%s\\n' READY\n".encode())
    stream.until("RESIZE_READY")
    replay_native(stream.frames, output / "shell")
    text = snapshot_text(output / "shell" / "snapshot.bin")
    check("shell_cjk_and_actual_pty_resize", "日本語" in text and "20 52" in text
          and "RESIZE_READY" in text)
    rival = fixture.stream("default", pane)
    rejection = rival.record()
    check("second_direct_controller_rejected_without_takeover",
          rejection.get("type") == "terminal.closed" and "already has" in rejection.get("reason", ""))
    rival.close()

    # A real full-screen raw-mode program, deliberately requiring DECCKM.
    tui = fixture.root / "tui.py"
    result_path = fixture.root / "tui-result.json"
    tui.write_text(
        "import os,sys,tty,termios,json,select\n"
        "fd=0; old=termios.tcgetattr(fd)\n"
        "try:\n"
        " tty.setraw(fd)\n"
        " os.write(1,b'\\x1b[?1049h\\x1b[?1h\\x1b[?2004h\\x1b[2J\\x1b[HDECCKM_READY')\n"
        " results={}\n"
        " for name in ['up','api_up']:\n"
        "  data=b''\n"
        "  while len(data)<3:\n"
        "   if not select.select([fd],[],[],10)[0]: raise RuntimeError('input deadline')\n"
        "   data+=os.read(fd,3-len(data))\n"
        "  results[name+'_hex']=data.hex()\n"
        f"  open({str(result_path)!r},'w').write(json.dumps(results))\n"
        "  os.write(1,b'\\r\\n'+name.upper().encode()+b'_RESULT_'+data.hex().encode())\n"
        " data=b''\n"
        " while not data.endswith(b'\\x1b[201~'):\n"
        "  if not select.select([fd],[],[],10)[0]: raise RuntimeError('paste deadline')\n"
        "  data+=os.read(fd,64)\n"
        "  if len(data)>128: raise RuntimeError('unexpected paste size')\n"
        " results['paste_hex']=data.hex()\n"
        f" open({str(result_path)!r},'w').write(json.dumps(results))\n"
        " os.write(1,b'\\r\\nPASTE_RESULT_READY')\n"
        " if select.select([fd],[],[],10)[0]: os.read(fd,1)\n"
        "finally:\n"
        " os.write(1,b'\\x1b[?1l\\x1b[?2004l\\x1b[?1049l')\n"
        " termios.tcsetattr(fd,termios.TCSANOW,old)\n"
    )
    stream.input((shlex.join([sys.executable, str(tui)]) + "\n").encode())
    stream.until("DECCKM_READY")
    native = replay_native(stream.frames, output / "tui")
    check("fullscreen_native_snapshot", "DECCKM_READY" in snapshot_text(output / "tui" / "snapshot.bin"))
    up = (output / "tui" / "up.bin").read_bytes()
    stream.input(up)
    stream.until("UP_RESULT_")
    actual = json.loads(result_path.read_text())["up_hex"]
    check("application_cursor_key_round_trip", actual == "1b4f41",
          expected_hex="1b4f41", native_hex=up.hex(), remote_hex=actual)
    paste = (output / "tui" / "paste.bin").read_bytes()
    check("native_bracketed_paste_mode", paste.startswith(b"\x1b[200~") and paste.endswith(b"\x1b[201~"),
          native_hex=paste.hex())
    # A separate automation client can encode the key correctly, but it does
    # not prove ownership of our active terminal controller. Record that this
    # alternate route writes despite lacking the control stream's lease.
    api_result = subprocess.run(fixture.command("default", "pane", "send-keys", pane, "up"),
                                capture_output=True, timeout=TIMEOUT)
    if api_result.returncode:
        raise ProbeError("automation key command rejected; inspect the changed backend contract")
    stream.until("API_UP_RESULT_")
    api_actual = json.loads(result_path.read_text())["api_up_hex"]
    report["observations"] = [{
        "name": "automation_key_bypasses_direct_controller_lease",
        "api_accepted": api_result.returncode == 0,
        "remote_hex": api_actual,
        "direct_controller_still_connected": stream.process.poll() is None,
        "meaning": "Correct logical-key encoding is available, but this API does not enforce the direct controller lease.",
    }]
    wrapped = b"\x1b[200~" + paste + b"\x1b[201~"
    stream.input(wrapped)
    stream.until("PASTE_RESULT_READY")
    check("explicit_control_paste_envelope", json.loads(result_path.read_text())["paste_hex"] == wrapped.hex())
    # Terminating the SSH client simulates losing the mobile process. Keep the
    # full-screen process waiting for q; attempt a new controller only once.
    # Headless Herdr may retain the last PTY size until a desktop client joins.
    stream.close()
    restored = fixture.cli("default", "api", "snapshot")["result"]["snapshot"]
    report["observations"].append({
        "name": "headless_geometry_after_ssh_loss",
        "viewport_rows": restored["panes"][0]["scroll"]["viewport_rows"],
        "meaning": "No desktop client is attached; this is not desktop handoff evidence.",
    })
    reopened = fixture.stream("default", pane)
    reconnect_frame = reopened.record()
    replay_native(reopened.frames, output / "reconnect")
    check("ssh_loss_and_fullscreen_reconnect", reconnect_frame.get("full") is True
          and "DECCKM_READY" in snapshot_text(output / "reconnect" / "snapshot.bin")
          and restored["panes"][0]["terminal_id"] == default["root_pane"]["terminal_id"]
          and restored["layouts"] == snapshot["layouts"])
    reopened.input(b"q")
    reopened.input(b"printf 'RESUMED_%s\\n' READY\n")
    reopened.until("RESUMED_READY")
    reopened.release()
    last = fixture.stream("default", pane)
    check("release_and_reconnect_same_remote_terminal", last.record().get("full") is True
          and fixture.cli("default", "api", "snapshot")["result"]["snapshot"]["panes"][0]["terminal_id"]
          == default["root_pane"]["terminal_id"])
    last.release()
    report["native_replay"] = native


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr", default=shutil.which("herdr"))
    parser.add_argument("--output", required=True, type=Path,
                        help="new directory for synthetic evidence (must not already exist)")
    args = parser.parse_args()
    if not args.herdr:
        parser.error("install Herdr separately or supply --herdr; the probe never installs it")
    binary = str(Path(args.herdr).resolve(strict=True))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {"checks": [], "scope": "Linux OpenSSH CLI + shared Rust terminal; no mobile UI/GPU/IME evidence"}
    report["started_at"] = datetime.now(timezone.utc).isoformat()
    paths = ["scripts/herdr/feasibility.py", "native/meeterm-core/src/herdr_probe_tests.rs",
             "native/meeterm-core/src/terminal.rs", "native/meeterm-core/src/input.rs",
             "native/meeterm-core/Cargo.lock"]
    report["source_sha256"] = {path: hashlib.sha256((REPO / path).read_bytes()).hexdigest() for path in paths}
    with Path(binary).open("rb") as executable:
        report["herdr_binary_sha256"] = hashlib.file_digest(executable, "sha256").hexdigest()
    try:
        # Compile before starting any remote input/readiness deadline.
        with (output / "native-build.log").open("wb") as log:
            built = subprocess.run(
                ["cargo", "test", "--locked", "--lib", "herdr_probe_tests::replay_live_frames", "--no-run"],
                cwd=REPO / "native" / "meeterm-core", stdout=log, stderr=subprocess.STDOUT, timeout=300,
            )
        if built.returncode:
            raise ProbeError("native probe build failed; see native-build.log")
        # sshd StrictModes checks parent directories too. A private directory
        # beneath the current user's home works on hosts where /tmp's ownership
        # is not accepted, while keeping host-key verification fully enabled.
        with tempfile.TemporaryDirectory(prefix=".mh17-", dir=Path.home()) as temporary:
            fixture = HerdrFixture(Path(temporary), binary)
            try:
                fixture.start()
                probe(fixture, output, report)
            finally:
                # These streams contain only commands/output from this fixture,
                # never SSH credentials or an existing user's terminal.
                for index, stream in enumerate(fixture.streams):
                    (output / f"stream-{index}.json").write_text(json.dumps(stream.frames) + "\n")
                fixture.close()
    except Exception as error:
        # Synthetic diagnostics only: do not copy raw sshd/server logs or keys.
        report["error"] = type(error).__name__ + ": " + str(error)
        print(report["error"], file=sys.stderr)
    passed = bool(report["checks"]) and not report.get("error") and all(c["passed"] for c in report["checks"])
    report["passed"] = passed
    (output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
