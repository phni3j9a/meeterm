#!/usr/bin/env python3
"""Capture optional Android presentation evidence for the Herdr smoke routes.

This is deliberately observational.  It never taps an action that changes
remote state, never makes a screenshot an acceptance gate, and reports a
bounded UI or capture failure as an artifact for later review.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import re
from pathlib import Path
import subprocess
import sys
import time
import xml.etree.ElementTree as ET


PACKAGE_NAME = "dev.meeterm.app"
ACTIVITY_NAME = ".MainActivity"
UI_DUMP_PATH = "/sdcard/meeterm-screen-fixture.xml"
SCREEN_NAMES = (
    "herdr-connection",
    "herdr-groups",
    "herdr-terminal",
    "herdr-workspaces",
)


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    unavailable: bool = False


def run_adb(*arguments: str, timeout: float = 10.0) -> CommandResult:
    try:
        completed = subprocess.run(
            ["adb", *arguments],
            check=False,
            capture_output=True,
            timeout=timeout,
        )
    except FileNotFoundError:
        return CommandResult(127, b"", b"", unavailable=True)
    except subprocess.TimeoutExpired:
        return CommandResult(124, b"", b"", unavailable=False)
    return CommandResult(
        completed.returncode,
        completed.stdout,
        completed.stderr,
    )


def decode(result: CommandResult) -> str:
    return result.stdout.decode("utf-8", errors="replace").replace("\r", "")


def package_pid() -> str | None:
    result = run_adb("shell", "pidof", "-s", PACKAGE_NAME)
    if result.returncode != 0:
        return None
    match = re.search(r"\b([0-9]+)\b", decode(result))
    return match.group(1) if match else None


def wait_until(predicate, deadline: float, interval: float = 0.5):
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    return None


def stop_app(deadline: float) -> tuple[bool, str]:
    result = run_adb("shell", "am", "force-stop", PACKAGE_NAME)
    if result.unavailable:
        return False, "adb_unavailable"
    if result.returncode != 0:
        return False, "force_stop_failed"
    if wait_until(lambda: package_pid() is None, deadline) is None:
        return False, "old_process_did_not_stop"
    return True, "stopped"


def start_app(screen: str) -> tuple[bool, str]:
    route = f"meeterm://smoke?screen={screen}"
    result = run_adb(
        "shell",
        "am",
        "start",
        "-W",
        "-a",
        "android.intent.action.VIEW",
        "-d",
        route,
        "-n",
        f"{PACKAGE_NAME}/{ACTIVITY_NAME}",
        timeout=20.0,
    )
    if result.unavailable:
        return False, "adb_unavailable"
    if result.returncode != 0:
        return False, "route_launch_failed"
    return True, "launched"


def parse_ui_dump() -> ET.Element | None:
    dump = run_adb(
        "shell",
        "uiautomator",
        "dump",
        "--compressed",
        UI_DUMP_PATH,
        timeout=10.0,
    )
    if dump.unavailable:
        return None
    if dump.returncode != 0:
        return None
    content = run_adb("exec-out", "cat", UI_DUMP_PATH, timeout=10.0)
    if content.unavailable or content.returncode != 0:
        return None
    text = content.stdout.decode("utf-8", errors="replace")
    start = text.find("<hierarchy")
    end = text.rfind("</hierarchy>")
    if start < 0 or end < start:
        return None
    try:
        return ET.fromstring(text[start : end + len("</hierarchy>")])
    except ET.ParseError:
        return None


def normalized(value: str) -> str:
    return " ".join(value.split())


def ui_values(root: ET.Element) -> set[str]:
    values: set[str] = set()
    for node in root.iter():
        for key in ("text", "content-desc", "resource-id"):
            value = node.attrib.get(key, "").strip()
            if value:
                values.add(value)
                values.add(normalized(value))
    return values


def screen_checks(screen: str, values: set[str]) -> list[str]:
    checks: list[tuple[str, bool]]
    normalized_values = {normalized(value) for value in values}
    if screen == "herdr-connection":
        checks = [
            ("herdr_backend_radio", "herdr backend" in normalized_values),
            ("herdr_session_name_dev", "dev" in values or "dev" in normalized_values),
        ]
    elif screen == "herdr-groups":
        checks = [
            ("group_sheet_title", "Groupを切り替える" in values),
            ("group_development", "Group Development" in values),
            ("group_tests_review", "Group Tests & review" in values),
        ]
    elif screen == "herdr-terminal":
        checks = [
            ("terminal_group_switch", "Switch terminal group" in values),
            ("native_terminal", "Terminal" in values),
            ("agent_claude_code", "Claude Code" in values),
            ("agent_working", "作業中" in values),
        ]
    else:
        checks = [
            (
                "workspace_total_two",
                any(re.fullmatch(r"すべて\s+2", value) for value in normalized_values),
            ),
            ("main_terminal_count_four", "4 ターミナル" in normalized_values),
            ("tools_terminal_count_one", "1 ターミナル" in normalized_values),
        ]
    return [name for name, passed in checks if not passed]


def wait_for_screen(screen: str, deadline: float) -> tuple[bool, str]:
    if wait_until(package_pid, deadline) is None:
        return False, "process_not_running"

    state: dict[str, str] = {"reason": "ui_readiness_timeout"}

    def ready() -> bool:
        if package_pid() is None:
            state["reason"] = "process_exited"
            return False
        root = parse_ui_dump()
        if root is None:
            state["reason"] = "ui_dump_unavailable"
            return False
        missing = screen_checks(screen, ui_values(root))
        if missing:
            state["reason"] = "missing_" + ",".join(missing)
            return False
        state["reason"] = "ready"
        return True

    if wait_until(ready, deadline) is None:
        return False, state["reason"]
    return True, "ready"


def capture_screenshot(path: Path) -> tuple[bool, str]:
    try:
        result = subprocess.run(
            ["adb", "exec-out", "screencap", "-p"],
            check=False,
            capture_output=True,
            timeout=15.0,
        )
    except FileNotFoundError:
        return False, "adb_unavailable"
    except subprocess.TimeoutExpired:
        return False, "screenshot_timeout"
    if result.returncode != 0:
        return False, "screenshot_command_failed"
    if not result.stdout.startswith(b"\x89PNG\r\n\x1a\n"):
        return False, "screenshot_not_png"
    try:
        path.write_bytes(result.stdout)
    except OSError:
        try:
            path.unlink(missing_ok=True)
        except OSError:
            pass
        return False, "screenshot_write_failed"
    return True, "captured"


def write_unavailable(path: Path, screen: str, reason: str) -> None:
    path.write_text(
        "\n".join(
            (
                f"screen={screen}",
                "result=unavailable",
                f"reason={reason}",
            )
        ),
        encoding="utf-8",
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--artifact-dir",
        type=Path,
        default=Path("artifacts/android-emulator-observability"),
    )
    parser.add_argument("--timeout-seconds", type=float, default=45.0)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    timeout_seconds = max(5.0, min(args.timeout_seconds, 120.0))
    artifact_dir: Path = args.artifact_dir
    artifact_dir.mkdir(parents=True, exist_ok=True)
    report_lines = [
        "suite=android-herdr-screen-fixtures",
        "evidence=observational; screenshot_presence_is_not_a_gate",
    ]

    for screen in SCREEN_NAMES:
        screenshot = artifact_dir / f"{screen}.png"
        diagnostic = artifact_dir / f"{screen}-unavailable.txt"
        screenshot.unlink(missing_ok=True)
        diagnostic.unlink(missing_ok=True)
        started = time.monotonic()

        stopped, stop_reason = stop_app(started + timeout_seconds)
        if not stopped:
            write_unavailable(diagnostic, screen, stop_reason)
            report_lines.append(f"{screen}=unavailable:{stop_reason}")
            continue

        launched, launch_reason = start_app(screen)
        if not launched:
            write_unavailable(diagnostic, screen, launch_reason)
            report_lines.append(f"{screen}=unavailable:{launch_reason}")
            continue

        ready, ready_reason = wait_for_screen(screen, started + timeout_seconds)
        if not ready:
            write_unavailable(diagnostic, screen, ready_reason)
            report_lines.append(f"{screen}=unavailable:{ready_reason}")
            continue

        captured, capture_reason = capture_screenshot(screenshot)
        if not captured:
            write_unavailable(diagnostic, screen, capture_reason)
            report_lines.append(f"{screen}=unavailable:{capture_reason}")
            continue
        report_lines.append(f"{screen}=captured")

    (artifact_dir / "android-screen-fixtures.txt").write_text(
        "\n".join(report_lines) + "\n", encoding="utf-8"
    )
    print("Android Herdr screen evidence collection completed (optional).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
