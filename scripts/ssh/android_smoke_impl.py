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
DEFAULT_UI_TIMEOUT = 30.0
REMOTE_MARKER_TIMEOUT = 15.0
RECONNECT_TIMEOUT = 45.0
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

SYNC_MARKER = "MEETERM_ANDROID_SYNC_4C71"
ANSI_MARKER = "MEETERM_ANDROID_ANSI_8A26"
REMOTE_MARKER_PREFIX = "meeterm-android-shell-"
PANE_LABEL_PATTERN = re.compile(r"^Terminal (%[0-9]+)$")
WORKSPACE_LABEL_PATTERN = re.compile(r"^Workspace .+$")
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
        "class_name",
        "bounds",
        "scrollable",
        "enabled",
        "visible_to_user",
        "selected",
    )

    def __init__(
        self,
        text: str,
        content_description: str,
        class_name: str,
        bounds: tuple[int, int, int, int],
        *,
        scrollable: bool = False,
        enabled: bool = True,
        visible_to_user: bool = True,
        selected: bool = False,
    ) -> None:
        self.text = text
        self.content_description = content_description
        self.class_name = class_name
        self.bounds = bounds
        self.scrollable = scrollable
        self.enabled = enabled
        self.visible_to_user = visible_to_user
        self.selected = selected

    @property
    def center(self) -> tuple[int, int]:
        left, top, right, bottom = self.bounds
        return ((left + right) // 2, (top + bottom) // 2)


class TmuxPaneRecord(NamedTuple):
    """The fixture-side identity and selection state of one tmux pane."""

    window_id: str
    pane_id: str
    pane_pid: int
    active: bool
    window_active: bool
    zoomed: bool


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
                class_name=element.attrib.get("class", ""),
                bounds=bounds,
                scrollable=element.attrib.get("scrollable", "false") == "true",
                enabled=element.attrib.get("enabled", "true") == "true",
                visible_to_user=element.attrib.get("visible-to-user", "true")
                == "true",
                selected=element.attrib.get("selected", "false") == "true",
            )
        )
    return nodes


class AndroidDevice:
    def __init__(self, serial: str, adb_path: str) -> None:
        self.serial = serial
        self.adb_path = adb_path

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
        output = self.run(
            ("shell", "pidof", PACKAGE),
            stage,
            timeout=10.0,
        ).decode("utf-8", errors="replace")
        if not re.fullmatch(r"\s*\d+(?:\s+\d+)*\s*", output):
            raise SmokeFailure(stage, "app_not_running")

    def dump_ui(self) -> list[Node]:
        output = self.run(
            ("shell", "-tt", "uiautomator", "dump", "/dev/tty"),
            "uiautomator",
            timeout=10.0,
        )
        return parse_ui_dump(output)

    def assert_foreground(self, stage: str) -> None:
        output = self.run(
            ("shell", "dumpsys", "window", "windows"),
            f"{stage}_foreground",
            timeout=10.0,
        ).decode("utf-8", errors="replace")
        if f"Application Not Responding: {PACKAGE}" in output:
            raise SmokeFailure(stage, "app_anr_window")
        current_focus_lines = [
            line for line in output.splitlines() if "mCurrentFocus" in line
        ]
        if current_focus_lines:
            if any(f"{PACKAGE}/" in line for line in current_focus_lines):
                return
            raise SmokeFailure(stage, "app_not_foreground")

        # Some Android versions omit mCurrentFocus while a window is settling;
        # mFocusedApp is a safe fallback only in that absence, never when it
        # conflicts with a present current-focus window.
        if any(
            "mFocusedApp" in line and f"{PACKAGE}/" in line
            for line in output.splitlines()
        ):
            return
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

    def input_swipe(self, bounds: tuple[int, int, int, int], stage: str) -> None:
        self.assert_foreground(stage)
        left, top, right, bottom = bounds
        center_x = (left + right) // 2
        start_y = top + (bottom - top) * 4 // 5
        end_y = top + (bottom - top) // 5
        self.run(
            (
                "shell",
                "input",
                "swipe",
                str(center_x),
                str(start_y),
                str(center_x),
                str(end_y),
                "350",
            ),
            stage,
            timeout=10.0,
        )

    def screenshot(self, output_path: Path) -> None:
        image = self.run(("exec-out", "screencap", "-p"), "screenshot", timeout=20.0)
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
        for line in output.splitlines():
            if not any(tag in line for tag in ("MeetermTerminalView", "MeetermRenderer", "MeetermNative")):
                continue
            lowered = line.lower()
            if any(secret_word in lowered for secret_word in ("passphrase", "private key", "auth")):
                continue
            kept.append(line)
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
    """Parse only the stable tmux IDs/PIDs/selection fields we need."""

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
        if len(fields) != 6:
            raise SmokeFailure(stage, "tmux_state_invalid")
        window_id, pane_id, pid_text, active_text, window_active_text, zoomed_text = fields
        if not re.fullmatch(r"@[0-9]+", window_id):
            raise SmokeFailure(stage, "tmux_state_invalid")
        if not re.fullmatch(r"%[0-9]+", pane_id) or pane_id in seen_panes:
            raise SmokeFailure(stage, "tmux_state_invalid")
        if not re.fullmatch(r"[1-9][0-9]*", pid_text):
            raise SmokeFailure(stage, "tmux_state_invalid")
        seen_panes.add(pane_id)
        records.append(
            TmuxPaneRecord(
                window_id=window_id,
                pane_id=pane_id,
                pane_pid=int(pid_text),
                active=_parse_tmux_flag(active_text, stage),
                window_active=_parse_tmux_flag(window_active_text, stage),
                zoomed=_parse_tmux_flag(zoomed_text, stage),
            )
        )
    if not records:
        raise SmokeFailure(stage, "tmux_panes_unavailable")
    return records


