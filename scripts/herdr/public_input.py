#!/usr/bin/env python3
"""Exercise Herdr's existing public input API beside one direct controller.

This is a bounded, opt-in diagnostic.  It creates only a private Herdr and
OpenSSH fixture and uses the existing Herdr CLI over SSH to send synthetic
input to a test TUI.  It does not modify Herdr or the
meeterm runtime and does not connect to a user's existing session.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts" / "herdr"))
from feasibility import (  # noqa: E402
    HerdrFixture,
    ProbeError,
    Stream,
    TIMEOUT,
    replay_native,
    snapshot_text,
)

REFERENCE_SOURCE_COMMIT = "b99002ac99b09e00b4ca692436cb15a6b0d676f1"


class PublicInputClient:
    """Sequential diagnostic client; not the production Rust input actor.

    Herdr's CLI waits for one public API response per invocation. The diagnostic
    stops subsequent invocations after observing the direct stream's closure.
    An already submitted request cannot be recalled during external takeover.
    """

    def __init__(self, fixture: HerdrFixture):
        self.fixture = fixture
        self.disabled_reason: str | None = None
        self.requests_written = 0

    def disable(self, reason: str) -> None:
        self.disabled_reason = reason

    def send(self, operation: str, pane_id: str, *values: str) -> None:
        if self.disabled_reason is not None:
            raise ProbeError("public input disabled after controller closure")
        result = subprocess.run(
            self.fixture.command("default", "pane", operation, pane_id, *values),
            capture_output=True, timeout=TIMEOUT,
        )
        self.requests_written += 1
        if result.returncode:
            raise ProbeError(f"Herdr public input command failed: {operation}")

    def send_keys(self, pane_id: str, *keys: str) -> None:
        self.send("send-keys", pane_id, *keys)

    def send_text(self, pane_id: str, text: str) -> None:
        self.send("send-text", pane_id, text)


def control_stream(fixture: HerdrFixture, session: str, pane: str, takeover: bool = False) -> Stream:
    arguments = [
        "terminal",
        "session",
        "control",
        pane,
        "--cols",
        "48",
        "--rows",
        "18",
    ]
    if takeover:
        arguments.append("--takeover")
    process = subprocess.Popen(
        fixture.command(session, *arguments),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    stream = Stream(process)
    fixture.streams.append(stream)
    return stream


def expect_closed(stream: Stream) -> dict:
    deadline = time.monotonic() + TIMEOUT
    while True:
        record = stream.record(deadline)
        if record.get("type") == "terminal.closed":
            return record


TUI_SOURCE = r'''import json, os, select, sys, termios, time, tty

result_path = sys.argv[1]
fd = 0
old = termios.tcgetattr(fd)

def read_n(size):
    data = b""
    deadline = time.monotonic() + 10
    while len(data) < size:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise RuntimeError("input deadline")
        data += os.read(fd, size - len(data))
    return data

def read_until(suffix):
    data = b""
    deadline = time.monotonic() + 10
    while not data.endswith(suffix):
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
            raise RuntimeError("paste deadline")
        data += os.read(fd, 128)
        if len(data) > 512:
            raise RuntimeError("unexpected input size")
    return data

def save(**values):
    with open(result_path, "w", encoding="utf-8") as output:
        json.dump(values, output)

try:
    tty.setraw(fd)
    os.write(1, b"\x1b[?1049h\x1b[?1l\x1b[2J\x1b[HNORMAL_READY")
    normal = read_n(3)
    save(normal_hex=normal.hex())
    os.write(1, b"\r\nNORMAL_RESULT_" + normal.hex().encode())
    os.write(1, b"\x1b[?1h\r\nDECCKM_READY")
    decckm = read_n(3)
    save(normal_hex=normal.hex(), decckm_hex=decckm.hex())
    os.write(1, b"\r\nDECCKM_RESULT_" + decckm.hex().encode())
    os.write(1, b"\x1b[?2004h\r\nPASTE_READY")
    paste = read_until(b"\x1b[201~")
    save(normal_hex=normal.hex(), decckm_hex=decckm.hex(), paste_hex=paste.hex())
    os.write(1, b"\r\nPASTE_RESULT_" + paste.hex().encode())
    os.write(1, b"\x1b[?2004l\r\nTARGET_READY")
    target = read_n(9)
    save(normal_hex=normal.hex(), decckm_hex=decckm.hex(), paste_hex=paste.hex(), target_hex=target.hex())
    os.write(1, b"\r\nTARGET_RESULT_" + target.hex().encode() + b"\r\nTUI_WAITING")
    read_n(1)
    os.write(1, b"\r\nTUI_DONE")
finally:
    os.write(1, b"\x1b[?2004l\x1b[?1l\x1b[?1049l")
    termios.tcsetattr(fd, termios.TCSANOW, old)
'''


def check(report: dict, name: str, passed: bool, **details: object) -> None:
    report["checks"].append({"name": name, "passed": passed, **details})
    print(f"{'PASS' if passed else 'FAIL'} {name}", flush=True)


def pane_in_snapshot(snapshot: dict, pane_id: str) -> dict:
    for pane in snapshot.get("panes", []):
        if pane.get("pane_id") == pane_id:
            return pane
    raise ProbeError(f"pane {pane_id} disappeared from session snapshot")


def wait_marker(stream: Stream, marker: str, output: Path, name: str) -> str:
    stream.until(marker)
    destination = output / name
    replay_native(stream.frames, destination)
    text = snapshot_text(destination / "snapshot.bin")
    if marker not in text:
        raise ProbeError(f"native snapshot lost marker {marker}")
    return text


def install_tui(fixture: HerdrFixture) -> tuple[Path, Path]:
    source = fixture.root / "public-input-tui.py"
    result = fixture.root / "public-input-result.json"
    source.write_text(TUI_SOURCE, encoding="utf-8")
    return source, result


def run_probe(fixture: HerdrFixture, output: Path, report: dict) -> None:
    default = fixture.cli(
        "default",
        "workspace",
        "create",
        "--cwd",
        str(fixture.root),
        "--label",
        "public-input",
        "--no-focus",
    )["result"]
    pane_id = default["root_pane"]["pane_id"]
    api = PublicInputClient(fixture)
    snapshot = fixture.cli("default", "api", "snapshot")["result"]["snapshot"]
    report["herdr_version"] = snapshot["version"]
    report["protocol"] = snapshot["protocol"]
    report["target_terminal_id"] = pane_in_snapshot(snapshot, pane_id)["terminal_id"]

    stream = control_stream(fixture, "default", pane_id)
    initial = stream.record()
    check(report, "direct_controller_initial_frame", initial.get("type") == "terminal.frame" and initial.get("full"))
    source, result_path = install_tui(fixture)
    command = shlex.join([sys.executable, str(source), str(result_path)]) + "\n"
    stream.input(command.encode())
    wait_marker(stream, "NORMAL_READY", output, "normal-ready")

    normal_expected = b"\x1b[A"
    api.send_keys(pane_id, "up")
    wait_marker(stream, "DECCKM_READY", output, "normal-up")
    normal_actual = bytes.fromhex(json.loads(result_path.read_text())["normal_hex"])
    report["inputs"]["normal_up"] = {"expected_hex": normal_expected.hex(), "remote_hex": normal_actual.hex()}
    check(report, "public_send_keys_normal_up", normal_actual == normal_expected, expected_hex=normal_expected.hex(), remote_hex=normal_actual.hex())

    decckm_expected = b"\x1bOA"
    api.send_keys(pane_id, "up")
    wait_marker(stream, "PASTE_READY", output, "decckm-up")
    decckm_actual = bytes.fromhex(json.loads(result_path.read_text())["decckm_hex"])
    report["inputs"]["decckm_up"] = {"expected_hex": decckm_expected.hex(), "remote_hex": decckm_actual.hex()}
    check(report, "public_send_keys_decckm_up", decckm_actual == decckm_expected, expected_hex=decckm_expected.hex(), remote_hex=decckm_actual.hex())

    paste_source = "first\n日本語".encode("utf-8")
    paste_expected = b"\x1b[200~" + paste_source + b"\x1b[201~"
    stream.input(paste_expected)
    wait_marker(stream, "TARGET_READY", output, "paste")
    paste_actual = bytes.fromhex(json.loads(result_path.read_text())["paste_hex"])
    report["inputs"]["cjk_multiline_paste"] = {"expected_hex": paste_expected.hex(), "remote_hex": paste_actual.hex()}
    check(report, "direct_control_cjk_multiline_paste", paste_actual == paste_expected, expected_hex=paste_expected.hex(), remote_hex=paste_actual.hex())

    other = fixture.cli("default", "workspace", "create", "--cwd", str(fixture.root), "--focus", "--label", "other-focus")["result"]
    focused = fixture.cli("default", "api", "snapshot")["result"]["snapshot"]
    target_pane = pane_in_snapshot(focused, pane_id)
    check(report, "public_target_survives_other_workspace_focus", focused["focused_workspace_id"] == other["workspace"]["workspace_id"] and focused["focused_workspace_id"] != target_pane["workspace_id"], target_workspace=target_pane.get("workspace_id"), focused_workspace=focused.get("focused_workspace_id"))
    target_expected = b"TARGET_OK"
    api.send_text(pane_id, target_expected.decode())
    wait_marker(stream, "TARGET_RESULT_", output, "explicit-target")
    target_actual = bytes.fromhex(json.loads(result_path.read_text())["target_hex"])
    report["inputs"]["explicit_target"] = {"expected_hex": target_expected.hex(), "remote_hex": target_actual.hex()}
    check(report, "public_send_text_explicit_target", target_actual == target_expected, expected_hex=target_expected.hex(), remote_hex=target_actual.hex())

    rejected = control_stream(fixture, "default", pane_id)
    rejection = expect_closed(rejected)
    check(report, "second_direct_controller_rejected_without_takeover", "already has" in rejection.get("reason", ""), reason=rejection.get("reason"))
    rejected.close()

    rival = control_stream(fixture, "default", pane_id, takeover=True)
    rival_frame = rival.record()
    if rival_frame.get("type") != "terminal.frame":
        raise ProbeError("explicit rival did not acquire controller")
    closed = expect_closed(stream)
    check(report, "explicit_rival_takeover_closes_old_controller", closed.get("reason") == "terminal attach taken over", reason=closed.get("reason"))
    before = api.requests_written
    api.disable(closed.get("reason", "direct controller was closed"))
    try:
        api.send_text(pane_id, "BLOCKED")
    except ProbeError as error:
        blocked = "disabled" in str(error)
    else:
        blocked = False
    check(report, "stale_client_disables_subsequent_public_writes", blocked and api.requests_written == before, requests_written=api.requests_written)
    stream.close()

    rival.release()
    reconnected = control_stream(fixture, "default", pane_id)
    reconnect_frame = reconnected.record()
    replay_native(reconnected.frames, output / "reconnect")
    reconnected_snapshot = fixture.cli("default", "api", "snapshot")["result"]["snapshot"]
    reconnected_terminal_id = pane_in_snapshot(reconnected_snapshot, pane_id)["terminal_id"]
    check(
        report,
        "release_reconnect_preserves_remote_tui_and_terminal_id",
        reconnect_frame.get("full") is True
        and "TUI_WAITING" in snapshot_text(output / "reconnect" / "snapshot.bin")
        and reconnected_terminal_id == report["target_terminal_id"],
        terminal_id=reconnected_terminal_id,
    )
    reconnected.release()
    last = control_stream(fixture, "default", pane_id)
    last.record()
    last.input(b"q")
    last.input(b"printf 'PUBLIC_INPUT_%s\\n' DONE\n")
    wait_marker(last, "PUBLIC_INPUT_DONE", output, "final")
    last.release()


def source_hashes() -> dict[str, str]:
    paths = [
        "scripts/herdr/public_input.py",
        "scripts/herdr/feasibility.py",
        "native/meeterm-core/src/herdr_probe_tests.rs",
        "native/meeterm-core/src/terminal.rs",
        "native/meeterm-core/src/input.rs",
        "native/meeterm-core/Cargo.lock",
    ]
    return {path: hashlib.sha256((REPO / path).read_bytes()).hexdigest() for path in paths}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr", default=shutil.which("herdr"))
    parser.add_argument("--output", required=True, type=Path, help="new evidence directory")
    args = parser.parse_args()
    if not args.herdr:
        parser.error("install Herdr separately or supply --herdr; this diagnostic never installs it")
    binary = str(Path(args.herdr).resolve(strict=True))
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {
        "checks": [],
        "inputs": {},
        "scope": "Linux OpenSSH CLI + Herdr public key/text API + direct control paste + shared Rust snapshot; sequential Python diagnostic only, no Rust input actor or mobile UI/GPU/IME evidence",
        "started_at": datetime.now(timezone.utc).isoformat(),
        "reference_source_commit": REFERENCE_SOURCE_COMMIT,
        "source_sha256": source_hashes(),
    }
    with Path(binary).open("rb") as executable:
        report["herdr_binary_sha256"] = hashlib.file_digest(executable, "sha256").hexdigest()
    try:
        with (output / "native-build.log").open("wb") as log:
            built = subprocess.run(
                ["cargo", "test", "--locked", "--lib", "herdr_probe_tests::replay_live_frames", "--no-run"],
                cwd=REPO / "native" / "meeterm-core", stdout=log, stderr=subprocess.STDOUT, timeout=300,
            )
        if built.returncode:
            raise ProbeError("native replay helper build failed; see native-build.log")
        with tempfile.TemporaryDirectory(prefix=".mh17-public-", dir=Path.home()) as temporary:
            fixture = HerdrFixture(Path(temporary), binary)
            try:
                fixture.start()
                run_probe(fixture, output, report)
            finally:
                for index, stream in enumerate(fixture.streams):
                    (output / f"stream-{index}.json").write_text(json.dumps(stream.frames) + "\n")
                fixture.close()
    except Exception as error:
        report["error"] = type(error).__name__ + ": " + str(error)
        print(report["error"], file=sys.stderr)
    report["passed"] = bool(report["checks"]) and not report.get("error") and all(check["passed"] for check in report["checks"])
    (output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
