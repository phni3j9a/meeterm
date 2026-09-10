#!/usr/bin/env python3
"""Drive the Android SSH form against the disposable OpenSSH fixture.

This is a bounded, UI-only smoke.  It uses the accessibility tree to find
controls and sends the terminal's ASCII commands through Android input.  The
terminal data path stays in the native view; the script only captures a final
PNG for human review and checks marker files on the fixture host.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import secrets
import shlex
import stat
import shutil
import subprocess
import sys
import time
from typing import NamedTuple
import xml.etree.ElementTree as ElementTree


PACKAGE = "dev.meeterm.app"
TMUX = "tmux"
KEYCODE_DEL = 67
KEYCODE_ENTER = 66
KEYCODE_BACK = 4
KEYCODE_MOVE_END = 123
KEYCODE_F10 = 140
DEFAULT_UI_TIMEOUT = 30.0
REMOTE_MARKER_TIMEOUT = 15.0
RECONNECT_TIMEOUT = 45.0
FIELD_READBACK_TIMEOUT = 8.0
FIELD_SETTLE_SECONDS = 0.25
FIELD_INPUT_MAX_ATTEMPTS = 2
KEY_INPUT_TIMEOUT = 600.0
KEY_READBACK_TIMEOUT = 15.0
KEY_INPUT_SETTLE_SECONDS = 0.3
KEY_INPUT_MAX_ATTEMPTS = 4
# ``adb shell input text`` expands one argument into a burst of individual
# KeyEvents. A remote terminal forwards each committed character through a
# bounded asynchronous tmux queue, so keep the CI burst below that queue's
# service rate while still exercising the native keyboard key-event path.
# Paste has its own explicit native control. This is a driver pacing bound,
# not a delay in the product input path; the queue-overflow explanation
# remains a hypothesis until the sanitized native counters confirm it.
TERMINAL_INPUT_CHUNK_SIZE = 16
TERMINAL_INPUT_CHUNK_DELAY_SECONDS = 0.2
TERMINAL_FOCUS_SETTLE_SECONDS = 0.8
INPUT_REJECTION_REASONS = (
    "unbound",
    "native_exception",
    "native_rejection",
)
INPUT_REJECTION_PATTERN = re.compile(
    r"IME commit rejected; reason=(unbound|native_exception|native_rejection)\b"
)
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
MP4_FILE_TYPE_BOX = b"ftyp"
SCREENRECORD_REMOTE_PATH = "/sdcard/meeterm-daily-use.mp4"
SCREENRECORD_REMOTE_PID_PATH = "/sdcard/meeterm-daily-use.pid"
SCREENRECORD_LIMIT_SECONDS = 180

SYNC_MARKER = "MEETERM_ANDROID_SYNC_4C71"
ANSI_MARKER = "MEETERM_ANDROID_ANSI_8A26"
REMOTE_MARKER_PREFIX = "meeterm-android-shell-"
DAILY_PROFILE_NAME = "Android daily fixture"
DAILY_WORKSPACE_NAME = "daily-ci"
DAILY_WORKSPACE_RENAMED = "daily-ci-renamed"
DAILY_PANE_NAME = "daily-pane"
DAILY_SELECTION_MARKER = "COPY29F7"
DAILY_GLYPH_STRESS_COUNT = 1024
DAILY_GLYPH_STRESS_COLUMNS = 16
GLYPH_ATLAS_RESET_PATTERN = re.compile(
    r"\bMEETERM_GLYPH_ATLAS_RESET count=[1-9][0-9]*\b"
)
PANE_LABEL_PATTERN = re.compile(r"^Terminal (%[0-9]+)$")
WORKSPACE_LABEL_PATTERN = re.compile(r"^Workspace .+$")
BACK_TO_WORKSPACES_LABELS = (
    "Back to workspaces",
)
SWITCH_WORKSPACE_LABELS = (
    "Switch workspace",
)
TERMINAL_MENU_LABELS = (
    "Terminal menu",
)
SERVER_CONNECTION_LABELS = (
    "Server connection",
)
HANDOFF_LABELS = (
    "PC handoff help",
)
DISCONNECT_LABELS = (
    "Disconnect",
)
RECONNECT_LABELS = (
    "Reconnect",
)
HANDOFF_COMMAND = "tmux attach -t meeterm"
PRIVATE_KEY_ACCESSIBILITY_LABELS = (
    "Private OpenSSH key",
    "Private OpenSSH key, Empty",
    "Private OpenSSH key, Private key entered",
)


class SmokeFailure(RuntimeError):
    """An expected, sanitized smoke failure."""

    def __init__(self, stage: str, reason: str = "failed") -> None:
        super().__init__()
        self.stage = stage
        self.reason = reason


class Node:
    """The small subset of one accessibility node needed for tapping."""

    __slots__ = (
        "text",
        "content_description",
        "resource_id",
        "class_name",
        "bounds",
        "scrollable",
        "enabled",
        "visible_to_user",
        "selected",
        "checked",
        "focused",
    )

    def __init__(
        self,
        text: str,
        content_description: str,
        class_name: str,
        bounds: tuple[int, int, int, int],
        *,
        resource_id: str = "",
        scrollable: bool = False,
        enabled: bool = True,
        visible_to_user: bool = True,
        selected: bool = False,
        checked: bool = False,
        focused: bool = False,
    ) -> None:
        self.text = text
        self.content_description = content_description
        self.resource_id = resource_id
        self.class_name = class_name
        self.bounds = bounds
        self.scrollable = scrollable
        self.enabled = enabled
        self.visible_to_user = visible_to_user
        self.selected = selected
        self.checked = checked
        self.focused = focused

    @property
    def center(self) -> tuple[int, int]:
        left, top, right, bottom = self.bounds
        return ((left + right) // 2, (top + bottom) // 2)


class TmuxPaneRecord(NamedTuple):
    """The fixture-side identity, selection, and split geometry of a pane."""

    window_id: str
    window_name: str
    pane_id: str
    pane_pid: int
    pane_index: int
    active: bool
    pane_width: int
    pane_height: int
    pane_left: int
    pane_top: int
    pane_right: int
    pane_bottom: int
    window_active: bool
    zoomed: bool


class ScreenRecording(NamedTuple):
    """One optional, credential-free adb screenrecord process."""

    remote_pid: int
    remote_path: str
    output_path: Path


FIXTURE_WINDOW_NAMES = ("smoke", "handoff")
FIXTURE_PANES_PER_WINDOW = 2
FIXTURE_PANE_COUNT = len(FIXTURE_WINDOW_NAMES) * FIXTURE_PANES_PER_WINDOW


def parse_bounds(value: str) -> tuple[int, int, int, int] | None:
    match = re.fullmatch(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]", value)
    if match is None:
        return None
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def parse_ui_dump(output: bytes) -> list[Node]:
    """Parse a /dev/tty dump without writing or printing the XML."""

    xml_start = output.find(b"<?xml")
    if xml_start < 0:
        xml_start = output.find(b"<hierarchy")
    if xml_start < 0:
        if b"could not get idle state" in output:
            raise SmokeFailure("uiautomator", "accessibility_not_idle")
        if b"null root node" in output:
            raise SmokeFailure("uiautomator", "root_unavailable")
        raise SmokeFailure("uiautomator", "xml_unavailable")
    xml_end = output.find(b"</hierarchy>", xml_start)
    if xml_end < 0:
        raise SmokeFailure("uiautomator", "xml_incomplete")
    xml_end += len(b"</hierarchy>")
    try:
        root = ElementTree.fromstring(output[xml_start:xml_end])
    except ElementTree.ParseError as error:
        raise SmokeFailure("uiautomator", "xml_invalid") from error

    nodes: list[Node] = []
    for element in root.iter():
        bounds = parse_bounds(element.attrib.get("bounds", ""))
        if bounds is None:
            continue
        nodes.append(
            Node(
                text=element.attrib.get("text", ""),
                content_description=element.attrib.get("content-desc", ""),
                resource_id=element.attrib.get("resource-id", ""),
                class_name=element.attrib.get("class", ""),
                bounds=bounds,
                scrollable=element.attrib.get("scrollable", "false") == "true",
                enabled=element.attrib.get("enabled", "true") == "true",
                visible_to_user=element.attrib.get("visible-to-user", "true")
                == "true",
                selected=element.attrib.get("selected", "false") == "true",
                checked=element.attrib.get("checked", "false") == "true",
                focused=element.attrib.get("focused", "false") == "true",
            )
        )
    return nodes


class AndroidDevice:
    def __init__(self, serial: str, adb_path: str) -> None:
        self.serial = serial
        self.adb_path = adb_path
        # Counts only generated terminal command characters. The values are
        # used for a sanitized input health summary; command contents never
        # enter the artifact.
        self.terminal_input_chars = 0
        self.terminal_input_chunks = 0
        self.foreground_evidence_lost = False

    def note_terminal_input(self, character_count: int) -> None:
        self.terminal_input_chars += max(0, character_count)
        self.terminal_input_chunks += 1

    def run(self, arguments: tuple[str, ...], stage: str, timeout: float = 15.0) -> bytes:
        command = [self.adb_path, "-s", self.serial, *arguments]
        try:
            result = subprocess.run(
                command,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=timeout,
                check=False,
            )
        except (FileNotFoundError, OSError, subprocess.TimeoutExpired) as error:
            raise SmokeFailure(stage, "adb_unavailable") from error
        if result.returncode != 0:
            raise SmokeFailure(stage, "adb_failed")
        return result.stdout

    def wait_for_device(self) -> None:
        self.run(("wait-for-device",), "device_ready", timeout=30.0)

    def assert_process_alive(self, stage: str) -> None:
        self.process_id(stage)

    def process_id(self, stage: str) -> str:
        output = self.run(
            ("shell", "pidof", "-s", PACKAGE),
            stage,
            timeout=10.0,
        ).decode("utf-8", errors="replace")
        match = re.fullmatch(r"\s*(\d+)\s*", output)
        if match is None:
            raise SmokeFailure(stage, "app_not_running")
        return match.group(1)

    def dump_ui(self) -> list[Node]:
        # The accessibility service can temporarily return no hierarchy while
        # the keyboard/layout is changing. Retry acquisition only; never
        # replay input or persist the potentially credential-bearing output.
        for attempt in range(3):
            try:
                output = self.run(
                    ("shell", "-tt", "uiautomator", "dump", "/dev/tty"),
                    "uiautomator",
                    # UIAutomator itself waits up to ten seconds for idle;
                    # allow its fixed diagnostic to return before adb times out.
                    timeout=15.0,
                )
                return parse_ui_dump(output)
            except SmokeFailure:
                if attempt == 2:
                    raise
                time.sleep(0.5)
        raise AssertionError("unreachable")

    def assert_foreground(self, stage: str) -> None:
        try:
            output = self.run(
                ("shell", "dumpsys", "window"),
                f"{stage}_foreground",
                timeout=10.0,
            ).decode("utf-8", errors="replace")
        except SmokeFailure:
            self.foreground_evidence_lost = True
            raise
        if f"Application Not Responding: {PACKAGE}" in output:
            self.foreground_evidence_lost = True
            raise SmokeFailure(stage, "app_anr_window")
        current_focus_lines = [
            line for line in output.splitlines() if "mCurrentFocus" in line
        ]
        if current_focus_lines:
            if any(f"{PACKAGE}/" in line for line in current_focus_lines):
                return
            self.foreground_evidence_lost = True
            raise SmokeFailure(stage, "app_not_foreground")

        # Some Android versions omit mCurrentFocus while a window is settling;
        # mFocusedApp is a safe fallback only in that absence, never when it
        # conflicts with a present current-focus window.
        if any(
            "mFocusedApp" in line and f"{PACKAGE}/" in line
            for line in output.splitlines()
        ):
            return
        self.foreground_evidence_lost = True
        raise SmokeFailure(stage, "app_not_foreground")

    def input_text(self, value: str, stage: str) -> None:
        if not value or "\n" in value or "\r" in value or "%" in value:
            raise SmokeFailure(stage, "invalid_input_text")
        self.assert_foreground(stage)
        # Android's input tool uses %s for a space.  Quote the complete
        # argument for adb's remote shell so shell punctuation is typed as
        # text rather than interpreted by that shell.
        encoded = value.replace(" ", "%s")
        self.run(
            ("shell", "input", "text", shlex.quote(encoded)),
            stage,
            timeout=20.0,
        )
        if stage == "terminal_input":
            self.note_terminal_input(len(value))

    def input_keyevent(self, keycode: int, stage: str) -> None:
        self.input_keyevents((keycode,), stage)

    def dismiss_keyboard(self, stage: str) -> None:
        """Dismiss the Android IME while keeping the current activity open.

        The caller must only use this after tapping an editor.  Android
        consumes the first BACK event in that state to hide the IME; a second
        event would navigate away from the modal, so this is deliberately a
        single event rather than a generic retry loop.
        """

        self.input_keyevent(KEYCODE_BACK, stage)

    def input_keyevents(self, keycodes: tuple[int, ...], stage: str) -> None:
        if not keycodes:
            return
        self.assert_foreground(stage)
        self.run(
            ("shell", "input", "keyevent", *(str(keycode) for keycode in keycodes)),
            stage,
            timeout=10.0,
        )

    def input_tap(self, x: int, y: int, stage: str) -> None:
        self.assert_foreground(stage)
        self.run(("shell", "input", "tap", str(x), str(y)), stage, timeout=10.0)

    def input_swipe(
        self,
        bounds: tuple[int, int, int, int],
        stage: str,
        *,
        x: int | None = None,
        toward_start: bool = False,
    ) -> None:
        self.assert_foreground(stage)
        left, top, right, bottom = bounds
        swipe_x = (left + right) // 2 if x is None else x
        if not left < swipe_x < right:
            raise SmokeFailure(stage, "invalid_scroll_gutter")
        start_y = top + (bottom - top) * 4 // 5
        end_y = top + (bottom - top) // 5
        if toward_start:
            start_y, end_y = end_y, start_y
        self.run(
            (
                "shell",
                "input",
                "swipe",
                str(swipe_x),
                str(start_y),
                str(swipe_x),
                str(end_y),
                "350",
            ),
            stage,
            timeout=10.0,
        )

    def input_long_press_drag(
        self,
        start_x: int,
        start_y: int,
        end_x: int,
        end_y: int,
        stage: str,
        *,
        duration_ms: int = 1200,
    ) -> None:
        """Long-press one terminal cell, then extend its native selection."""

        if duration_ms <= 0:
            raise SmokeFailure(stage, "invalid_long_press")
        self.assert_foreground(stage)
        self.run(
            (
                "shell",
                "input",
                "swipe",
                str(start_x),
                str(start_y),
                str(end_x),
                str(end_y),
                str(duration_ms),
            ),
            stage,
            timeout=max(10.0, duration_ms / 1000.0 + 5.0),
        )

    def screenshot(self, output_path: Path) -> None:
        self.assert_foreground("screenshot")
        image = self.run(("exec-out", "screencap", "-p"), "screenshot", timeout=20.0)
        # Keep pixels in memory until the post-capture focus check succeeds.
        # An observed app switch must never leave a third-party screen artifact.
        self.assert_foreground("screenshot")
        if not image.startswith(PNG_SIGNATURE):
            raise SmokeFailure("screenshot", "png_unavailable")
        try:
            output_path.parent.mkdir(parents=True, exist_ok=True)
            output_path.write_bytes(image)
        except OSError as error:
            raise SmokeFailure("screenshot", "artifact_write_failed") from error

    def logcat(self) -> str:
        output = self.run(
            ("shell", "logcat", "-d", "-v", "threadtime"),
            "logcat",
            timeout=20.0,
        ).decode("utf-8", errors="replace")
        kept: list[str] = []
        accepted_commits = 0
        accepted_bytes = 0
        last_native_count: int | None = None
        rejected_commits = dict.fromkeys(INPUT_REJECTION_REASONS, 0)
        for line in output.splitlines():
            if not any(
                tag in line
                for tag in (
                    "MeetermTerminalView",
                    "MeetermRenderer",
                    "MeetermNative",
                    # This tag reports only accepted counts and byte lengths;
                    # it never includes committed text or clipboard content.
                    "MeetermInput",
                )
            ):
                continue
            lowered = line.lower()
            if any(secret_word in lowered for secret_word in ("passphrase", "private key", "auth")):
                continue
            if "MeetermInput" in line:
                accepted = re.search(
                    r"IME commit accepted; nativeCount=(\d+) byteCount=(\d+)",
                    line,
                )
                if accepted is not None:
                    accepted_commits += 1
                    accepted_bytes += int(accepted.group(2))
                    last_native_count = int(accepted.group(1))
                rejected = INPUT_REJECTION_PATTERN.search(line)
                if rejected is not None:
                    rejected_commits[rejected.group(1)] += 1
                # One aggregate line below is enough for CI diagnosis. Keep no
                # per-character native input records in the artifact.
                continue
            kept.append(line)
        if self.terminal_input_chunks or accepted_commits or any(rejected_commits.values()):
            # A lower observed byte count is a diagnostic only: logcat can be
            # truncated or sampled while callbacks are still in flight, so it
            # must not be reported as proof of native rejection.
            unobserved_bytes = max(0, self.terminal_input_chars - accepted_bytes)
            kept.append(
                "MeetermInput: terminal_input_summary "
                f"chunks={self.terminal_input_chunks} "
                f"attemptedBytes={self.terminal_input_chars} "
                f"acceptedCommits={accepted_commits} "
                f"acceptedBytes={accepted_bytes} "
                f"unobservedBytes={unobserved_bytes} "
                f"lastNativeCount={last_native_count if last_native_count is not None else 'none'} "
                f"rejectedCommits={sum(rejected_commits.values())} "
                f"rejectedUnbound={rejected_commits['unbound']} "
                f"rejectedNativeException={rejected_commits['native_exception']} "
                f"rejectedNativeRejection={rejected_commits['native_rejection']}"
            )
        return "\n".join(kept) + ("\n" if kept else "<no filtered native log lines>\n")

    def force_stop(self) -> None:
        try:
            self.run(("shell", "am", "force-stop", PACKAGE), "cleanup", timeout=10.0)
        except SmokeFailure:
            pass


def resolve_serial(adb_path: str, requested: str | None) -> str:
    serial = requested or os.environ.get("ANDROID_SERIAL")
    if serial:
        return serial
    try:
        result = subprocess.run(
            [adb_path, "devices"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=10.0,
            check=False,
        )
    except (FileNotFoundError, OSError, subprocess.TimeoutExpired) as error:
        raise SmokeFailure("device_select", "adb_unavailable") from error
    if result.returncode != 0:
        raise SmokeFailure("device_select", "adb_failed")
    devices = [
        line.split("\t", 1)[0]
        for line in result.stdout.decode("utf-8", errors="replace").splitlines()
        if "\tdevice" in line
    ]
    if len(devices) != 1:
        raise SmokeFailure("device_select", "set_android_serial")
    return devices[0]


def required_environment(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise SmokeFailure("fixture_environment", "missing_value")
    return value


def load_fixture() -> tuple[str, int, str, str, Path]:
    host = required_environment("MEETERM_SSH_HOST")
    if host != "127.0.0.1":
        raise SmokeFailure("fixture_environment", "loopback_required")
    try:
        port = int(required_environment("MEETERM_SSH_PORT"), 10)
    except ValueError as error:
        raise SmokeFailure("fixture_environment", "invalid_port") from error
    if not 1025 <= port <= 65535:
        raise SmokeFailure("fixture_environment", "invalid_port")

    username = required_environment("MEETERM_SSH_USERNAME")
    if any(character.isspace() or ord(character) < 32 for character in username):
        raise SmokeFailure("fixture_environment", "invalid_username")

    key_path = Path(required_environment("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE"))
    fixture_key_path = Path(required_environment("MEETERM_SSH_PRIVATE_KEY_FILE"))
    if not key_path.is_file() or not fixture_key_path.is_file():
        raise SmokeFailure("fixture_environment", "key_unavailable")
    try:
        key = key_path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise SmokeFailure("fixture_environment", "key_unreadable") from error
    if not key.startswith("-----BEGIN OPENSSH PRIVATE KEY-----") or not key.rstrip().endswith(
        "-----END OPENSSH PRIVATE KEY-----"
    ):
        raise SmokeFailure("fixture_environment", "key_format")
    # Only the unencrypted sibling is used here.  This keeps passphrase bytes
    # out of adb input while the Rust integration test covers encrypted keys.
    # Infer the marker directory from the canonical fixture key path.  The
    # unencrypted sibling is used only for the UI form, but both must belong
    # to the same disposable fixture tree.
    if fixture_key_path.parent != key_path.parent:
        raise SmokeFailure("fixture_environment", "key_tree_mismatch")
    return host, port, username, key, fixture_key_path


def tmux_socket_from_fixture(key_path: Path) -> Path:
    """Resolve the fixture tmux socket without permitting host-server access."""

    root = key_path.parent.resolve()
    if not root.is_dir() or not root.name.startswith("meeterm-ssh-fixture-"):
        raise SmokeFailure("tmux_fixture", "fixture_root_unavailable")
    raw_socket = required_environment("MEETERM_TMUX_SOCKET")
    socket_path = Path(raw_socket)
    if not socket_path.is_absolute():
        raise SmokeFailure("tmux_fixture", "socket_path_invalid")
    try:
        resolved = socket_path.resolve(strict=False)
        relative = resolved.relative_to(root)
    except (OSError, ValueError) as error:
        raise SmokeFailure("tmux_fixture", "socket_path_outside_fixture") from error

    # The fixture publishes $root/tmux/... and the final default socket.  A
    # strict shape check prevents a malformed environment value from causing
    # a bare `tmux` invocation to reach a developer's ordinary server.
    if (
        len(relative.parts) != 3
        or relative.parts[0] != "tmux"
        or relative.parts[1] != f"tmux-{os.getuid()}"
        or resolved.name != "default"
    ):
        raise SmokeFailure("tmux_fixture", "socket_path_invalid")
    if socket_path.is_symlink():
        raise SmokeFailure("tmux_fixture", "socket_path_invalid")
    return resolved


def _tmux_environment(socket_path: Path) -> dict[str, str]:
    """Build a scrubbed environment for local fixture-side tmux commands."""

    environment = os.environ.copy()
    for name in tuple(environment):
        if name.startswith("MEETERM_SSH_"):
            environment.pop(name, None)
    for name in (
        "TMUX",
        "TMUX_PANE",
        "MEETERM_TMUX_SOCKET",
        "MEETERM_TMUX_TMPDIR",
    ):
        environment.pop(name, None)
    # Keep tmux's default path aligned with the fixture, while every command
    # still carries the explicit -S argument below.
    environment["TMUX_TMPDIR"] = str(socket_path.parent.parent)
    return environment


def run_tmux_command(
    socket_path: Path,
    arguments: tuple[str, ...],
    stage: str,
    *,
    allow_failure: bool = False,
    timeout: float = 10.0,
) -> subprocess.CompletedProcess[bytes]:
    """Run one explicit, fixture-scoped tmux command without exposing output."""

    tmux_path = shutil.which(TMUX)
    if tmux_path is None:
        raise SmokeFailure(stage, "tmux_unavailable")
    # /dev/null keeps a developer's tmux hooks/options from changing the
    # disposable two-pane arrangement. The server is still ordinary tmux;
    # only its explicit socket and fixture-scoped environment are isolated.
    command = [tmux_path, "-f", "/dev/null", "-S", str(socket_path), *arguments]
    try:
        result = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=_tmux_environment(socket_path),
            timeout=timeout,
            check=False,
        )
    except (FileNotFoundError, OSError, subprocess.TimeoutExpired) as error:
        raise SmokeFailure(stage, "tmux_unavailable") from error
    if result.returncode != 0 and not allow_failure:
        raise SmokeFailure(stage, "tmux_command_failed")
    return result


def _parse_tmux_flag(value: str, stage: str) -> bool:
    if value in ("1", "true"):
        return True
    if value in ("0", "false"):
        return False
    raise SmokeFailure(stage, "tmux_state_invalid")


def parse_tmux_panes(output: bytes, stage: str = "tmux_fixture") -> list[TmuxPaneRecord]:
    """Parse stable tmux identity, selection, and geometry fields."""

    records: list[TmuxPaneRecord] = []
    seen_panes: set[str] = set()
    try:
        lines = output.decode("utf-8", errors="strict").splitlines()
    except UnicodeDecodeError as error:
        raise SmokeFailure(stage, "tmux_state_invalid") from error
    for line in lines:
        if not line:
            continue
        fields = line.split("\t")
        if len(fields) != 14:
            raise SmokeFailure(stage, "tmux_state_invalid")
        (
            window_id,
            window_name,
            pane_id,
            pid_text,
            pane_index_text,
            active_text,
            pane_width_text,
            pane_height_text,
            pane_left_text,
            pane_top_text,
            pane_right_text,
            pane_bottom_text,
            window_active_text,
            zoomed_text,
        ) = fields
        if not re.fullmatch(r"@[0-9]+", window_id):
            raise SmokeFailure(stage, "tmux_state_invalid")
        if not window_name or any(ord(character) < 32 for character in window_name):
            raise SmokeFailure(stage, "tmux_state_invalid")
        if not re.fullmatch(r"%[0-9]+", pane_id) or pane_id in seen_panes:
            raise SmokeFailure(stage, "tmux_state_invalid")
        if not re.fullmatch(r"[1-9][0-9]*", pid_text):
            raise SmokeFailure(stage, "tmux_state_invalid")
        integer_fields = (
            pane_index_text,
            pane_width_text,
            pane_height_text,
            pane_left_text,
            pane_top_text,
            pane_right_text,
            pane_bottom_text,
        )
        if any(not re.fullmatch(r"[0-9]+", value) for value in integer_fields):
            raise SmokeFailure(stage, "tmux_state_invalid")
        pane_index = int(pane_index_text)
        pane_width = int(pane_width_text)
        pane_height = int(pane_height_text)
        pane_left = int(pane_left_text)
        pane_top = int(pane_top_text)
        pane_right = int(pane_right_text)
        pane_bottom = int(pane_bottom_text)
        if (
            pane_width <= 0
            or pane_height <= 0
            or pane_right <= pane_left
            or pane_bottom <= pane_top
        ):
            raise SmokeFailure(stage, "tmux_state_invalid")
        seen_panes.add(pane_id)
        records.append(
            TmuxPaneRecord(
                window_id=window_id,
                window_name=window_name,
                pane_id=pane_id,
                pane_pid=int(pid_text),
                pane_index=pane_index,
                active=_parse_tmux_flag(active_text, stage),
                pane_width=pane_width,
                pane_height=pane_height,
                pane_left=pane_left,
                pane_top=pane_top,
                pane_right=pane_right,
                pane_bottom=pane_bottom,
                window_active=_parse_tmux_flag(window_active_text, stage),
                zoomed=_parse_tmux_flag(zoomed_text, stage),
            )
        )
    if not records:
        raise SmokeFailure(stage, "tmux_panes_unavailable")
    return records


def list_tmux_panes(socket_path: Path, stage: str) -> list[TmuxPaneRecord]:
    format_string = (
        "#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_index}\t"
        "#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_left}\t#{pane_top}\t"
        "#{pane_right}\t#{pane_bottom}\t#{window_active}\t#{window_zoomed_flag}"
    )
    result = run_tmux_command(
        socket_path,
        ("list-panes", "-s", "-t", "=meeterm", "-F", format_string),
        stage,
    )
    return parse_tmux_panes(result.stdout, stage)


def pane_layout_signature(
    records: list[TmuxPaneRecord],
) -> tuple[tuple[str, str, str, int, int], ...]:
    """Return stable window/pane/PID/index identity tuples for handoff checks."""

    return tuple(
        sorted(
            (
                record.window_id,
                record.window_name,
                record.pane_id,
                record.pane_pid,
                record.pane_index,
            )
            for record in records
        )
    )


def pane_split_shape(
    records: list[TmuxPaneRecord],
) -> tuple[tuple[str, tuple[tuple[int, bool, bool, bool, bool], ...]], ...]:
    """Normalize pane geometry to split edges, ignoring a resize's dimensions."""

    by_window: dict[str, list[TmuxPaneRecord]] = {}
    for record in records:
        by_window.setdefault(record.window_id, []).append(record)
    shapes: list[tuple[str, tuple[tuple[int, bool, bool, bool, bool], ...]]] = []
    for window_id, panes in by_window.items():
        min_left = min(pane.pane_left for pane in panes)
        min_top = min(pane.pane_top for pane in panes)
        max_right = max(pane.pane_right for pane in panes)
        max_bottom = max(pane.pane_bottom for pane in panes)
        shape = tuple(
            sorted(
                (
                    pane.pane_index,
                    pane.pane_left > min_left,
                    pane.pane_top > min_top,
                    pane.pane_right < max_right,
                    pane.pane_bottom < max_bottom,
                )
                for pane in panes
            )
        )
        shapes.append((window_id, shape))
    return tuple(sorted(shapes))