def list_tmux_panes(socket_path: Path, stage: str) -> list[TmuxPaneRecord]:
    format_string = (
        "#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_active}\t"
        "#{window_active}\t#{window_zoomed_flag}"
    )
    result = run_tmux_command(
        socket_path,
        ("list-panes", "-s", "-t", "=meeterm", "-F", format_string),
        stage,
    )
    return parse_tmux_panes(result.stdout, stage)


def _selection_matches(
    records: list[TmuxPaneRecord],
    pane_id: str,
    pane_pid: int,
) -> bool:
    if len(records) != 2:
        return False
    target = next((record for record in records if record.pane_id == pane_id), None)
    if target is None or target.pane_pid != pane_pid:
        return False
    active = [record for record in records if record.active]
    active_windows = [record for record in records if record.window_active]
    return (
        len({record.window_id for record in records}) == 1
        and target.active
        and target.window_active
        and target.zoomed
        and len(active) == 1
        and len(active_windows) == 2
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
    """Create exactly two panes on the fixture socket and select the first."""

    stage = "tmux_fixture"
    try:
        socket_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        socket_path.parent.chmod(0o700)
    except OSError as error:
        raise SmokeFailure(stage, "socket_directory_unavailable") from error
    if socket_path.exists() or socket_path.is_symlink():
        raise SmokeFailure(stage, "socket_path_in_use")

    existing = run_tmux_command(
        socket_path,
        ("list-sessions", "-F", "#{session_name}"),
        stage,
        allow_failure=True,
    )
    if existing.returncode == 0:
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
    run_tmux_command(socket_path, ("select-pane", "-t", first.pane_id), stage)
    selected = list_tmux_panes(socket_path, stage)
    if len(selected) != 2 or not any(
        record.pane_id == first.pane_id
        and record.active
        and record.window_active
        and not record.zoomed
        for record in selected
    ):
        raise SmokeFailure(stage, "initial_selection_invalid")
    if sum(record.active for record in selected) != 1:
        raise SmokeFailure(stage, "initial_selection_invalid")
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


def accessible_label(node: Node) -> str:
    """Return the stable user-facing label exposed by a UI node."""

    return node.content_description or node.text


def pane_id_from_node(node: Node) -> str | None:
    """Extract a tmux pane ID from the required accessibility label."""

    match = PANE_LABEL_PATTERN.fullmatch(accessible_label(node).strip())
    return match.group(1) if match is not None else None


def is_workspace_label(node: Node) -> bool:
    return WORKSPACE_LABEL_PATTERN.fullmatch(accessible_label(node).strip()) is not None


def find_workspace_node(nodes: list[Node]) -> Node | None:
    for node in nodes:
        if not node.visible_to_user or not node.enabled or not is_workspace_label(node):
            continue
        left, top, right, bottom = node.bounds
        if right > left and bottom > top:
            return node
    return None


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
        workspace = find_workspace_node(nodes)
        if workspace is not None and (label is None or accessible_label(workspace) == label):
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
        if len(panes) >= count and (
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


def wait_for_node(
    device: AndroidDevice,
    stage: str,
    *,
    text: str | None = None,
    content_description: str | None = None,
    content_descriptions: tuple[str, ...] | None = None,
    class_fragment: str | None = None,
    scroll: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    deadline = time.monotonic() + timeout
    last_swipe_at = 0.0
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
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
                device.input_swipe(bounds, stage)
                last_swipe_at = now
                time.sleep(0.5)
            else:
                time.sleep(0.2)
        else:
            time.sleep(0.2)
    raise SmokeFailure(stage, "ui_timeout")


def tap_node(device: AndroidDevice, node: Node, stage: str) -> None:
    left, top, right, bottom = node.bounds
    if right <= left or bottom <= top:
        raise SmokeFailure(stage, "invalid_bounds")
    x, y = node.center
    device.input_tap(x, y, stage)


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
    clear_count: int = 0,
) -> None:
    node = wait_for_node(
        device,
        stage,
        content_description=content_description,
        scroll=scroll,
    )
    tap_node(device, node, stage)
    clear_field(device, stage, clear_count)
    if value:
        device.input_text(value, stage)


def fill_multiline_key(device: AndroidDevice, key: str) -> None:
    lines = key.splitlines()
    if not lines:
        raise SmokeFailure("private_key_input", "key_empty")
    node = wait_for_node(
        device,
        "private_key_input",
        # React Native's Android bridge appends accessibilityValue.text to
        # contentDescription (for example, "Private OpenSSH key, Empty").
        # Keep the accepted states explicit so this does not become a broad
        # prefix match over unrelated form controls.
        content_descriptions=PRIVATE_KEY_ACCESSIBILITY_LABELS,
        scroll=True,
    )
    tap_node(device, node, "private_key_input")
    for index, line in enumerate(lines):
        device.input_text(line, "private_key_input")
        if index + 1 < len(lines):
            device.input_keyevent(KEYCODE_ENTER, "private_key_input")
        # Let React Native's controlled TextInput commit each line before the
        # next adb input event; a busy emulator can otherwise coalesce adjacent
        # multiline updates even when adb reports success.
        time.sleep(0.05)


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
    while time.monotonic() < title_deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        title = find_node(nodes, text="Trust this SSH host?")
        if title is None:
            time.sleep(0.2)
            continue
        actual_fingerprint = host_fingerprint_from_nodes(nodes)
        if actual_fingerprint is None:
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
        raise SmokeFailure("host_key_prompt", "fingerprint_unavailable")
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
    device.input_text(command, "terminal_input")
    device.input_keyevent(KEYCODE_ENTER, "terminal_input")


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
        prepare_tmux_fixture(tmux_socket)
        marker_path, marker_value = make_marker_file(key_path)

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
        completed.append("connect_form_open")

        fill_field(device, "Host", host, "host_input", scroll=False)
        fill_field(device, "Port", str(port), "port_input", scroll=False, clear_count=2)
        fill_field(device, "Username", username, "username_input")
        # Finish the short fields with the IME hidden.  The key editor is
        # below the fold; this prevents keyboard inset changes during scroll.
        # One BACK is safe here because the username editor was just tapped
        # and owns the IME.
        time.sleep(0.5)
        device.dismiss_keyboard("username_input")
        time.sleep(0.5)
        fill_multiline_key(device, key)
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

        stage = "tmux_session_state"
        workspace = wait_for_workspace(device, stage, timeout=RECONNECT_TIMEOUT)
        workspace_label = accessible_label(workspace)
        completed.append("tmux_workspace_discovered")

        pane_nodes = wait_for_panes(
            device,
            stage,
            count=2,
            selected_count=1,
            timeout=RECONNECT_TIMEOUT,
        )
        if len(pane_nodes) != 2:
            raise SmokeFailure(stage, "pane_count_mismatch")
        selected_nodes = [node for node in pane_nodes if node.selected]
        if len(selected_nodes) != 1:
            raise SmokeFailure(stage, "pane_selection_unavailable")
        fixture_panes = list_tmux_panes(tmux_socket, stage)
        if len(fixture_panes) != 2 or len({pane.window_id for pane in fixture_panes}) != 1:
            raise SmokeFailure(stage, "pane_layout_mismatch")
        fixture_active = [pane for pane in fixture_panes if pane.active]
        if len(fixture_active) != 1:
            raise SmokeFailure(stage, "pane_selection_unavailable")
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
        tap_node(device, terminal, stage)
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

        stage = "disconnect"
        disconnect_button = wait_for_node(device, stage, text="Disconnect")
        tap_node(device, disconnect_button, stage)
        wait_for_node(
            device,
            "disconnected",
            text="Not connected",
            timeout=RECONNECT_TIMEOUT,
        )
        completed.append("disconnected")

        stage = "reconnect"
        reconnect_button = wait_for_node(
            device,
            stage,
            text="Reconnect",
            timeout=RECONNECT_TIMEOUT,
        )
        tap_node(device, reconnect_button, stage)
        wait_for_node(device, stage, text="Connected", timeout=RECONNECT_TIMEOUT)
        completed.append("reconnected")

        stage = "tmux_session_resume"
        wait_for_workspace(
            device,
            stage,
            label=workspace_label,
            timeout=RECONNECT_TIMEOUT,
        )
        resumed_panes = wait_for_panes(
            device,
            stage,
            count=2,
            selected_count=1,
            timeout=RECONNECT_TIMEOUT,
        )
        if len(resumed_panes) != 2:
            raise SmokeFailure(stage, "pane_count_mismatch")
        resumed_pane = find_pane_node(resumed_panes, pane_id, selected=True)
        if resumed_pane is None:
            raise SmokeFailure(stage, "pane_identity_changed")
        if pane_id_from_node(resumed_pane) != pane_id:
            raise SmokeFailure(stage, "pane_identity_changed")
        tap_node(device, resumed_pane, stage)
        wait_for_tmux_selection(
            tmux_socket,
            pane_id,
            target_pane_pid,
            stage,
            timeout=RECONNECT_TIMEOUT,
        )
        terminal = wait_for_terminal(device, stage, timeout=RECONNECT_TIMEOUT)
        tap_node(device, terminal, stage)
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
        device.assert_process_alive(stage)
        completed.append("process_alive")

        stage = "screenshot"
        output_path = args.artifact_dir / "ssh-terminal.png"
        # The native SSH interaction must still belong to the meeterm activity;
        # screenshot collection alone is best effort and must not hide an ANR
        # or a system dialog that covered the terminal.
        device.assert_foreground(stage)
        try:
            device.screenshot(output_path)
        except SmokeFailure as error:
            # A screenshot is observability evidence and must never turn a
            # successful real-SSH interaction into a failed machine gate.
            screenshot_reason = error.reason
            completed.append("screenshot_unavailable")
        else:
            screenshot_written = True
            screenshot_reason = "ok"
            completed.append("screenshot")
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
        if device is not None:
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
