#!/usr/bin/env python3
"""Capture optional Android presentation evidence for the public smoke routes.

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
    "home", "servers", "connection", "password", "workspaces", "terminal",
    "settings", "workspace-name", "terminal-name", "handoff",
    "runtime-picker", "runtime-partial-error", "runtime-empty", "runtime-create",
    "herdr-connection",
    "herdr-groups",
    "herdr-terminal",
    "herdr-workspaces",
    "recovery-progress",
    "recovery-exhausted",
    "recovery-mismatch",
    "herdr-recovery-confirm",
    "layout-restore-unconfirmed",
    "runtime-layout-restore-unconfirmed",
    "welcome", "empty", "search-empty", "disconnected", "reconnecting",
    "connection-error", "long-workspaces",
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


def has_positive_ui_bounds(value: str) -> bool:
    match = re.fullmatch(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", value)
    if match is None:
        return False
    left, top, right, bottom = (int(part) for part in match.groups())
    return right > left and bottom > top


def ui_values(root: ET.Element) -> set[str]:
    values: set[str] = set()
    for node in root.iter():
        for key in ("text", "content-desc", "resource-id"):
            value = node.attrib.get(key, "").strip()
            if value:
                values.add(value)
                values.add(normalized(value))
        resource_id = node.attrib.get("resource-id", "").strip()
        if (
            resource_id
            and node.attrib.get("visible-to-user", "true") == "true"
            and has_positive_ui_bounds(node.attrib.get("bounds", ""))
        ):
            state = "enabled" if node.attrib.get("enabled", "true") == "true" else "disabled"
            values.add(f"{resource_id}::{state}")
            values.add(f"{normalized(resource_id)}::{state}")
    return values


def has_visible_test_id(values: set[str], test_id: str, *, state: str = "enabled") -> bool:
    """Match a visible Android resource ID and its UIAutomator state.

    React Native exposes ``testID`` as a package-qualified resource ID on
    Android.  The package prefix is an implementation detail, while the
    ``::enabled`` suffix is added by :func:`ui_values` from the actual
    UIAutomator node.  Keeping both in the readiness contract prevents a
    hidden or disabled recovery action from satisfying a screenshot route.
    """

    expected = f"{test_id}::{state}"
    qualified = f":id/{expected}"
    return any(value == expected or value.endswith(qualified) for value in values)


def recovery_screen_checks(values: set[str]) -> list[tuple[str, bool]]:
    normalized_values = {normalized(value) for value in values}
    return [
        ("recovery_rail", has_visible_test_id(values, "recovery-rail")),
        ("recovery_title", has_visible_test_id(values, "recovery-title")),
        ("recovery_detail", has_visible_test_id(values, "recovery-detail")),
        ("recovery_meta", has_visible_test_id(values, "recovery-meta")),
        # Recovery screens deliberately retain the native terminal in its
        # cached, read-only mode.  A generic "Terminal" node is not enough:
        # it could be a stale toolbar label or a live surface.
        ("native_cached_terminal", "Terminal, cached output, read only" in normalized_values),
    ]


def screen_checks(screen: str, values: set[str]) -> list[str]:
    checks: list[tuple[str, bool]]
    normalized_values = {normalized(value) for value in values}
    if screen in {
        "recovery-progress",
        "recovery-exhausted",
        "recovery-mismatch",
        "herdr-recovery-confirm",
    }:
        checks = recovery_screen_checks(values)
        recovery_copy = {
            "recovery-progress": (
                "Verifying this workspace…",
                "Checking the server, runtime, and terminal.",
                "Last received output · Input paused",
            ),
            "recovery-exhausted": (
                "Still offline",
                "Couldn’t reach Smoke server.",
                "Last received output · Input paused",
            ),
            "recovery-mismatch": (
                "This runtime can’t be restored",
                'The runtime named “meeterm” is not the same instance as before.',
                "Last received output · Input paused",
            ),
            "herdr-recovery-confirm": (
                "Confirmation needed",
                'Herdr can’t verify that “dev” is the same instance.',
                "Last received output · Input paused",
            ),
        }[screen]
        checks.extend(
            (f"recovery_copy_{index}", copy in normalized_values)
            for index, copy in enumerate(recovery_copy)
        )
        actions = {
            "recovery-progress": (),
            "recovery-exhausted": ("recovery-retry", "recovery-change"),
            "recovery-mismatch": ("recovery-retry", "recovery-change"),
            "herdr-recovery-confirm": ("recovery-review", "recovery-change"),
        }[screen]
        checks.extend(
            (f"recovery_action_{action}_enabled", has_visible_test_id(values, action))
            for action in actions
        )
    elif screen == "herdr-connection":
        checks = [
            ("runtime_picker_heading", "Choose a runtime for Smoke server" in normalized_values),
            ("herdr_default", "Herdr runtime default" in normalized_values),
            ("last_used_hint", "Last used" in normalized_values),
        ]
    elif screen == "runtime-picker":
        checks = [
            ("runtime_picker_heading", "Choose a runtime for Smoke server" in normalized_values),
            ("tmux_meeterm", "tmux runtime meeterm" in normalized_values),
            ("herdr_default", "Herdr runtime default" in normalized_values),
            ("herdr_paused", "Herdr runtime paused" in normalized_values),
        ]
    elif screen == "runtime-partial-error":
        checks = [
            ("tmux_meeterm", "tmux runtime meeterm" in normalized_values),
            (
                "herdr_error",
                "Herdr is not available over SSH. Open Herdr on your computer or check its installation."
                in normalized_values,
            ),
        ]
    elif screen == "runtime-empty":
        checks = [
            ("tmux_empty", "No running tmux sessions found." in normalized_values),
            ("herdr_paused", "Herdr runtime paused" in normalized_values),
            ("stopped", "Stopped" in normalized_values),
        ]
    elif screen == "runtime-create":
        checks = [
            ("create_heading", "Create tmux session" in normalized_values),
            ("session_name", "tmux session name" in normalized_values),
            (
                "create_submit",
                any("runtime-tmux-create-submit" in value for value in normalized_values),
            ),
        ]
    elif screen == "layout-restore-unconfirmed":
        checks = [
            ("disconnected_state", "Disconnected" in normalized_values),
            (
                "layout_restore_warning",
                "The old connection's desktop layout restore could not be confirmed."
                in normalized_values,
            ),
            ("warning_dismiss", "Dismiss desktop layout warning" in normalized_values),
        ]
    elif screen == "runtime-layout-restore-unconfirmed":
        checks = [
            ("runtime_picker_heading", "Choose a runtime for Smoke server" in normalized_values),
            (
                "layout_restore_warning",
                "The old connection's desktop layout restore could not be confirmed."
                in normalized_values,
            ),
            ("warning_dismiss", "Dismiss desktop layout warning" in normalized_values),
        ]
    elif screen == "herdr-groups":
        checks = [
            ("group_sheet_title", "Switch group" in values),
            ("group_development", any(value.startswith("Group Development") for value in normalized_values)),
            ("group_tests_review", any(value.startswith("Group Tests & review") for value in normalized_values)),
            ("group_working_status", any("Agent status: working" in value for value in normalized_values)),
            ("group_done_status", any("Agent status: finished" in value for value in normalized_values)),
        ]
    elif screen == "herdr-terminal":
        # Representative-view boundary: the one-shot screenshot is taken at
        # the initial horizontal position. Require the selected pane's owner,
        # its visible status, and the native group/terminal surface here. The
        # other four statuses stay seeded and are covered by App/component
        # tests; offscreen tabs are not a screenshot-readiness prerequisite.
        # This one screenshot does not visually prove all five statuses.
        checks = [
            ("terminal_group_switch", any(value.startswith("Switch terminal group") for value in normalized_values)),
            ("native_terminal", "Terminal" in values),
            ("selected_agent_owner", "Claude Code, Agent status: working" in normalized_values),
            ("selected_agent_status", "Working" in normalized_values),
        ]
    elif screen in ("workspaces", "herdr-workspaces"):
        checks = [
            (
                "workspace_total_two",
                any(re.fullmatch(r"All\s+2", value) for value in normalized_values),
            ),
            ("main_workspace_row", any(value.startswith("Workspace Main workspace") for value in normalized_values)),
            ("tools_workspace_row", any(value.startswith("Workspace Tools workspace") for value in normalized_values)),
        ]
        if screen == "herdr-workspaces":
            checks.extend([
                ("main_workspace_status", any("Agent status: blocked" in value for value in normalized_values)),
                ("tools_workspace_status", any("Agent status: idle" in value for value in normalized_values)),
            ])
    elif screen == "long-workspaces":
        checks = [
            (
                "main_long_workspace_row",
                any(value.startswith("Workspace Production infrastructure — migration and release preparation") for value in normalized_values),
            ),
            (
                "tools_long_workspace_row",
                any(value.startswith("Workspace Research / terminal typography and international text") for value in normalized_values),
            ),
        ]
    else:
        required = {
            "home": ("Workspaces", "Connect saved server Smoke server"),
            "servers": ("Saved servers", "server-profile-smoke-profile"),
            "connection": ("Connect to server", "Host"),
            "password": ("Connect to server", "Password authentication"),
            "terminal": ("Connected", "Terminal"),
            "settings": ("Settings", "settings-submit"),
            "workspace-name": ("Rename workspace", "Workspace or terminal name"),
            "terminal-name": ("Rename terminal", "Workspace or terminal name"),
            "handoff": ("Continue on your computer", "Disconnect"),
            "welcome": ("Your workspace. Anywhere.", "Connect"),
            "empty": ("A fresh workspace starts here.", "Create workspace"),
            "search-empty": ("No matching workspaces", "Clear workspace search"),
            "disconnected": ("Disconnected", "Reconnect"),
            "reconnecting": ("Reconnecting…", "Cancel connection"),
            "connection-error": ("Connection failed", "Reconnect"),
        }
        if screen == "connection-error":
            checks = [
                ("connection_error_heading", "Connection failed" in normalized_values),
                ("connection_error_reconnect", "Reconnect" in normalized_values),
                (
                    "authentication_guidance",
                    "Authentication failed. Check your username and the password or private key for your chosen sign-in method."
                    in normalized_values,
                ),
                (
                    "layout_restore_warning",
                    "The old connection's desktop layout restore could not be confirmed."
                    in normalized_values,
                ),
                ("warning_dismiss", "Dismiss desktop layout warning" in normalized_values),
            ]
        else:
            checks = [(f"screen_element_{index}", value in normalized_values)
                      for index, value in enumerate(required[screen])]
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
        "suite=android-screen-fixtures",
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
    print("Android screen evidence collection completed (optional).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