def assert_fixture_layout_preserved(
    before: list[TmuxPaneRecord],
    after: list[TmuxPaneRecord],
    stage: str,
) -> None:
    if pane_layout_signature(before) != pane_layout_signature(after):
        raise SmokeFailure(stage, "pane_layout_changed")
    if pane_split_shape(before) != pane_split_shape(after):
        raise SmokeFailure(stage, "pane_split_changed")
    if len({record.window_id for record in after}) != len(FIXTURE_WINDOW_NAMES):
        raise SmokeFailure(stage, "window_layout_changed")
    if any(record.zoomed for record in after):
        raise SmokeFailure(stage, "pane_layout_still_zoomed")


def assert_fixture_identity_preserved(
    before: list[TmuxPaneRecord],
    after: list[TmuxPaneRecord],
    stage: str,
) -> None:
    """Check durable pane identities while mobile presentation may be zoomed."""

    if pane_layout_signature(before) != pane_layout_signature(after):
        raise SmokeFailure(stage, "pane_layout_changed")
    window_ids = {record.window_id for record in after}
    if len(window_ids) != len(FIXTURE_WINDOW_NAMES) or any(
        sum(record.window_id == window_id for record in after)
        != FIXTURE_PANES_PER_WINDOW
        for window_id in window_ids
    ):
        raise SmokeFailure(stage, "window_layout_changed")


def records_for_window(
    records: list[TmuxPaneRecord], window_id: str
) -> list[TmuxPaneRecord]:
    return [record for record in records if record.window_id == window_id]


def _selection_matches(
    records: list[TmuxPaneRecord],
    pane_id: str,
    pane_pid: int,
) -> bool:
    if len(records) != FIXTURE_PANE_COUNT:
        return False
    target = next((record for record in records if record.pane_id == pane_id), None)
    if target is None or target.pane_pid != pane_pid:
        return False
    active = [record for record in records if record.active and record.window_active]
    active_windows = [record for record in records if record.window_active]
    return (
        len({record.window_id for record in records}) == len(FIXTURE_WINDOW_NAMES)
        and target.active
        and target.window_active
        and target.zoomed
        and len(active) == 1
        and len(active_windows) == FIXTURE_PANES_PER_WINDOW
    )


def wait_for_tmux_selection(
    socket_path: Path,
    pane_id: str,
    pane_pid: int,
    stage: str,
    timeout: float = RECONNECT_TIMEOUT,
) -> list[TmuxPaneRecord]:
    """Wait for real tmux selection/zoom state, including the pane PID."""

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            records = list_tmux_panes(socket_path, stage)
        except SmokeFailure:
            time.sleep(0.2)
            continue
        if _selection_matches(records, pane_id, pane_pid):
            return records
        time.sleep(0.2)
    raise SmokeFailure(stage, "tmux_selection_timeout")


def prepare_tmux_fixture(socket_path: Path) -> list[TmuxPaneRecord]:
    """Create two windows with two panes each and select the first pane."""

    stage = "tmux_fixture"
    try:
        socket_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        socket_path.parent.chmod(0o700)
    except OSError as error:
        raise SmokeFailure(stage, "socket_directory_unavailable") from error
    if socket_path.is_symlink():
        raise SmokeFailure(stage, "socket_path_invalid")
    if socket_path.exists():
        metadata = socket_path.stat()
        if not stat.S_ISSOCK(metadata.st_mode) or metadata.st_uid != os.getuid():
            raise SmokeFailure(stage, "socket_path_invalid")

    existing = run_tmux_command(
        socket_path,
        ("list-sessions", "-F", "#{session_name}"),
        stage,
        allow_failure=True,
    )
    # fixture.py starts an empty -D server with /dev/null configuration. The
    # validated private socket may already exist, but no session may exist.
    if existing.returncode == 0 and existing.stdout.strip():
        raise SmokeFailure(stage, "session_already_exists")

    run_tmux_command(
        socket_path,
        ("new-session", "-d", "-s", "meeterm", "-n", "smoke", "/bin/sh", "-i"),
        stage,
    )
    initial = list_tmux_panes(socket_path, stage)
    if len(initial) != 1:
        raise SmokeFailure(stage, "initial_pane_count_invalid")
    first = initial[0]

    run_tmux_command(
        socket_path,
        ("split-window", "-h", "-t", first.pane_id, "/bin/sh", "-i"),
        stage,
    )
    split = list_tmux_panes(socket_path, stage)
    if len(split) != 2 or len({record.window_id for record in split}) != 1:
        raise SmokeFailure(stage, "pane_layout_invalid")

    run_tmux_command(
        socket_path,
        (
            "new-window",
            "-d",
            "-t",
            "=meeterm:",
            "-n",
            FIXTURE_WINDOW_NAMES[1],
            "/bin/sh",
            "-i",
        ),
        stage,
    )
    second_window = list_tmux_panes(socket_path, stage)
    second_window_ids = {
        record.window_id for record in second_window if record.window_id != first.window_id
    }
    if len(second_window_ids) != 1:
        raise SmokeFailure(stage, "window_layout_invalid")
    second_first = next(
        record for record in second_window if record.window_id in second_window_ids
    )
    run_tmux_command(
        socket_path,
        ("split-window", "-h", "-t", second_first.pane_id, "/bin/sh", "-i"),
        stage,
    )
    run_tmux_command(socket_path, ("select-window", "-t", first.window_id), stage)
    run_tmux_command(socket_path, ("select-pane", "-t", first.pane_id), stage)
    selected = list_tmux_panes(socket_path, stage)
    if len(selected) != FIXTURE_PANE_COUNT or not any(
        record.pane_id == first.pane_id
        and record.active
        and record.window_active
        and not record.zoomed
        for record in selected
    ):
        raise SmokeFailure(stage, "initial_selection_invalid")
    if sum(record.active and record.window_active for record in selected) != 1:
        raise SmokeFailure(stage, "initial_selection_invalid")
    if len({record.window_id for record in selected}) != len(FIXTURE_WINDOW_NAMES):
        raise SmokeFailure(stage, "window_layout_invalid")
    if any(
        sum(record.window_id == window_id for record in selected) != FIXTURE_PANES_PER_WINDOW
        for window_id in {record.window_id for record in selected}
    ):
        raise SmokeFailure(stage, "pane_layout_invalid")
    return selected


def find_node(
    nodes: list[Node],
    *,
    text: str | None = None,
    content_description: str | None = None,
    class_fragment: str | None = None,
) -> Node | None:
    for node in nodes:
        if not node.visible_to_user or not node.enabled:
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if (
            text is not None
            and node.text != text
            and node.content_description != text
        ):
            continue
        if content_description is not None and node.content_description != content_description:
            continue
        if class_fragment is not None and class_fragment not in node.class_name:
            continue
        return node
    return None


def find_node_with_content_descriptions(
    nodes: list[Node],
    descriptions: tuple[str, ...],
    *,
    class_fragment: str | None = None,
) -> Node | None:
    """Match one of a deliberately allowlisted set of RN descriptions.

    React Native concatenates ``accessibilityValue.text`` to
    ``accessibilityLabel`` in Android's ``contentDescription``.  The private
    key editor therefore exposes either its empty or entered state alongside
    the label; accepting only those two known values avoids a broad prefix
    match that could select a different field.
    """

    for description in descriptions:
        node = find_node(
            nodes,
            content_description=description,
            class_fragment=class_fragment,
        )
        if node is not None:
            return node
    return None


def find_private_key_editor(
    nodes: list[Node], *, include_invisible: bool = False
) -> Node | None:
    """Find the explicitly labeled private-key editor in a changing layout.

    Android can expose the React Native multiline TextInput as an ``EditText``
    child on one accessibility pass and as its labeled wrapper on another
    while the keyboard is changing the ScrollView viewport.  Prefer the real
    editor when present, but keep the exact label as the identity boundary;
    this never falls back to a coordinate or to an unrelated text field.
    """

    candidates: list[Node] = []
    for node in nodes:
        if not node.enabled or (not include_invisible and not node.visible_to_user):
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if node.content_description not in PRIVATE_KEY_ACCESSIBILITY_LABELS:
            continue
        candidates.append(node)
    if not candidates:
        return None
    return next(
        (node for node in candidates if "EditText" in node.class_name),
        candidates[0],
    )


def find_node_casefold(nodes: list[Node], text: str) -> Node | None:
    target = text.casefold()
    for node in nodes:
        if not node.visible_to_user or not node.enabled:
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if (
            node.text.casefold() == target
            or node.content_description.casefold() == target
        ):
            return node
    return None


def content_description_has_label(value: str, label: str) -> bool:
    """Match a React Native label with its optional Android value suffix.

    Android may expose a TextInput's ``accessibilityLabel`` as either the
    label alone or ``label, value`` after the controlled value changes.  The
    suffix is intentionally not interpreted or printed; it only identifies
    the field whose text is read back below.
    """

    return value == label or value.startswith(f"{label}, ")


def find_text_input(nodes: list[Node], label: str) -> Node | None:
    """Find one visible enabled TextInput for a stable form label."""

    for node in nodes:
        if not node.visible_to_user or not node.enabled:
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if "EditText" not in node.class_name:
            continue
        if content_description_has_label(node.content_description, label):
            return node
    return None


def accessible_label(node: Node) -> str:
    """Return the stable user-facing label exposed by a UI node."""

    return node.content_description or node.text


def pane_id_from_node(node: Node) -> str | None:
    """Extract a tmux pane ID from the required accessibility label."""

    match = PANE_LABEL_PATTERN.fullmatch(accessible_label(node).strip())
    return match.group(1) if match is not None else None


def workspace_id_from_node(node: Node) -> str | None:
    resource_id = node.resource_id.removeprefix(f"{PACKAGE}:id/")
    match = re.fullmatch(r"workspace-row-(@[0-9]+)", resource_id)
    return match.group(1) if match is not None else None


def find_workspace_nodes(nodes: list[Node]) -> list[Node]:
    """Return one stable test-ID row for each tmux workspace window."""

    workspaces: dict[str, Node] = {}
    for node in nodes:
        window_id = workspace_id_from_node(node)
        if (
            not node.visible_to_user
            or not node.enabled
            or window_id is None
            or WORKSPACE_LABEL_PATTERN.fullmatch(accessible_label(node).strip()) is None
        ):
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        previous = workspaces.get(window_id)
        if previous is None or (node.selected and not previous.selected):
            workspaces[window_id] = node
    return list(workspaces.values())


def find_workspace_node(nodes: list[Node]) -> Node | None:
    workspaces = find_workspace_nodes(nodes)
    return next((node for node in workspaces if node.selected), None) or next(
        iter(workspaces), None
    )


def find_node_with_labels(nodes: list[Node], labels: tuple[str, ...]) -> Node | None:
    """Find one exact, visible control from an allowlisted label set."""

    for label in labels:
        node = find_node_casefold(nodes, label)
        if node is not None:
            return node
    return None


def wait_for_node_with_labels(
    device: AndroidDevice,
    stage: str,
    labels: tuple[str, ...],
    *,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    deadline = time.monotonic() + timeout
    last_dump_failure: SmokeFailure | None = None
    hierarchy_seen = False
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
            hierarchy_seen = True
        except SmokeFailure as error:
            last_dump_failure = error
            time.sleep(0.2)
            continue
        node = find_node_with_labels(nodes, labels)
        if node is not None:
            return node
        time.sleep(0.2)
    if not hierarchy_seen and last_dump_failure is not None:
        raise SmokeFailure(stage, last_dump_failure.reason)
    raise SmokeFailure(stage, "ui_timeout")


def wait_for_workspace_count(
    device: AndroidDevice,
    stage: str,
    *,
    count: int,
    exact: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> list[Node]:
    if count < 1:
        raise SmokeFailure(stage, "invalid_workspace_count")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        workspaces = find_workspace_nodes(nodes)
        if (len(workspaces) == count if exact else len(workspaces) >= count):
            return workspaces
        time.sleep(0.2)
    raise SmokeFailure(stage, "ui_timeout")


def wait_for_workspace(
    device: AndroidDevice,
    stage: str,
    *,
    label: str | None = None,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    """Wait for a synchronized tmux window label."""

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        if label is None:
            workspace = find_workspace_node(nodes)
        else:
            # The selected tmux window need not be the workspace requested by
            # the caller (for example immediately after switching windows).
            # Search all visible rows when an exact label is provided instead
            # of letting the selected row mask the desired non-selected one.
            workspace = next(
                (
                    candidate
                    for candidate in find_workspace_nodes(nodes)
                    if accessible_label(candidate) == label
                ),
                None,
            )
        if workspace is not None:
            return workspace
        time.sleep(0.2)
    raise SmokeFailure(stage, "ui_timeout")


def find_pane_nodes(nodes: list[Node]) -> list[Node]:
    """Return one usable accessibility node for each pane runtime ID."""

    panes: dict[str, Node] = {}
    for node in nodes:
        if not node.visible_to_user or not node.enabled:
            continue
        found_pane_id = pane_id_from_node(node)
        if found_pane_id is None:
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        previous = panes.get(found_pane_id)
        # A provider may expose both a wrapper and its accessible child.  If
        # that happens, prefer the node carrying the selected state so the
        # wait below observes the same tab state the user sees.
        if previous is None or (node.selected and not previous.selected):
            panes[found_pane_id] = node
    return list(panes.values())


def find_pane_node(
    nodes: list[Node],
    pane_id: str | None = None,
    *,
    selected: bool | None = None,
) -> Node | None:
    for node in find_pane_nodes(nodes):
        if pane_id is not None and pane_id_from_node(node) != pane_id:
            continue
        if selected is not None and node.selected != selected:
            continue
        return node
    return None


def wait_for_pane(
    device: AndroidDevice,
    stage: str,
    *,
    pane_id: str | None = None,
    selected: bool | None = None,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    """Wait for a pane tab and, when requested, its selected accessibility state."""

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        node = find_pane_node(nodes, pane_id, selected=selected)
        if node is not None:
            return node
        time.sleep(0.2)
    raise SmokeFailure(stage, "ui_timeout")


def wait_for_panes(
    device: AndroidDevice,
    stage: str,
    *,
    count: int,
    selected_count: int | None = None,
    exact: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> list[Node]:
    """Wait until the accessibility tree exposes the requested pane tabs."""

    if (
        count < 1
        or (selected_count is not None and not 0 <= selected_count <= count)
    ):
        raise SmokeFailure(stage, "invalid_pane_count")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        panes = find_pane_nodes(nodes)
        count_matches = len(panes) == count if exact else len(panes) >= count
        if count_matches and (
            selected_count is None
            or sum(node.selected for node in panes) == selected_count
        ):
            return panes
        time.sleep(0.2)
    raise SmokeFailure(stage, "ui_timeout")


def find_terminal_node(nodes: list[Node]) -> Node | None:
    """Locate the native terminal surface exposed by the app."""

    for class_fragment in ("MeetermTerminalView", "GLSurfaceView"):
        node = find_node(nodes, class_fragment=class_fragment)
        if node is not None and node.visible_to_user and node.enabled:
            return node

    # Expo/RN may expose only the native child as a generic View.  Choose the
    # largest visible non-text surface below the toolbar; this remains a
    # geometry-derived fallback and never guesses a hard-coded coordinate.
    candidates = [
        node
        for node in nodes
        if node.visible_to_user
        and node.enabled
        and node.bounds[1] > 50
        and "TextView" not in node.class_name
        and "EditText" not in node.class_name
        and not node.scrollable
    ]
    return max(
        candidates,
        key=lambda node: (node.bounds[2] - node.bounds[0])
        * (node.bounds[3] - node.bounds[1]),
        default=None,
    )


def wait_for_terminal(device: AndroidDevice, stage: str, timeout: float = DEFAULT_UI_TIMEOUT) -> Node:
    """Wait for the selected pane's native terminal surface."""

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        node = find_terminal_node(nodes)
        if node is not None:
            return node
        time.sleep(0.2)
    raise SmokeFailure(stage, "terminal_view_unavailable")


def reverse_local_mapping_exists(output: str, port: int) -> bool:
    """Recognize adb reverse output with or without its serial prefix."""

    local = f"tcp:{port}"
    for line in output.splitlines():
        fields = line.split()
        if len(fields) >= 2 and fields[-2] == local:
            return True
    return False


def screen_bounds(nodes: list[Node]) -> tuple[int, int, int, int]:
    right = max((node.bounds[2] for node in nodes), default=1080)
    bottom = max((node.bounds[3] for node in nodes), default=1920)
    return (0, 0, max(right, 1), max(bottom, 1))


def scroll_container_bounds(nodes: list[Node]) -> tuple[int, int, int, int] | None:
    """Return the largest usable accessibility scroll container.

    A full-screen swipe is ambiguous when an Android IME is open: it may
    dismiss the keyboard, hit the modal window, or scroll a parent behind the
    form.  Prefer the actual ScrollView reported by UIAutomator and keep the
    gesture inside its bounds.  A node can report itself as scrollable while
    its current viewport is only a few pixels tall, so reject those bounds and
    let the caller retry after the layout settles.
    """

    candidates = [
        node
        for node in nodes
        if node.scrollable
        and node.visible_to_user
        and node.enabled
        and node.bounds[2] - node.bounds[0] >= 100
        and node.bounds[3] - node.bounds[1] >= 100
    ]
    if not candidates:
        return None
    return max(
        (node.bounds for node in candidates),
        key=lambda bounds: (bounds[2] - bounds[0]) * (bounds[3] - bounds[1]),
    )


def scroll_target_bounds(nodes: list[Node]) -> tuple[int, int, int, int] | None:
    """Return gesture bounds for the form, without touching system chrome."""

    return scroll_container_bounds(nodes)


def form_scroll_gutter_x(
    nodes: list[Node], bounds: tuple[int, int, int, int]
) -> int:
    """Choose the form padding beside editors for an outer ScrollView gesture."""

    left, _top, right, _bottom = bounds
    width = right - left
    if width < 4:
        return left
    interactive_lefts = [
        node.bounds[0]
        for node in nodes
        if node.visible_to_user
        and node.enabled
        and ("EditText" in node.class_name or "Switch" in node.class_name)
        and left < node.bounds[0] < right
    ]
    if interactive_lefts:
        # React Native's form content has horizontal padding. Staying halfway
        # into that observed gap avoids a multiline TextInput consuming the
        # vertical gesture while remaining inside the ScrollView.
        gap = min(interactive_lefts) - left
        if gap >= 4:
            return left + gap // 2
    return left + max(1, min(width - 1, width // 32))


def wait_for_node(
    device: AndroidDevice,
    stage: str,
    *,
    text: str | None = None,
    content_description: str | None = None,
    content_descriptions: tuple[str, ...] | None = None,
    class_fragment: str | None = None,
    scroll: bool = False,
    scroll_gutter: bool = False,
    scroll_toward_start: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    deadline = time.monotonic() + timeout
    last_swipe_at = 0.0
    last_dump_failure: SmokeFailure | None = None
    hierarchy_seen = False
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
            hierarchy_seen = True
        except SmokeFailure as error:
            last_dump_failure = error
            time.sleep(0.2)
            continue
        if content_descriptions is not None:
            node = find_node_with_content_descriptions(
                nodes,
                content_descriptions,
                class_fragment=class_fragment,
            )
        else:
            node = find_node(
                nodes,
                text=text,
                content_description=content_description,
                class_fragment=class_fragment,
            )
        if node is not None:
            return node
        if scroll:
            bounds = scroll_target_bounds(nodes)
            now = time.monotonic()
            # Do not send a new gesture while the previous one is still being
            # applied. Spacing gestures gives the layout a chance to settle
            # before the next accessibility dump.
            if bounds is not None and now - last_swipe_at >= 0.8:
                if scroll_gutter:
                    device.input_swipe(
                        bounds,
                        stage,
                        x=form_scroll_gutter_x(nodes, bounds),
                        toward_start=scroll_toward_start,
                    )
                else:
                    device.input_swipe(bounds, stage)
                last_swipe_at = now
                time.sleep(0.5)
            else:
                time.sleep(0.2)
        else:
            time.sleep(0.2)
    if not hierarchy_seen and last_dump_failure is not None:
        raise SmokeFailure(stage, last_dump_failure.reason)
    raise SmokeFailure(stage, "ui_timeout")


def wait_for_text_input(
    device: AndroidDevice,
    stage: str,
    label: str,
    *,
    scroll: bool = False,
    scroll_gutter: bool = False,
    scroll_toward_start: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    """Wait for a labeled TextInput without assuming its value is in a label."""

    deadline = time.monotonic() + timeout
    last_swipe_at = 0.0
    last_dump_failure: SmokeFailure | None = None
    hierarchy_seen = False
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
            hierarchy_seen = True
        except SmokeFailure as error:
            last_dump_failure = error
            time.sleep(0.2)
            continue
        node = find_text_input(nodes, label)
        if node is not None:
            return node
        if scroll:
            bounds = scroll_target_bounds(nodes)
            now = time.monotonic()
            if bounds is not None and now - last_swipe_at >= 0.8:
                if scroll_gutter:
                    device.input_swipe(
                        bounds,
                        stage,
                        x=form_scroll_gutter_x(nodes, bounds),
                        toward_start=scroll_toward_start,
                    )
                else:
                    device.input_swipe(bounds, stage)
                last_swipe_at = now
                time.sleep(0.5)
            else:
                time.sleep(0.2)
        else:
            time.sleep(0.2)
    if not hierarchy_seen and last_dump_failure is not None:
        raise SmokeFailure(stage, last_dump_failure.reason)
    raise SmokeFailure(stage, "ui_timeout")


def wait_for_private_key_editor(
    device: AndroidDevice,
    stage: str,
    *,
    scroll_gutter: bool = False,
    scroll_toward_start: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    """Wait for the labeled multiline editor while scrolling the form."""

    deadline = time.monotonic() + timeout
    last_swipe_at = 0.0
    last_dump_failure: SmokeFailure | None = None
    hierarchy_seen = False
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
            hierarchy_seen = True
        except SmokeFailure as error:
            last_dump_failure = error
            time.sleep(0.2)
            continue
        editor = find_private_key_editor(nodes)
        if editor is not None:
            return editor
        bounds = scroll_target_bounds(nodes)
        now = time.monotonic()
        if bounds is not None and now - last_swipe_at >= 0.8:
            if scroll_gutter:
                device.input_swipe(
                    bounds,
                    stage,
                    x=form_scroll_gutter_x(nodes, bounds),
                    toward_start=scroll_toward_start,
                )
            else:
                device.input_swipe(bounds, stage)
            last_swipe_at = now
            time.sleep(0.5)
        else:
            time.sleep(0.2)
    if not hierarchy_seen and last_dump_failure is not None:
        raise SmokeFailure(stage, last_dump_failure.reason)
    raise SmokeFailure(stage, "editor_unavailable")


def wait_for_field_value(
    device: AndroidDevice,
    stage: str,
    label: str,
    expected: str,
    *,
    timeout: float = FIELD_READBACK_TIMEOUT,
) -> Node:
    """Wait for a controlled TextInput to expose a settled expected value.

    The first accessibility dump after ``adb shell input text`` can contain
    the old value.  Waiting for two identical reads avoids sending a duplicate
    value while keeping all field contents in memory.
    """

    deadline = time.monotonic() + timeout
    previous: str | None = None
    unchanged_since = time.monotonic()
    last_dump_failure: SmokeFailure | None = None
    while time.monotonic() < deadline:
        try:
            node = find_text_input(device.dump_ui(), label)
        except SmokeFailure as error:
            last_dump_failure = error
            time.sleep(FIELD_SETTLE_SECONDS)
            continue
        if node is None:
            # A system or third-party Activity can replace the foreground
            # window between the pre-input check and controlled-value
            # readback. Preserve that actionable classification instead of
            # reporting the resulting missing editor as an input failure.
            device.assert_foreground(stage)
            raise SmokeFailure(stage, "field_unavailable")
        now = time.monotonic()
        if node.text != previous:
            previous = node.text
            unchanged_since = now
        elif node.text == expected and now - unchanged_since >= FIELD_SETTLE_SECONDS:
            return node
        time.sleep(FIELD_SETTLE_SECONDS)
    if last_dump_failure is not None and previous is None:
        device.assert_foreground(stage)
        raise SmokeFailure(stage, last_dump_failure.reason)
    device.assert_foreground(stage)
    raise SmokeFailure(stage, "entry_mismatch")


def tap_node(device: AndroidDevice, node: Node, stage: str) -> None:
    left, top, right, bottom = node.bounds
    if right <= left or bottom <= top:
        raise SmokeFailure(stage, "invalid_bounds")
    x, y = node.center
    device.input_tap(x, y, stage)


def set_toggle(
    device: AndroidDevice,
    label: str,
    desired: bool,
    stage: str,
    *,
    scroll: bool = True,
    scroll_gutter: bool = False,
    scroll_toward_start: bool = False,
) -> None:
    """Set one labeled native switch and verify its checked state."""

    node = wait_for_node(
        device,
        stage,
        content_description=label,
        scroll=scroll,
        scroll_gutter=scroll_gutter,
        scroll_toward_start=scroll_toward_start,
    )
    if node.checked == desired:
        return
    tap_node(device, node, stage)
    deadline = time.monotonic() + FIELD_READBACK_TIMEOUT
    while time.monotonic() < deadline:
        try:
            current = find_node_casefold(device.dump_ui(), label)
        except SmokeFailure:
            time.sleep(FIELD_SETTLE_SECONDS)
            continue
        if current is not None and current.checked == desired:
            return
        time.sleep(FIELD_SETTLE_SECONDS)
    raise SmokeFailure(stage, "toggle_mismatch")


def selection_drag_points(
    node: Node,
    *,
    columns: int,
    character_count: int,
) -> tuple[tuple[int, int], tuple[int, int]]:
    """Map a known first-row ASCII range to density-independent touch points."""

    left, top, right, bottom = node.bounds
    width = right - left
    height = bottom - top
    if (
        width <= 1
        or height <= 1
        or columns < 2
        or character_count < 1
        or character_count > columns
    ):
        raise SmokeFailure("terminal_selection", "invalid_selection_geometry")
    cell_width = width / columns
    start_x = left + max(1, min(width - 1, int(cell_width * 0.5)))
    end_x = left + max(
        1,
        min(width - 1, int(cell_width * (character_count - 0.5))),
    )
    # Android's terminal font metrics use a cell height close to twice the
    # monospace advance. One advance below the top lands near row-zero center
    # without including the 48dp native key row at the bottom of the view.
    row_zero_y = top + max(1, min(height - 1, round(cell_width)))
    return (start_x, row_zero_y), (end_x, row_zero_y)


def focus_terminal(device: AndroidDevice, node: Node, stage: str) -> None:
    """Tap a native terminal and let its IME connection settle.

    The terminal is a native surface rather than an RN text input. A tap
    posts ``showSoftInput`` and the corresponding InputConnection can attach
    after the accessibility node is already visible. Keep the delay bounded
    and apply it at every terminal route boundary before injecting a command.
    """

    tap_node(device, node, stage)
    time.sleep(TERMINAL_FOCUS_SETTLE_SECONDS)


def clear_field(device: AndroidDevice, stage: str, delete_count: int) -> None:
    if delete_count <= 0:
        return
    device.input_keyevents(
        (KEYCODE_MOVE_END, *([KEYCODE_DEL] * delete_count)),
        stage,
    )


def fill_field(
    device: AndroidDevice,
    content_description: str,
    value: str,
    stage: str,
    *,
    scroll: bool = True,
    scroll_gutter: bool = False,
    scroll_toward_start: bool = False,
    clear_count: int = 0,
) -> None:
    for attempt in range(FIELD_INPUT_MAX_ATTEMPTS):
        node = wait_for_text_input(
            device,
            stage,
            label=content_description,
            scroll=scroll,
            scroll_gutter=scroll_gutter,
            scroll_toward_start=scroll_toward_start,
            timeout=DEFAULT_UI_TIMEOUT if attempt == 0 else FIELD_READBACK_TIMEOUT,
        )
        tap_node(device, node, stage)
        # A failed readback means the first burst may have been dropped or
        # partially applied. Clear the value observed on the retry pass before
        # sending it again; this avoids appending a duplicate suffix.
        retry_clear_count = clear_count if attempt == 0 else len(node.text)
        clear_field(device, stage, retry_clear_count)
        if value:
            device.input_text(value, stage)
            # ``adb input text`` is delivered through the device's active
            # IME. Pixel 3 Japanese Gboard keeps ASCII-looking host/name input
            # as a composition and can expose full-width digits, punctuation,
            # or kana in the controlled TextInput. Android KEYCODE_F10 asks a
            # Japanese IME to convert that current composition to half-width
            # alphanumeric; Latin IMEs ignore it. Public fields retain the
            # exact whole-value readback below; the key editor has its own
            # secret-safe exact-prefix gate.
            device.input_keyevent(KEYCODE_F10, stage)
        # Compare in memory only. In particular, do not report entered text in
        # a failure: this helper also protects against an incorrectly cleared
        # port.
        try:
            wait_for_field_value(device, stage, content_description, value)
            return
        except SmokeFailure as error:
            if (
                error.reason != "entry_mismatch"
                or attempt + 1 >= FIELD_INPUT_MAX_ATTEMPTS
            ):
                raise
            time.sleep(FIELD_SETTLE_SECONDS)


def fill_multiline_key(
    device: AndroidDevice, key: str, *, return_from_form_end: bool = False
) -> None:
    lines = key.splitlines()
    if not lines:
        raise SmokeFailure("private_key_input", "key_empty")
    stage = "private_key_input"
    editor = wait_for_private_key_editor(
        device,
        stage,
        scroll_gutter=return_from_form_end,
        scroll_toward_start=return_from_form_end,
    )

    def tap_editor(nodes: list[Node], candidate: Node) -> None:
        left, top, right, bottom = candidate.bounds
        viewport = scroll_container_bounds(nodes)
        if viewport is not None:
            left, top = max(left, viewport[0]), max(top, viewport[1])
            right, bottom = min(right, viewport[2]), min(bottom, viewport[3])
        if right <= left or bottom - top < 12:
            raise SmokeFailure(stage, "editor_not_visible")
        device.input_tap(
            (left + right) // 2,
            top + min(24, (bottom - top) // 2),
            stage,
        )

    # A fixed-height multiline field can move out of the visible viewport as
    # the IME opens.  Keep the exact labeled node as the input identity and
    # allow the accessibility tree several settled passes before using one
    # bounded keyboard-dismiss/re-scroll recovery.  No credential bytes are
    # sent until the labeled editor is present again.
    nodes = device.dump_ui()
    tap_editor(nodes, editor)
    focus_deadline = time.monotonic() + 8.0
    keyboard_reset = False
    missing_since: float | None = None
    while time.monotonic() < focus_deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.25)
            continue
        editor = find_private_key_editor(nodes, include_invisible=True)
        if editor is None:
            now = time.monotonic()
            missing_since = missing_since or now
            if not keyboard_reset and now - missing_since >= 1.5:
                # The first BACK is consumed by the IME when the field was
                # focused. It exposes the editor without closing the form.
                device.dismiss_keyboard(stage)
                keyboard_reset = True
                time.sleep(0.5)
                editor = wait_for_private_key_editor(device, stage, timeout=5.0)
                nodes = device.dump_ui()
                tap_editor(nodes, editor)
                missing_since = None
            else:
                time.sleep(0.25)
            continue
        missing_since = None
        # RN may expose a labeled wrapper without a focus bit. The exact label
        # is still sufficient for that wrapper; a real EditText must report
        # focus before any input is sent.
        if "EditText" not in editor.class_name or editor.focused:
            break
        if editor.visible_to_user:
            tap_editor(nodes, editor)
        time.sleep(0.25)
    else:
        raise SmokeFailure(stage, "editor_not_focused")

    deadline = time.monotonic() + KEY_INPUT_TIMEOUT
    expected = ""
    enter_key_prefix(device, expected, deadline=deadline)
    for index, line in enumerate(lines):
        # Every operation starts from settled, verified editor state. Never
        # replay a batch just because Android accepted only part of it.
        for offset in range(0, len(line), 16):
            chunk = line[offset:offset + 16]
            expected += chunk
            enter_key_prefix(device, expected, deadline=deadline)
        if index + 1 < len(lines):
            expected += "\n"
            enter_key_prefix(device, expected, deadline=deadline)


def verify_key_readback(
    device: AndroidDevice, expected: str, *, deadline: float | None = None
) -> str:
    """Return a settled exact prefix, keeping all credential text in memory.

    UIAutomator waits for accessibility idle, but a controlled React input
    can still update after the first read. Require a second identical read
    after a quiet interval, including when the first value is a full match.
    A real EditText that loses focus and non-prefix text are never recoverable
    by replaying input; a labeled React Native wrapper may omit the focus bit.
    """

    read_deadline = time.monotonic() + KEY_READBACK_TIMEOUT
    deadline = min(deadline, read_deadline) if deadline is not None else read_deadline
    previous: str | None = None
    unchanged_since = time.monotonic()
    editor_missing = False
    while time.monotonic() < deadline:
        nodes = device.dump_ui()
        editor = find_private_key_editor(nodes, include_invisible=True)
        if editor is None:
            # UIAutomator can omit a focused multiline editor for one pass as
            # its internal caret scrolls across long wrapped key lines. Keep
            # the exact identity/focus gate, but allow that transient layout
            # pass to settle. A foreground replacement is still classified
            # immediately and persistent absence remains a bounded failure.
            device.assert_foreground("private_key_input")
            editor_missing = True
            previous = None
            unchanged_since = time.monotonic()
            time.sleep(KEY_INPUT_SETTLE_SECONDS)
            continue
        editor_missing = False
        if "EditText" in editor.class_name and not editor.focused:
            raise SmokeFailure("private_key_input", "editor_lost_focus")
        if not expected.startswith(editor.text):
            raise SmokeFailure("private_key_input", "entry_content_mismatch")
        now = time.monotonic()
        if now >= deadline:
            break
        if editor.text != previous:
            previous = editor.text
            unchanged_since = now
        elif now - unchanged_since >= KEY_INPUT_SETTLE_SECONDS:
            return editor.text
        time.sleep(KEY_INPUT_SETTLE_SECONDS)
    if editor_missing:
        raise SmokeFailure("private_key_input", "editor_unavailable")
    raise SmokeFailure("private_key_input", "entry_not_settled")


def enter_key_prefix(device: AndroidDevice, expected: str, *, deadline: float) -> None:
    """Append only a verified missing suffix, with finite attempts and time."""

    attempts = 0
    while time.monotonic() < deadline:
        observed = verify_key_readback(device, expected, deadline=deadline)
        if observed == expected:
            return
        if time.monotonic() >= deadline:
            break
        if attempts >= KEY_INPUT_MAX_ATTEMPTS:
            raise SmokeFailure("private_key_input", "entry_retry_limit")
        if attempts:
            # Structural counters only, never the credential or raw hierarchy.
            print(
                "Private key input recovery: "
                f"operation={'newline' if expected.endswith(chr(10)) else 'text'} "
                f"attempt={attempts} "
                f"expected_length={len(expected)} observed_length={len(observed)} "
                f"expected_newlines={expected.count(chr(10))} "
                f"observed_newlines={observed.count(chr(10))}",
                flush=True,
            )
        missing = expected[len(observed):]
        if missing.startswith("\n"):
            device.input_keyevent(KEYCODE_ENTER, "private_key_input")
        else:
            device.input_text(missing.split("\n", 1)[0][:16], "private_key_input")
            if time.monotonic() < deadline:
                # Keep every secret chunk behind the same exact-prefix gate,
                # while converting Japanese-IME composition to the literal
                # half-width OpenSSH bytes before its readback. No key text is
                # logged or copied out of the editor.
                device.input_keyevent(KEYCODE_F10, "private_key_input")
        attempts += 1
    raise SmokeFailure("private_key_input", "entry_timeout")


def host_fingerprint_from_nodes(nodes: list[Node]) -> str | None:
    pattern = re.compile(r"SHA256:[A-Za-z0-9+/]+={0,2}")
    for node in nodes:
        for value in (node.text, node.content_description):
            match = pattern.search(value)
            if match:
                return match.group(0)
    return None


def trust_host(device: AndroidDevice, expected_fingerprint: str) -> None:
    title_deadline = time.monotonic() + DEFAULT_UI_TIMEOUT
    actual_fingerprint: str | None = None
    last_observation = "fingerprint_unavailable"
    while time.monotonic() < title_deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        # Record only fixed product error identifiers. Never dump editable
        # form values merely to diagnose a missing host-key dialog.
        known_errors = {
            "Enter a hostname or IP address.": "host_empty",
            "The host cannot contain spaces.": "host_invalid",
            "Use a port from 1 to 65535.": "port_invalid",
            "Enter the SSH username.": "username_empty",
            "The username cannot contain spaces.": "username_invalid",
            "Paste a complete OpenSSH private key, including its BEGIN and END lines.": "key_incomplete",
            "The private key could not be loaded.": "key_decode_failed",
            "The SSH connection could not be established.": "network_failed",
            "The native connection request could not be started.": "native_request_failed",
        }
        for message, reason in known_errors.items():
            if find_node(nodes, text=message) is not None:
                raise SmokeFailure("host_key_prompt", reason)
        for label, observation in (
            ("Connection status is temporarily unavailable.", "state_poll_unavailable"),
            ("Connecting…", "still_connecting"),
            ("Verify host key", "host_pending_without_dialog"),
            ("Connected", "connected_without_driver_approval"),
        ):
            if find_node(nodes, text=label) is not None:
                last_observation = observation
        if find_node_with_content_descriptions(nodes, PRIVATE_KEY_ACCESSIBILITY_LABELS) is not None:
            last_observation = "form_still_open"
        title = find_node(nodes, text="Trust this SSH host?") or find_node(
            nodes, content_description="Trust this SSH host?"
        )
        if title is None:
            time.sleep(0.2)
            continue
        actual_fingerprint = host_fingerprint_from_nodes(nodes)
        if actual_fingerprint is None:
            last_observation = "dialog_fingerprint_unavailable"
            time.sleep(0.2)
            continue
        if actual_fingerprint != expected_fingerprint:
            raise SmokeFailure("host_key_prompt", "fingerprint_mismatch")
        trust_button = find_node_casefold(nodes, "Trust and connect")
        if trust_button is None:
            time.sleep(0.2)
            continue
        tap_node(device, trust_button, "host_key_prompt")
        return
    if actual_fingerprint is None:
        raise SmokeFailure("host_key_prompt", last_observation)
    raise SmokeFailure("host_key_prompt", "trust_button_unavailable")


def shell_quote(value: str) -> str:
    return shlex.quote(value)


def _validate_marker_pid(pane_pid: int | None, stage: str) -> None:
    if pane_pid is not None and (not isinstance(pane_pid, int) or pane_pid <= 0):
        raise SmokeFailure(stage, "invalid_pane_pid")


def session_marker_command(
    marker: str,
    path: Path,
    pane_pid: int | None = None,
) -> str:
    """Build the one-shot marker command sent through the terminal.

    Android's ``input text`` reserves ``%s`` for spaces, so the generated
    command never includes a percent character. The marker is generated
    locally and the path is quoted as a shell argument.
    """

    if not marker or any(character in marker for character in "\r\n%"):
        raise SmokeFailure("remote_marker", "invalid_marker")
    _validate_marker_pid(pane_pid, "remote_marker")
    pid_suffix = ":$$" if pane_pid is not None else ""
    return (
        f"export MEETERM_ANDROID_SESSION_MARKER={shell_quote(marker)}; "
        f"printf \"$MEETERM_ANDROID_SESSION_MARKER{pid_suffix}\\n\" > "
        f"{shell_quote(str(path))}"
    )


def resumed_marker_command(
    marker: str,
    path: Path,
    pane_pid: int | None = None,
) -> str:
    """Build the reconnect assertion that depends on the same tmux shell."""

    if not marker or any(character in marker for character in "\r\n%"):
        raise SmokeFailure("remote_marker_resume", "invalid_marker")
    _validate_marker_pid(pane_pid, "remote_marker_resume")
    resumed_marker = f"{marker}-reconnected"
    if pane_pid is None:
        resumed_marker_literal = shell_quote(resumed_marker + r"\n")
        pid_check = ""
    else:
        resumed_marker_literal = shell_quote(f"{resumed_marker}:{pane_pid}" + r"\n")
        pid_check = f" && [ \"$$\" = {pane_pid} ]"
    return (
        f"if [ \"$MEETERM_ANDROID_SESSION_MARKER\" = {shell_quote(marker)} ]{pid_check}; "
        f"then printf {resumed_marker_literal} >> {shell_quote(str(path))}; fi"
    )


def printf_octal(value: str) -> str:
    return "".join(f"\\{byte:03o}" for byte in value.encode("utf-8"))


def terminal_line(device: AndroidDevice, command: str) -> None:
    # All commands passed here are generated ASCII.  The remote shell output
    # may contain UTF-8, but it never travels through this Python process.
    if any(ord(character) > 127 for character in command):
        raise SmokeFailure("terminal_input", "non_ascii_command")
    terminal_text(device, command)
    device.input_keyevent(KEYCODE_ENTER, "terminal_input")


def terminal_text(device: AndroidDevice, text: str) -> None:
    """Enter generated ASCII without submitting the remote shell line."""

    if any(ord(character) > 127 for character in text):
        raise SmokeFailure("terminal_input", "non_ascii_command")
    # Android's input tool turns text into individual KeyEvents. Pacing small
    # chunks reduces the chance of overwhelming the bounded native SSH queue
    # with a synthetic CI burst while retaining the real native key-event
    # route.
    for offset in range(0, len(text), TERMINAL_INPUT_CHUNK_SIZE):
        chunk = text[offset : offset + TERMINAL_INPUT_CHUNK_SIZE]
        device.input_text(chunk, "terminal_input")
        if offset + TERMINAL_INPUT_CHUNK_SIZE < len(text):
            time.sleep(TERMINAL_INPUT_CHUNK_DELAY_SECONDS)


def make_marker_file(key_path: Path) -> tuple[Path, str]:
    root = key_path.parent
    if not root.is_dir() or not root.name.startswith("meeterm-ssh-fixture-"):
        raise SmokeFailure("remote_marker", "fixture_root_unavailable")
    marker = f"{REMOTE_MARKER_PREFIX}{secrets.token_hex(12)}"
    path = root / f".{marker}.txt"
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(descriptor)
        path.unlink()
    except (FileExistsError, OSError) as error:
        raise SmokeFailure("remote_marker", "marker_file_unavailable") from error
    return path, marker


def make_glyph_stress_file(key_path: Path) -> Path:
    """Write public, deterministic CJK rows inside the disposable fixture."""

    root = key_path.parent
    if not root.is_dir() or not root.name.startswith("meeterm-ssh-fixture-"):
        raise SmokeFailure("daily_glyph_atlas", "fixture_root_unavailable")
    path = root / f".meeterm-glyph-stress-{secrets.token_hex(12)}.txt"
    characters = [chr(0x4E00 + offset) for offset in range(DAILY_GLYPH_STRESS_COUNT)]
    rows = [
        "".join(characters[offset : offset + DAILY_GLYPH_STRESS_COLUMNS])
        for offset in range(0, len(characters), DAILY_GLYPH_STRESS_COLUMNS)
    ]
    rows.append("END 日本語")
    payload = ("\n".join(rows) + "\n").encode("utf-8")
    descriptor: int | None = None
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            descriptor = None
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
    except (FileExistsError, OSError) as error:
        if descriptor is not None:
            os.close(descriptor)
        try:
            path.unlink()
        except OSError:
            pass
        raise SmokeFailure("daily_glyph_atlas", "stress_file_unavailable") from error
    return path


def renderer_atlas_reset_events(device: AndroidDevice, stage: str) -> int:
    """Count only the renderer's fixed, credential-free atlas reset marker."""

    output = device.run(
        ("shell", "logcat", "-d", "-v", "brief", "-s", "MeetermRenderer:I"),
        stage,
        timeout=20.0,
    ).decode("utf-8", errors="replace")
    return len(GLYPH_ATLAS_RESET_PATTERN.findall(output))


def wait_for_new_atlas_reset(
    device: AndroidDevice,
    baseline_events: int,
    stage: str,
    *,
    timeout: float = REMOTE_MARKER_TIMEOUT,
) -> None:
    """Require one reset emitted after this smoke's bounded stress output."""

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        device.assert_foreground(stage)
        if renderer_atlas_reset_events(device, stage) > baseline_events:
            return
        time.sleep(0.3)
    raise SmokeFailure(stage, "atlas_reset_timeout")


def wait_for_file_contents(path: Path, expected: str, stage: str) -> None:
    deadline = time.monotonic() + REMOTE_MARKER_TIMEOUT
    while time.monotonic() < deadline:
        try:
            content = path.read_text(encoding="utf-8") if path.exists() else ""
        except (OSError, UnicodeError) as error:
            raise SmokeFailure(stage, "marker_read_failed") from error
        if len(content) > len(expected):
            raise SmokeFailure(stage, "marker_repeated")
        if content == expected:
            # Let a second execution arrive before accepting the exact result;
            # the host check must distinguish duplicate input.
            time.sleep(0.5)
            try:
                stable = path.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as error:
                raise SmokeFailure(stage, "marker_read_failed") from error
            if stable == expected:
                return
            raise SmokeFailure(stage, "marker_repeated")
        time.sleep(0.2)
    raise SmokeFailure(stage, "marker_timeout")


def wait_for_marker(path: Path, marker: str) -> None:
    """Wait for exactly one marker line (kept for existing smoke callers)."""

    wait_for_file_contents(path, f"{marker}\n", "remote_marker")


def write_artifact(path: Path, contents: str) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
    except OSError:
        # Artifact collection must not turn a sanitized validation result into
        # a traceback containing an environment path or input value.
        pass


def capture_optional_screenshot(
    device: AndroidDevice,
    output_path: Path,
    completed: list[str],
    name: str,
) -> str:
    """Capture credential-free UI evidence without making PNG existence a gate."""

    try:
        device.screenshot(output_path)
    except SmokeFailure as error:
        try:
            output_path.unlink(missing_ok=True)
        except OSError:
            pass
        completed.append(f"{name}_screenshot_unavailable")
        return error.reason
    completed.append(f"{name}_screenshot")
    return "ok"


def start_optional_screenrecord(
    device: AndroidDevice,
    output_path: Path,
) -> tuple[ScreenRecording | None, str]:
    """Start bounded video evidence after all credential UI is dismissed."""

    if getattr(device, "foreground_evidence_lost", False) is True:
        return None, "foreground_lost"
    remote_pid: int | None = None
    try:
        device.assert_foreground("daily_screenrecord_start")
        output_path.parent.mkdir(parents=True, exist_ok=True)
        try:
            output_path.unlink()
        except FileNotFoundError:
            pass
        device.run(
            (
                "shell",
                "rm",
                "-f",
                SCREENRECORD_REMOTE_PATH,
                SCREENRECORD_REMOTE_PID_PATH,
            ),
            "daily_screenrecord_cleanup",
            timeout=10.0,
        )
        script = (
            "screenrecord --time-limit "
            f"{SCREENRECORD_LIMIT_SECONDS} {SCREENRECORD_REMOTE_PATH} "
            "</dev/null >/dev/null 2>&1 & child=$!; "
            f"echo $child > {SCREENRECORD_REMOTE_PID_PATH}; echo $child"
        )
        pid_output = device.run(
            ("shell", "sh", "-c", shlex.quote(script)),
            "daily_screenrecord_start",
            timeout=10.0,
        ).decode("ascii", errors="replace")
        pid_match = re.fullmatch(r"\s*(\d+)\s*", pid_output)
        if pid_match is None:
            pid_output = device.run(
                ("shell", "cat", SCREENRECORD_REMOTE_PID_PATH),
                "daily_screenrecord_start",
                timeout=10.0,
            ).decode("ascii", errors="replace")
            pid_match = re.fullmatch(r"\s*(\d+)\s*", pid_output)
        if pid_match is None:
            raise SmokeFailure("daily_screenrecord_start", "pid_unavailable")
        remote_pid = int(pid_match.group(1))
        if remote_pid <= 0:
            raise SmokeFailure("daily_screenrecord_start", "pid_unavailable")
        device.run(
            ("shell", "kill", "-0", str(remote_pid)),
            "daily_screenrecord_start",
            timeout=10.0,
        )
    except (OSError, SmokeFailure):
        if remote_pid is not None:
            try:
                device.run(
                    ("shell", "kill", "-2", str(remote_pid)),
                    "daily_screenrecord_cleanup",
                    timeout=10.0,
                )
            except SmokeFailure:
                pass
            try:
                device.run(
                    ("shell", "kill", "-9", str(remote_pid)),
                    "daily_screenrecord_cleanup",
                    timeout=10.0,
                )
            except SmokeFailure:
                pass
        try:
            device.run(
                (
                    "shell",
                    "rm",
                    "-f",
                    SCREENRECORD_REMOTE_PATH,
                    SCREENRECORD_REMOTE_PID_PATH,
                ),
                "daily_screenrecord_cleanup",
                timeout=10.0,
            )
        except SmokeFailure:
            pass
        return None, "start_failed"
    try:
        device.run(
            ("shell", "rm", "-f", SCREENRECORD_REMOTE_PID_PATH),
            "daily_screenrecord_cleanup",
            timeout=10.0,
        )
    except SmokeFailure:
        pass
    assert remote_pid is not None
    return ScreenRecording(remote_pid, SCREENRECORD_REMOTE_PATH, output_path), "ok"


def finish_optional_screenrecord(
    device: AndroidDevice,
    recording: ScreenRecording,
) -> str:
    """Stop, pull, validate, and remotely remove optional video evidence."""

    try:
        try:
            device.run(
                ("shell", "kill", "-2", str(recording.remote_pid)),
                "daily_screenrecord_stop",
                timeout=10.0,
            )
        except SmokeFailure:
            # A recording that reached its 180-second bound has already
            # finalized and exited; pulling it remains useful evidence.
            pass
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            try:
                device.run(
                    ("shell", "kill", "-0", str(recording.remote_pid)),
                    "daily_screenrecord_wait",
                    timeout=5.0,
                )
            except SmokeFailure:
                break
            time.sleep(0.2)
        else:
            try:
                device.run(
                    ("shell", "kill", "-9", str(recording.remote_pid)),
                    "daily_screenrecord_cleanup",
                    timeout=10.0,
                )
            except SmokeFailure:
                pass
            return "stop_timeout"
        # Any detected foreground loss invalidates the whole recording, even
        # if meeterm has returned by cleanup time. Do not pull those pixels.
        if getattr(device, "foreground_evidence_lost", False) is True:
            return "foreground_lost"
        try:
            device.assert_foreground("daily_screenrecord_finish")
        except SmokeFailure:
            return "foreground_lost"
        device.run(
            ("pull", recording.remote_path, str(recording.output_path)),
            "daily_screenrecord_pull",
            timeout=30.0,
        )
        video = recording.output_path.read_bytes()
        if len(video) < 12 or video[4:8] != MP4_FILE_TYPE_BOX:
            try:
                recording.output_path.unlink()
            except OSError:
                pass
            return "invalid_mp4"
        return "ok"
    except (OSError, SmokeFailure):
        try:
            recording.output_path.unlink()
        except OSError:
            pass
        return "pull_failed"
    finally:
        try:
            device.run(
                (
                    "shell",
                    "rm",
                    "-f",
                    recording.remote_path,
                    SCREENRECORD_REMOTE_PID_PATH,
                ),
                "daily_screenrecord_cleanup",
                timeout=10.0,
            )
        except SmokeFailure:
            pass


def find_node_containing_text(nodes: list[Node], value: str) -> Node | None:
    """Find a fixed, non-secret text fragment in the accessibility tree."""

    for node in nodes:
        if not node.visible_to_user or not node.enabled:
            continue
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if value in node.text or value in node.content_description:
            return node
    return None


def wait_for_text_fragment(
    device: AndroidDevice,
    stage: str,
    value: str,
    *,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        node = find_node_containing_text(nodes, value)
        if node is not None:
            return node
        time.sleep(0.2)
    raise SmokeFailure(stage, "ui_timeout")


def tap_action(
    device: AndroidDevice,
    stage: str,
    labels: tuple[str, ...],
    *,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    node = wait_for_node_with_labels(device, stage, labels, timeout=timeout)
    tap_node(device, node, stage)
    return node


def dismiss_handoff(device: AndroidDevice, stage: str) -> None:
    """Close the handoff sheet without assuming its presentation primitive."""

    # The sheet owns the top-most Back action after opening and the terminal
    # view remains mounted underneath it. Android's modal back contract is
    # stable even when the visible copy is localized.
    device.input_keyevent(KEYCODE_BACK, stage)


def open_handoff_and_capture(
    device: AndroidDevice,
    artifact_dir: Path,
    completed: list[str],
) -> None:
    tap_action(device, "terminal_menu", TERMINAL_MENU_LABELS)
    tap_action(device, "handoff_action", HANDOFF_LABELS)
    wait_for_text_fragment(device, "handoff_action", HANDOFF_COMMAND)
    capture_optional_screenshot(
        device,
        artifact_dir / "ssh-handoff.png",
        completed,
        "handoff",
    )
    dismiss_handoff(device, "handoff_close")


def open_disconnect_action(device: AndroidDevice, stage: str) -> None:
    """Reach Disconnect in the terminal's server-connection menu."""

    try:
        tap_action(device, stage, TERMINAL_MENU_LABELS, timeout=3.0)
    except SmokeFailure as error:
        if error.reason != "ui_timeout":
            raise
        # Older workspace builds exposed Disconnect directly in the toolbar;
        # keep that compatibility path narrow and label based.
    try:
        tap_action(device, stage, SERVER_CONNECTION_LABELS, timeout=5.0)
    except SmokeFailure as error:
        if error.reason != "ui_timeout":
            raise
    tap_action(device, stage, DISCONNECT_LABELS, timeout=DEFAULT_UI_TIMEOUT)


def reconnect_saved_profile_after_restart(
    device: AndroidDevice,
    artifact_dir: Path,
    completed: list[str],
    previous_pid: str,
) -> str:
    """Prove that profile metadata and native credentials survive process death."""

    stage = "daily_process_restart"
    device.run(("shell", "am", "force-stop", PACKAGE), stage, timeout=10.0)
    device.run(
        ("shell", "am", "start", "-W", "-n", f"{PACKAGE}/.MainActivity"),
        stage,
        timeout=15.0,
    )
    saved_servers = wait_for_node(
        device,
        stage,
        content_description="Saved servers",
        timeout=RECONNECT_TIMEOUT,
    )
    restarted_pid = device.process_id(stage)
    if restarted_pid == previous_pid:
        raise SmokeFailure(stage, "app_process_unchanged")
    completed.append("daily_process_restarted")

    stage = "daily_saved_profile"
    tap_node(device, saved_servers, stage)
    profile = wait_for_node(
        device,
        stage,
        content_description=f"Connect saved server {DAILY_PROFILE_NAME}",
        timeout=RECONNECT_TIMEOUT,
    )
    wait_for_text_fragment(
        device,
        stage,
        "認証情報を保存済み",
        timeout=RECONNECT_TIMEOUT,
    )
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-servers.png",
        completed,
        "daily_servers",
    )
    completed.append("daily_profile_and_credential_restored")

    stage = "daily_saved_profile_connect"
    tap_node(device, profile, stage)
    wait_for_node(device, stage, text="Connected", timeout=RECONNECT_TIMEOUT)
    device.assert_process_alive(stage)
    completed.append("daily_saved_profile_connected")
    return restarted_pid


def exercise_daily_settings(
    device: AndroidDevice,
    artifact_dir: Path,
    completed: list[str],
) -> None:
    """Persist the daily terminal settings and verify their redisplay."""

    stage = "daily_settings_open"
    tap_action(device, stage, ("Terminal settings",))
    wait_for_text_input(device, stage, "Terminal font size")

    stage = "daily_settings_theme"
    tap_action(device, stage, ("Terminal theme",))
    light = wait_for_node(device, stage, text="ライト")
    tap_node(device, light, stage)

    fill_field(
        device,
        "Terminal font size",
        "18",
        "daily_settings_font",
        scroll=False,
        clear_count=2,
    )
    device.dismiss_keyboard("daily_settings_font")
    time.sleep(0.5)
    fill_field(
        device,
        "Scrollback lines",
        "20000",
        "daily_settings_scrollback",
        clear_count=5,
    )
    device.dismiss_keyboard("daily_settings_scrollback")

    stage = "daily_settings_save"
    tap_action(device, stage, ("Save settings",))
    wait_for_node(
        device,
        stage,
        content_description="Terminal settings",
        timeout=RECONNECT_TIMEOUT,
    )
    completed.append("daily_settings_saved")

    stage = "daily_settings_reopen"
    tap_action(device, stage, ("Terminal settings",))
    wait_for_field_value(device, stage, "Terminal font size", "18")
    wait_for_node(device, stage, text="ライト")
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-settings.png",
        completed,
        "daily_settings",
    )
    wait_for_text_input(
        device,
        stage,
        "Scrollback lines",
        scroll=True,
    )
    wait_for_field_value(device, stage, "Scrollback lines", "20000")
    completed.append("daily_settings_redisplayed")
    tap_action(device, "daily_settings_close", ("Cancel",))
    wait_for_node(
        device,
        "daily_settings_close",
        content_description="Terminal settings",
        timeout=RECONNECT_TIMEOUT,
    )


def fixture_workspace_label(fixture_layout: list[TmuxPaneRecord]) -> str:
    active_names = {pane.window_name for pane in fixture_layout if pane.window_active}
    if len(active_names) != 1:
        raise SmokeFailure("daily_fixture_workspace", "workspace_selection_unavailable")
    return f"Workspace {next(iter(active_names))}"


def exercise_glyph_atlas_stress(
    device: AndroidDevice,
    stress_path: Path,
    done_marker_path: Path,
    done_marker_value: str,
    artifact_dir: Path,
    completed: list[str],
) -> None:
    """Cross the glyph atlas boundary and retain the late visible CJK page."""

    stage = "daily_glyph_atlas"
    baseline_events = renderer_atlas_reset_events(device, stage)
    terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
    focus_terminal(device, terminal, stage)
    terminal_line(device, "stty -echo")
    device.dismiss_keyboard(stage)
    time.sleep(0.5)
    # Pace the public fixture rows to expose more than the final snapshot.
    # The new atlas-reset assertion below proves that capacity was crossed;
    # timing alone does not guarantee a renderer frame for every row.
    # The remote marker follows the final END Japanese line.
    command = (
        f"clear; cat {shell_quote(str(stress_path))} | "
        "while IFS= read -r line; do echo \"$line\"; sleep 0.05; done; "
        f"echo {shell_quote(done_marker_value)} > "
        f"{shell_quote(str(done_marker_path))}; stty echo"
    )
    terminal_line(device, command)
    wait_for_file_contents(
        done_marker_path,
        f"{done_marker_value}\n",
        stage,
    )
    wait_for_new_atlas_reset(device, baseline_events, stage)
    time.sleep(0.5)
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-glyph-atlas.png",
        completed,
        "daily_glyph_atlas",
    )
    completed.append("daily_glyph_atlas_reset")


def exercise_daily_workspace_and_selection(
    device: AndroidDevice,
    tmux_socket: Path,
    fixture_layout: list[TmuxPaneRecord],
    glyph_stress_path: Path,
    glyph_done_marker_path: Path,
    glyph_done_marker_value: str,
    copy_marker_path: Path,
    copy_marker_value: str,
    artifact_dir: Path,
    completed: list[str],
) -> None:
    """Exercise light rendering, native selection, and temporary tmux CRUD."""

    original_workspace_label = fixture_workspace_label(fixture_layout)

    stage = "daily_terminal_light"
    original_workspace = wait_for_workspace(
        device,
        stage,
        label=original_workspace_label,
        timeout=RECONNECT_TIMEOUT,
    )
    tap_node(device, original_workspace, stage)
    wait_for_panes(
        device,
        stage,
        count=FIXTURE_PANES_PER_WINDOW,
        selected_count=1,
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
    time.sleep(1.0)
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-terminal-light.png",
        completed,
        "daily_terminal_light",
    )
    exercise_glyph_atlas_stress(
        device,
        glyph_stress_path,
        glyph_done_marker_path,
        glyph_done_marker_value,
        artifact_dir,
        completed,
    )
    tap_action(device, stage, BACK_TO_WORKSPACES_LABELS)
    wait_for_workspace(
        device,
        stage,
        label=original_workspace_label,
        timeout=RECONNECT_TIMEOUT,
    )

    stage = "daily_workspace_create"
    tap_action(device, stage, ("Create workspace",))
    fill_field(
        device,
        "Workspace or terminal name",
        DAILY_WORKSPACE_NAME,
        stage,
        scroll=False,
        clear_count=80,
    )
    tap_action(device, stage, ("Save name",))
    wait_for_workspace_count(
        device,
        stage,
        count=len(FIXTURE_WINDOW_NAMES) + 1,
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    temporary_workspace = wait_for_workspace(
        device,
        stage,
        label=f"Workspace {DAILY_WORKSPACE_NAME}",
        timeout=RECONNECT_TIMEOUT,
    )
    completed.append("daily_workspace_created")

    stage = "daily_workspace_rename"
    options = wait_for_node(
        device,
        stage,
        content_description=f"Workspace options {DAILY_WORKSPACE_NAME}",
        scroll=True,
    )
    tap_node(device, options, stage)
    tap_action(device, stage, ("名前を変更",))
    fill_field(
        device,
        "Workspace or terminal name",
        DAILY_WORKSPACE_RENAMED,
        stage,
        scroll=False,
        clear_count=len(DAILY_WORKSPACE_NAME),
    )
    tap_action(device, stage, ("Save name",))
    temporary_workspace = wait_for_workspace(
        device,
        stage,
        label=f"Workspace {DAILY_WORKSPACE_RENAMED}",
        timeout=RECONNECT_TIMEOUT,
    )
    completed.append("daily_workspace_renamed")

    stage = "daily_workspace_open"
    tap_node(device, temporary_workspace, stage)
    initial_panes = wait_for_panes(
        device,
        stage,
        count=1,
        selected_count=1,
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    initial_pane_ids = {pane_id_from_node(node) for node in initial_panes}

    stage = "daily_pane_create"
    tap_action(device, stage, ("Create terminal",))
    pane_nodes = wait_for_panes(
        device,
        stage,
        count=2,
        selected_count=1,
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    created_panes = [
        node for node in pane_nodes if pane_id_from_node(node) not in initial_pane_ids
    ]
    if len(created_panes) != 1:
        raise SmokeFailure(stage, "pane_identity_unavailable")
    created_pane = created_panes[0]
    created_pane_id = pane_id_from_node(created_pane)
    if created_pane_id is None:
        raise SmokeFailure(stage, "pane_identity_unavailable")
    if not created_pane.selected:
        tap_node(device, created_pane, stage)
        wait_for_pane(
            device,
            stage,
            pane_id=created_pane_id,
            selected=True,
            timeout=RECONNECT_TIMEOUT,
        )
    completed.append("daily_pane_created")

    stage = "daily_pane_rename"
    tap_action(device, stage, TERMINAL_MENU_LABELS)
    rename_pane = wait_for_node(
        device,
        stage,
        content_description="Rename terminal",
        scroll=True,
    )
    tap_node(device, rename_pane, stage)
    fill_field(
        device,
        "Workspace or terminal name",
        DAILY_PANE_NAME,
        stage,
        scroll=False,
        clear_count=80,
    )
    tap_action(device, stage, ("Save name",))
    wait_for_node(device, stage, text=DAILY_PANE_NAME, timeout=RECONNECT_TIMEOUT)
    completed.append("daily_pane_renamed")
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-created-pane.png",
        completed,
        "daily_created_pane",
    )

    stage = "daily_terminal_selection"
    terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
    focus_terminal(device, terminal, stage)
    # Hide the IME before placing the marker. Its resize can reflow terminal
    # history, so clearing first and then hiding would make row zero unstable.
    terminal_line(device, "stty -echo")
    device.dismiss_keyboard(stage)
    time.sleep(0.5)
    terminal_line(
        device,
        f"clear; printf '{DAILY_SELECTION_MARKER}\\n'; stty echo",
    )
    time.sleep(1.0)
    terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
    current_panes = list_tmux_panes(tmux_socket, stage)
    current_pane = next(
        (pane for pane in current_panes if pane.pane_id == created_pane_id),
        None,
    )
    if current_pane is None:
        raise SmokeFailure(stage, "pane_identity_changed")
    selection_start, selection_end = selection_drag_points(
        terminal,
        columns=current_pane.pane_width,
        character_count=len(DAILY_SELECTION_MARKER),
    )
    device.input_long_press_drag(
        selection_start[0],
        selection_start[1],
        selection_end[0],
        selection_end[1],
        stage,
    )
    copy_selection = wait_for_node(
        device,
        stage,
        content_description="Copy selection",
        timeout=RECONNECT_TIMEOUT,
    )
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-selection.png",
        completed,
        "daily_selection",
    )
    tap_node(device, copy_selection, stage)
    time.sleep(0.5)
    capture_optional_screenshot(
        device,
        artifact_dir / "daily-selection-cleared.png",
        completed,
        "daily_selection_cleared",
    )
    terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
    focus_terminal(device, terminal, stage)
    terminal_line(device, "IFS= read -r MEETERM_DAILY_COPIED")
    time.sleep(0.3)
    paste = wait_for_node(
        device,
        stage,
        content_description="Paste",
        timeout=RECONNECT_TIMEOUT,
    )
    tap_node(device, paste, stage)
    time.sleep(0.3)
    device.input_keyevent(KEYCODE_ENTER, "terminal_input")
    time.sleep(0.3)
    verify_copy = (
        f"[ \"$MEETERM_DAILY_COPIED\" = {shell_quote(DAILY_SELECTION_MARKER)} ] "
        f"&& echo {shell_quote(copy_marker_value)} > "
        f"{shell_quote(str(copy_marker_path))}"
    )
    terminal_line(device, verify_copy)
    wait_for_file_contents(
        copy_marker_path,
        f"{copy_marker_value}\n",
        stage,
    )
    completed.append("daily_native_selection_copied")

    stage = "daily_pane_close"
    tap_action(device, stage, TERMINAL_MENU_LABELS)
    close_pane = wait_for_node(
        device,
        stage,
        content_description="Close terminal",
        scroll=True,
    )
    tap_node(device, close_pane, stage)
    wait_for_node(device, stage, text="ターミナルを終了しますか？")
    tap_action(device, stage, ("終了",))
    wait_for_panes(
        device,
        stage,
        count=1,
        selected_count=1,
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    completed.append("daily_pane_closed")

    stage = "daily_workspace_close"
    tap_action(device, stage, BACK_TO_WORKSPACES_LABELS)
    wait_for_workspace(
        device,
        stage,
        label=f"Workspace {DAILY_WORKSPACE_RENAMED}",
        timeout=RECONNECT_TIMEOUT,
    )
    options = wait_for_node(
        device,
        stage,
        content_description=f"Workspace options {DAILY_WORKSPACE_RENAMED}",
        scroll=True,
    )
    tap_node(device, options, stage)
    tap_action(device, stage, ("終了",))
    wait_for_node(device, stage, text="ワークスペースを終了しますか？")
    tap_action(device, stage, ("終了",))
    remaining_workspaces = wait_for_workspace_count(
        device,
        stage,
        count=len(FIXTURE_WINDOW_NAMES),
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    if any(
        accessible_label(node) == f"Workspace {DAILY_WORKSPACE_RENAMED}"
        for node in remaining_workspaces
    ):
        raise SmokeFailure(stage, "workspace_not_closed")
    restored_layout = list_tmux_panes(tmux_socket, stage)
    assert_fixture_identity_preserved(fixture_layout, restored_layout, stage)
    completed.append("daily_workspace_closed")

    stage = "daily_fixture_return"
    original_workspace = wait_for_workspace(
        device,
        stage,
        label=original_workspace_label,
        timeout=RECONNECT_TIMEOUT,
    )
    tap_node(device, original_workspace, stage)
    wait_for_panes(
        device,
        stage,
        count=FIXTURE_PANES_PER_WINDOW,
        selected_count=1,
        exact=True,
        timeout=RECONNECT_TIMEOUT,
    )
    tap_action(device, stage, BACK_TO_WORKSPACES_LABELS)
    returned_layout = list_tmux_panes(tmux_socket, stage)
    assert_fixture_identity_preserved(fixture_layout, returned_layout, stage)
    active_names = {
        pane.window_name for pane in returned_layout if pane.window_active
    }
    if active_names != {original_workspace_label.removeprefix("Workspace ")}:
        raise SmokeFailure(stage, "workspace_selection_mismatch")
    completed.append("daily_fixture_restored")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Run the Android real-SSH UI smoke.")
    parser.add_argument("--artifact-dir", type=Path, default=Path("artifacts/android-ssh"))
    parser.add_argument("--serial", help="adb serial; defaults to ANDROID_SERIAL")
    args = parser.parse_args(argv)

    completed: list[str] = []
    stage = "startup"
    result = "failed"
    reason = "unexpected"
    screenshot_written = False
    screenshot_reason = "not_attempted"
    secrets_submitted = False
    device: AndroidDevice | None = None
    reverse_created = False
    tmux_socket: Path | None = None
    marker_path: Path | None = None
    marker_value: str | None = None
    second_marker_path: Path | None = None
    second_marker_value: str | None = None
    copy_marker_path: Path | None = None
    copy_marker_value: str | None = None
    glyph_stress_path: Path | None = None
    glyph_done_marker_path: Path | None = None
    glyph_done_marker_value: str | None = None
    initial_app_pid: str | None = None
    daily_recording: ScreenRecording | None = None
    screenrecord_reason = "not_attempted"

    try:
        try:
            args.artifact_dir.mkdir(parents=True, exist_ok=True)
        except OSError as error:
            raise SmokeFailure("artifacts", "artifact_write_failed") from error

        host, port, username, key, key_path = load_fixture()
        expected_fingerprint = required_environment("MEETERM_SSH_FINGERPRINT")
        if not re.fullmatch(r"SHA256:[A-Za-z0-9+/]+={0,2}", expected_fingerprint):
            raise SmokeFailure("fixture_environment", "invalid_fingerprint")
        tmux_socket = tmux_socket_from_fixture(key_path)
        fixture_layout = prepare_tmux_fixture(tmux_socket)
        marker_path, marker_value = make_marker_file(key_path)
        second_marker_path, second_marker_value = make_marker_file(key_path)
        copy_marker_path, copy_marker_value = make_marker_file(key_path)
        glyph_stress_path = make_glyph_stress_file(key_path)
        glyph_done_marker_path, glyph_done_marker_value = make_marker_file(key_path)

        stage = "device_select"
        adb_path = shutil.which("adb") or "adb"
        serial = resolve_serial(adb_path, args.serial)
        device = AndroidDevice(serial, adb_path)
        device.wait_for_device()
        completed.append("device_ready")

        stage = "reverse"
        reverse_list = device.run(("reverse", "--list"), stage, timeout=10.0).decode(
            "utf-8", errors="replace"
        )
        if reverse_local_mapping_exists(reverse_list, port):
            raise SmokeFailure(stage, "reverse_already_exists")
        device.run(("reverse", f"tcp:{port}", f"tcp:{port}"), stage, timeout=10.0)
        reverse_created = True
        completed.append("loopback_reverse")

        stage = "launch"
        device.run(("shell", "am", "force-stop", PACKAGE), stage, timeout=10.0)
        device.run(
            ("shell", "am", "start", "-W", "-n", f"{PACKAGE}/.MainActivity"),
            stage,
            timeout=15.0,
        )
        connect_button = wait_for_node(device, stage, text="Connect")
        tap_node(device, connect_button, stage)
        initial_app_pid = device.process_id("launch")
        completed.append("connect_form_open")

        fill_field(device, "Host", host, "host_input", scroll=False)
        completed.append("host_filled")
        fill_field(device, "Port", str(port), "port_input", scroll=False, clear_count=2)
        completed.append("port_filled")
        fill_field(device, "Username", username, "username_input")
        completed.append("username_filled")
        # Finish the short fields with the IME hidden.  The key editor is
        # below the fold; this prevents keyboard inset changes during scroll.
        # One BACK is safe here because the username editor was just tapped
        # and owns the IME.
        time.sleep(0.5)
        device.dismiss_keyboard("username_input")
        time.sleep(0.5)
        # Exercise the authentication selector before entering any secret.
        # These captures therefore contain only the disposable fixture address.
        stage = "password_form"
        password_choice = wait_for_node(
            device, stage, content_description="Password authentication", scroll=True
        )
        tap_node(device, password_choice, stage)
        password_editor = wait_for_text_input(
            device, stage, label="SSH password", scroll=True
        )
        tap_node(device, password_editor, stage)
        time.sleep(0.5)
        capture_optional_screenshot(
            device, args.artifact_dir / "password-form-keyboard.png", completed,
            "password_form_keyboard",
        )
        device.dismiss_keyboard(stage)
        capture_optional_screenshot(
            device, args.artifact_dir / "password-form.png", completed, "password_form"
        )
        key_choice = wait_for_node(
            device, stage, content_description="Private key authentication", scroll=True
        )
        tap_node(device, key_choice, stage)
        completed.append("authentication_selector_verified")

        # Configure persistence while every field is still public. Gestures
        # stay in the form's observed outer padding so the empty multiline key
        # editor cannot consume a ScrollView swipe. After this block the driver
        # returns to the key editor, enters it with exact in-memory readback,
        # and submits without searching or scrolling through credential UI.
        set_toggle(
            device,
            "Save server profile",
            True,
            "daily_profile_save_toggle",
            scroll_gutter=True,
        )
        fill_field(
            device,
            "Server name",
            DAILY_PROFILE_NAME,
            "daily_profile_name",
            scroll_gutter=True,
        )
        device.dismiss_keyboard("daily_profile_name")
        set_toggle(
            device,
            "Save credentials securely",
            True,
            "daily_credential_save_toggle",
            scroll_gutter=True,
        )
        completed.append("daily_profile_persistence_selected")
        fill_multiline_key(device, key, return_from_form_end=True)
        completed.append("form_filled")

        stage = "form_submit"
        submit_button = wait_for_node(device, stage, text="Connect")
        tap_node(device, submit_button, stage)
        # The app clears private key/passphrase state before this callback
        # reaches native code. No screenshot is attempted before this point.
        secrets_submitted = True
        completed.append("form_submitted")

        stage = "host_key_prompt"
        trust_host(device, expected_fingerprint)
        completed.append("host_key_verified")

        stage = "connected"
        wait_for_node(device, stage, text="Connected", timeout=RECONNECT_TIMEOUT)
        completed.append("connected")

        if initial_app_pid is None:
            raise SmokeFailure("daily_process_restart", "app_process_unavailable")
        initial_app_pid = reconnect_saved_profile_after_restart(
            device,
            args.artifact_dir,
            completed,
            initial_app_pid,
        )

        # The private-key editor was cleared before native connect, the app
        # process was replaced, and the saved-profile sheet has closed. Video
        # therefore starts only after no credential UI can be captured.
        daily_recording, screenrecord_reason = start_optional_screenrecord(
            device,
            args.artifact_dir / "daily-use.mp4",
        )
        if daily_recording is None:
            completed.append("daily_video_unavailable")
        else:
            completed.append("daily_video_started")

        exercise_daily_settings(device, args.artifact_dir, completed)
        exercise_daily_workspace_and_selection(
            device,
            tmux_socket,
            fixture_layout,
            glyph_stress_path,
            glyph_done_marker_path,
            glyph_done_marker_value,
            copy_marker_path,
            copy_marker_value,
            args.artifact_dir,
            completed,
        )
        if daily_recording is not None:
            screenrecord_reason = finish_optional_screenrecord(
                device,
                daily_recording,
            )
            daily_recording = None
            completed.append(
                "daily_video" if screenrecord_reason == "ok" else "daily_video_unavailable"
            )

        stage = "tmux_session_state"
        workspace = wait_for_workspace(device, stage, timeout=RECONNECT_TIMEOUT)
        workspace_label = accessible_label(workspace)
        workspace_nodes = wait_for_workspace_count(
            device,
            stage,
            count=len(FIXTURE_WINDOW_NAMES),
            timeout=RECONNECT_TIMEOUT,
        )
        completed.append("tmux_workspace_discovered")
        capture_optional_screenshot(
            device,
            args.artifact_dir / "ssh-workspaces.png",
            completed,
            "workspaces",
        )

        # Workspace-first builds land on a list and enter a terminal only when
        # the row is tapped. Older builds already showed the selected window's
        # pane tabs; retain a bounded compatibility path while the new flow is
        # rolled through hosted runners.
        try:
            pane_nodes = wait_for_panes(
                device,
                stage,
                count=FIXTURE_PANES_PER_WINDOW,
                selected_count=1,
                timeout=5.0,
            )
        except SmokeFailure as error:
            if error.reason != "ui_timeout":
                raise
            tap_node(device, workspace, "workspace_open")
            pane_nodes = wait_for_panes(
                device,
                "workspace_open",
                count=FIXTURE_PANES_PER_WINDOW,
                selected_count=1,
                timeout=RECONNECT_TIMEOUT,
            )
            completed.append("workspace_opened")
        if len(pane_nodes) != FIXTURE_PANES_PER_WINDOW:
            raise SmokeFailure(stage, "pane_count_mismatch")
        selected_nodes = [node for node in pane_nodes if node.selected]
        if len(selected_nodes) != 1:
            raise SmokeFailure(stage, "pane_selection_unavailable")
        fixture_panes = list_tmux_panes(tmux_socket, stage)
        if len(fixture_panes) != FIXTURE_PANE_COUNT or len(
            {pane.window_id for pane in fixture_panes}
        ) != len(FIXTURE_WINDOW_NAMES):
            raise SmokeFailure(stage, "pane_layout_mismatch")
        # tmux keeps one active pane in every window.  Only the pane in the
        # active window is the pane the mobile terminal should expose.
        fixture_active = [
            pane for pane in fixture_panes if pane.active and pane.window_active
        ]
        if len(fixture_active) != 1:
            raise SmokeFailure(stage, "pane_selection_unavailable")
        active_window_id = fixture_active[0].window_id
        expected_visible_panes = records_for_window(fixture_panes, active_window_id)
        visible_ids = {pane_id_from_node(node) for node in pane_nodes}
        if visible_ids != {record.pane_id for record in expected_visible_panes}:
            raise SmokeFailure(stage, "pane_window_mismatch")
        selected_id = pane_id_from_node(selected_nodes[0])
        if selected_id != fixture_active[0].pane_id:
            raise SmokeFailure(stage, "pane_selection_mismatch")

        # Deliberately choose the pane that was not selected on first connect.
        # A one-pane smoke could report success even when selectPane is a no-op;
        # this target must receive the native terminal input below.
        pane = next((node for node in pane_nodes if not node.selected), None)
        if pane is None:
            raise SmokeFailure(stage, "pane_selection_unavailable")
        pane_id = pane_id_from_node(pane)
        if pane_id is None:
            raise SmokeFailure(stage, "pane_identity_unavailable")
        target_fixture_pane = next(
            (record for record in fixture_panes if record.pane_id == pane_id),
            None,
        )
        if target_fixture_pane is None:
            raise SmokeFailure(stage, "pane_identity_mismatch")
        if target_fixture_pane.active:
            raise SmokeFailure(stage, "pane_initially_selected")
        target_pane_pid = target_fixture_pane.pane_pid

        stage = "tmux_pane_select"
        tap_node(device, pane, stage)
        wait_for_pane(
            device,
            stage,
            pane_id=pane_id,
            selected=True,
            timeout=RECONNECT_TIMEOUT,
        )
        wait_for_tmux_selection(
            tmux_socket,
            pane_id,
            target_pane_pid,
            stage,
            timeout=RECONNECT_TIMEOUT,
        )
        completed.append("tmux_pane_selected")

        stage = "terminal_focus"
        terminal = wait_for_terminal(device, stage)
        focus_terminal(device, terminal, stage)
        completed.append("terminal_focused")

        stage = "remote_marker"
        terminal_line(device, "exec /bin/sh -i")
        time.sleep(0.5)
        terminal_line(device, f"stty -echo; printf '{printf_octal(SYNC_MARKER)}\\n'")
        time.sleep(0.7)
        marker_command = session_marker_command(
            marker_value,
            marker_path,
            target_pane_pid,
        )
        terminal_line(device, marker_command)
        wait_for_file_contents(
            marker_path,
            f"{marker_value}:{target_pane_pid}\n",
            stage,
        )
        completed.append("remote_marker_once")

        stage = "remote_output"
        terminal_line(device, "export LC_ALL=C.UTF-8")
        terminal_line(device, "clear")
        output_command = (
            f"printf '\\033[1;31m{printf_octal(ANSI_MARKER)}\\033[0m\\n'; "
            f"printf '{printf_octal('日本語')}\\n'; stty size; ls -d /tmp"
        )
        terminal_line(device, output_command)
        time.sleep(2.0)
        completed.append("remote_ansi_cjk_ls_size")

        stage = "keyboard_screenshot"
        device.assert_foreground(stage)
        capture_optional_screenshot(
            device,
            args.artifact_dir / "ssh-terminal-keyboard.png",
            completed,
            "terminal_keyboard",
        )

        # The terminal is a child route in the workspace-first app. Exercise
        # its explicit back control and return to the same workspace row.
        stage = "workspace_back"
        tap_action(device, stage, BACK_TO_WORKSPACES_LABELS)
        wait_for_workspace(
            device,
            stage,
            label=workspace_label,
            timeout=RECONNECT_TIMEOUT,
        )
        completed.append("back_to_workspaces")

        stage = "workspace_reopen"
        workspace = wait_for_workspace(device, stage, label=workspace_label)
        tap_node(device, workspace, stage)
        pane_nodes = wait_for_panes(
            device,
            stage,
            count=FIXTURE_PANES_PER_WINDOW,
            selected_count=1,
            timeout=RECONNECT_TIMEOUT,
        )
        if len(pane_nodes) != FIXTURE_PANES_PER_WINDOW:
            raise SmokeFailure(stage, "pane_count_mismatch")
        completed.append("workspace_reopened")

        # Switch to the other tmux window through the terminal's workspace
        # picker. This proves the mobile window mapping rather than merely
        # changing a local tab label.
        stage = "tmux_workspace_switch"
        tap_action(device, stage, SWITCH_WORKSPACE_LABELS)
        workspace_nodes = wait_for_workspace_count(
            device,
            stage,
            count=len(FIXTURE_WINDOW_NAMES),
            timeout=RECONNECT_TIMEOUT,
        )
        other_workspace = next(
            (
                node
                for node in workspace_nodes
                if accessible_label(node) != workspace_label
            ),
            None,
        )
        if other_workspace is None:
            raise SmokeFailure(stage, "workspace_switch_unavailable")
        tap_node(device, other_workspace, stage)
        other_panes = wait_for_panes(
            device,
            stage,
            count=FIXTURE_PANES_PER_WINDOW,
            selected_count=1,
            timeout=RECONNECT_TIMEOUT,
        )
        other_fixture_panes = list_tmux_panes(tmux_socket, stage)
        other_active = [
            pane
            for pane in other_fixture_panes
            if pane.active and pane.window_active
        ]
        if len(other_active) != 1 or other_active[0].window_id == active_window_id:
            raise SmokeFailure(stage, "workspace_selection_mismatch")
        if len(other_panes) != FIXTURE_PANES_PER_WINDOW:
            raise SmokeFailure(stage, "pane_count_mismatch")
        other_window_id = other_active[0].window_id
        if {pane_id_from_node(node) for node in other_panes} != {
            record.pane_id
            for record in records_for_window(other_fixture_panes, other_window_id)
        }:
            raise SmokeFailure(stage, "pane_window_mismatch")
        other_selected = next((node for node in other_panes if node.selected), None)
        other_pane = next((node for node in other_panes if not node.selected), None)
        if other_selected is None or other_pane is None:
            raise SmokeFailure(stage, "pane_selection_unavailable")
        other_pane_id = pane_id_from_node(other_pane)
        if other_pane_id is None:
            raise SmokeFailure(stage, "pane_identity_unavailable")
        other_target = next(
            (record for record in other_fixture_panes if record.pane_id == other_pane_id),
            None,
        )
        if other_target is None or other_target.active:
            raise SmokeFailure(stage, "pane_identity_mismatch")
        tap_node(device, other_pane, stage)
        wait_for_pane(
            device,
            stage,
            pane_id=other_pane_id,
            selected=True,
            timeout=RECONNECT_TIMEOUT,
        )
        wait_for_tmux_selection(
            tmux_socket,
            other_pane_id,
            other_target.pane_pid,
            stage,
            timeout=RECONNECT_TIMEOUT,
        )
        other_terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
        # Selecting another workspace mounts a fresh native terminal view;
        # focus_terminal keeps its first key event behind the same bounded
        # input-connection boundary as the initial terminal. The hosted smoke
        # exposed a dropped character here (``export`` became ``expot``).
        focus_terminal(device, other_terminal, stage)
        terminal_line(
            device,
            session_marker_command(
                second_marker_value or "",
                second_marker_path or Path("/invalid"),
                other_target.pane_pid,
            ),
        )
        wait_for_file_contents(
            second_marker_path or Path("/invalid"),
            f"{second_marker_value}:{other_target.pane_pid}\n",
            stage,
        )
        completed.append("tmux_workspace_switched")
        completed.append("tmux_second_pane_selected")
        completed.append("remote_marker_second_window")

        # Return to the process-preservation pane before showing the handoff
        # instructions and ending the mobile connection. Keep each UI
        # boundary separate so a keyboard/modal race is observable.
        stage = "tmux_workspace_return_switch"
        tap_action(device, stage, SWITCH_WORKSPACE_LABELS)
        stage = "tmux_workspace_return_picker"
        first_workspace = wait_for_workspace(
            device,
            stage,
            label=workspace_label,
            timeout=RECONNECT_TIMEOUT,
        )
        stage = "tmux_workspace_return_open"
        tap_node(device, first_workspace, stage)
        stage = "tmux_workspace_return_panes"
        first_panes = wait_for_panes(
            device,
            stage,
            count=FIXTURE_PANES_PER_WINDOW,
            selected_count=1,
            timeout=RECONNECT_TIMEOUT,
        )
        if len(first_panes) != FIXTURE_PANES_PER_WINDOW:
            raise SmokeFailure(stage, "pane_count_mismatch")
        resume_pane = find_pane_node(first_panes, pane_id)
        if resume_pane is None:
            raise SmokeFailure(stage, "pane_identity_changed")
        stage = "tmux_workspace_return_pane"
        tap_node(device, resume_pane, stage)
        wait_for_pane(
            device,
            stage,
            pane_id=pane_id,
            selected=True,
            timeout=RECONNECT_TIMEOUT,
        )
        wait_for_tmux_selection(
            tmux_socket,
            pane_id,
            target_pane_pid,
            stage,
            timeout=RECONNECT_TIMEOUT,
        )
        stage = "tmux_workspace_return_terminal"
        terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
        focus_terminal(device, terminal, stage)
        completed.append("tmux_workspace_returned")

        stage = "pc_handoff"
        open_handoff_and_capture(device, args.artifact_dir, completed)
        completed.append("pc_handoff_layout")

        stage = "disconnect"
        open_disconnect_action(device, stage)
        wait_for_node(device, "disconnected", text="Not connected", timeout=RECONNECT_TIMEOUT)
        disconnected_layout = list_tmux_panes(tmux_socket, stage)
        assert_fixture_layout_preserved(fixture_layout, disconnected_layout, stage)
        completed.append("disconnected")

        stage = "reconnect"
        reconnect_button = wait_for_node_with_labels(
            device,
            stage,
            RECONNECT_LABELS,
            timeout=RECONNECT_TIMEOUT,
        )
        tap_node(device, reconnect_button, stage)
        wait_for_node(device, stage, text="Connected", timeout=RECONNECT_TIMEOUT)
        completed.append("reconnected")

        stage = "tmux_session_resume"
        # Disconnect leaves the current terminal route mounted, while a
        # reconnect initiated from the home/server sheet may still show the
        # workspace list.  Accept either settled route and only tap a row when
        # the accessibility tree actually exposes one.
        resumed_workspace: Node | None = None
        try:
            resumed_workspace = wait_for_workspace(
                device,
                stage,
                label=workspace_label,
                timeout=5.0,
            )
        except SmokeFailure as error:
            if error.reason != "ui_timeout":
                raise
        try:
            resumed_panes = wait_for_panes(
                device,
                stage,
                count=FIXTURE_PANES_PER_WINDOW,
                selected_count=1,
                timeout=5.0,
            )
        except SmokeFailure as error:
            if error.reason != "ui_timeout":
                raise
            if resumed_workspace is None:
                resumed_workspace = wait_for_workspace(
                    device,
                    stage,
                    label=workspace_label,
                    timeout=RECONNECT_TIMEOUT,
                )
            tap_node(device, resumed_workspace, stage)
            resumed_panes = wait_for_panes(
                device,
                stage,
                count=FIXTURE_PANES_PER_WINDOW,
                selected_count=1,
                timeout=RECONNECT_TIMEOUT,
            )
        if len(resumed_panes) != FIXTURE_PANES_PER_WINDOW:
            raise SmokeFailure(stage, "pane_count_mismatch")
        resumed_pane = find_pane_node(resumed_panes, pane_id)
        if resumed_pane is None:
            raise SmokeFailure(stage, "pane_identity_changed")
        if pane_id_from_node(resumed_pane) != pane_id:
            raise SmokeFailure(stage, "pane_identity_changed")
        tap_node(device, resumed_pane, stage)
        wait_for_pane(
            device,
            stage,
            pane_id=pane_id,
            selected=True,
            timeout=RECONNECT_TIMEOUT,
        )
        resumed_layout = list_tmux_panes(tmux_socket, stage)
        # Reconnect can legitimately leave the mobile-selected pane zoomed;
        # compare durable window/pane identities here and defer the desktop
        # split/no-zoom assertion until the explicit disconnect below.
        assert_fixture_identity_preserved(fixture_layout, resumed_layout, stage)
        wait_for_tmux_selection(
            tmux_socket,
            pane_id,
            target_pane_pid,
            stage,
            timeout=RECONNECT_TIMEOUT,
        )
        terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
        focus_terminal(device, terminal, stage)
        completed.append("tmux_pane_resumed")

        stage = "remote_marker_resume"
        resumed_marker = f"{marker_value}-reconnected"
        resume_command = resumed_marker_command(
            marker_value,
            marker_path,
            target_pane_pid,
        )
        terminal_line(device, resume_command)
        wait_for_file_contents(
            marker_path,
            f"{marker_value}:{target_pane_pid}\n"
            f"{resumed_marker}:{target_pane_pid}\n",
            stage,
        )
        completed.append("remote_marker_resumed")

        stage = "process_alive"
        current_app_pid = device.process_id(stage)
        if initial_app_pid is not None and current_app_pid != initial_app_pid:
            raise SmokeFailure(stage, "app_process_changed")
        completed.append("process_alive")

        stage = "screenshot"
        # The native SSH interaction must still belong to the meeterm activity;
        # screenshot collection alone is best effort and must not hide an ANR
        # or a system dialog that covered the terminal.
        device.assert_foreground(stage)
        screenshot_reason = capture_optional_screenshot(
            device,
            args.artifact_dir / "ssh-terminal.png",
            completed,
            "terminal",
        )
        if screenshot_reason == "ok":
            screenshot_written = True

        stage = "disconnect_after_resume"
        open_disconnect_action(device, stage)
        wait_for_node(
            device,
            stage,
            text="Not connected",
            timeout=RECONNECT_TIMEOUT,
        )
        final_layout = list_tmux_panes(tmux_socket, stage)
        assert_fixture_layout_preserved(fixture_layout, final_layout, stage)
        completed.append("disconnected_after_resume")
        result = "passed"
        reason = "ok"
    except SmokeFailure as error:
        stage = error.stage
        reason = error.reason
    except Exception:
        reason = "unexpected"
    finally:
        if marker_path is not None:
            try:
                marker_path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass
        if second_marker_path is not None:
            try:
                second_marker_path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass
        if copy_marker_path is not None:
            try:
                copy_marker_path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass
        if glyph_stress_path is not None:
            try:
                glyph_stress_path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass
        if glyph_done_marker_path is not None:
            try:
                glyph_done_marker_path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass
        if device is not None:
            if daily_recording is not None:
                screenrecord_reason = finish_optional_screenrecord(
                    device,
                    daily_recording,
                )
                daily_recording = None
                completed.append(
                    "daily_video"
                    if screenrecord_reason == "ok"
                    else "daily_video_unavailable"
                )
            # Once the credential form has been submitted, a terminal-only
            # failure can be reviewed safely. Capture the visible state before
            # force-stop; never take this diagnostic while secrets are on
            # screen. The normal terminal screenshot remains the preferred
            # artifact when the smoke reaches it.
            if (
                result != "passed"
                and secrets_submitted
                and "terminal_focused" in completed
            ):
                try:
                    device.assert_foreground("terminal_failure_screenshot")
                except SmokeFailure as error:
                    completed.append("terminal_failure_screenshot_unavailable")
                    screenshot_reason = error.reason
                else:
                    screenshot_reason = capture_optional_screenshot(
                        device,
                        args.artifact_dir / "ssh-terminal-failure.png",
                        completed,
                        "terminal_failure",
                    )
                    if screenshot_reason == "ok":
                        screenshot_written = True
            device.force_stop()
            try:
                log_contents = device.logcat()
            except SmokeFailure:
                log_contents = "<filtered native log unavailable>\n"
            write_artifact(args.artifact_dir / "ssh-logcat.txt", log_contents)
            if reverse_created:
                try:
                    device.run(("reverse", "--remove", f"tcp:{port}"), "cleanup", timeout=10.0)
                except SmokeFailure:
                    pass

        summary_lines = [
            f"result={result}",
            f"stage={stage}",
            f"reason={reason}",
            f"secrets_submitted={'yes' if secrets_submitted else 'no'}",
            f"screenshot={'written' if screenshot_written else 'unavailable'}",
            f"screenshot_reason={screenshot_reason}",
            f"screenrecord_reason={screenrecord_reason}",
            "completed=" + (",".join(completed) if completed else "none"),
        ]
        write_artifact(args.artifact_dir / "ssh-validation.txt", "\n".join(summary_lines) + "\n")

    if result == "passed":
        print("Android real SSH smoke passed.")
        return 0
    print(f"Android real SSH smoke failed at {stage} ({reason}).", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
