#!/usr/bin/env python3
"""Drive the Android SSH form against the disposable OpenSSH fixture.

This is a bounded, UI-only smoke.  It uses the accessibility tree to find
controls and sends the terminal's ASCII commands through Android input.  The
terminal data path stays in the native view; the script only captures a final
PNG for human review and checks one marker file on the host.
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
import xml.etree.ElementTree as ElementTree


PACKAGE = "dev.meeterm.app"
KEYCODE_DEL = 67
KEYCODE_ENTER = 66
KEYCODE_MOVE_END = 123
DEFAULT_UI_TIMEOUT = 30.0
REMOTE_MARKER_TIMEOUT = 15.0
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

SYNC_MARKER = "MEETERM_ANDROID_SYNC_4C71"
ANSI_MARKER = "MEETERM_ANDROID_ANSI_8A26"
REMOTE_MARKER_PREFIX = "meeterm-android-shell-"


class SmokeFailure(RuntimeError):
    """An expected, sanitized smoke failure."""

    def __init__(self, stage: str, reason: str = "failed") -> None:
        super().__init__()
        self.stage = stage
        self.reason = reason


class Node:
    """The small subset of one accessibility node needed for tapping."""

    __slots__ = ("text", "content_description", "class_name", "bounds")

    def __init__(
        self,
        text: str,
        content_description: str,
        class_name: str,
        bounds: tuple[int, int, int, int],
    ) -> None:
        self.text = text
        self.content_description = content_description
        self.class_name = class_name
        self.bounds = bounds

    @property
    def center(self) -> tuple[int, int]:
        left, top, right, bottom = self.bounds
        return ((left + right) // 2, (top + bottom) // 2)


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
            ("shell", "uiautomator", "dump", "/dev/tty"),
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


def find_node(
    nodes: list[Node],
    *,
    text: str | None = None,
    content_description: str | None = None,
    class_fragment: str | None = None,
) -> Node | None:
    for node in nodes:
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if text is not None and node.text != text:
            continue
        if content_description is not None and node.content_description != content_description:
            continue
        if class_fragment is not None and class_fragment not in node.class_name:
            continue
        return node
    return None


def find_node_casefold(nodes: list[Node], text: str) -> Node | None:
    target = text.casefold()
    for node in nodes:
        left, top, right, bottom = node.bounds
        if right <= left or bottom <= top:
            continue
        if node.text.casefold() == target:
            return node
    return None


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


def wait_for_node(
    device: AndroidDevice,
    stage: str,
    *,
    text: str | None = None,
    content_description: str | None = None,
    class_fragment: str | None = None,
    scroll: bool = False,
    timeout: float = DEFAULT_UI_TIMEOUT,
) -> Node:
    deadline = time.monotonic() + timeout
    last_bounds = (0, 0, 1080, 1920)
    while time.monotonic() < deadline:
        try:
            nodes = device.dump_ui()
        except SmokeFailure:
            time.sleep(0.2)
            continue
        last_bounds = screen_bounds(nodes)
        node = find_node(
            nodes,
            text=text,
            content_description=content_description,
            class_fragment=class_fragment,
        )
        if node is not None:
            return node
        if scroll:
            device.input_swipe(last_bounds, stage)
            time.sleep(0.25)
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
        content_description="Private OpenSSH key",
        scroll=True,
    )
    tap_node(device, node, "private_key_input")
    for index, line in enumerate(lines):
        device.input_text(line, "private_key_input")
        if index + 1 < len(lines):
            device.input_keyevent(KEYCODE_ENTER, "private_key_input")


def host_fingerprint_from_nodes(nodes: list[Node]) -> str | None:
    pattern = re.compile(r"SHA256:[A-Za-z0-9+/]+={0,2}")
    for node in nodes:
        match = pattern.search(node.text)
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


def wait_for_marker(path: Path, marker: str) -> None:
    expected = f"{marker}\n"
    deadline = time.monotonic() + REMOTE_MARKER_TIMEOUT
    while time.monotonic() < deadline:
        try:
            content = path.read_text(encoding="utf-8") if path.exists() else ""
        except (OSError, UnicodeError) as error:
            raise SmokeFailure("remote_marker", "marker_read_failed") from error
        if len(content) > len(expected):
            raise SmokeFailure("remote_marker", "marker_repeated")
        if content == expected:
            # Let a second execution arrive before accepting the one-line
            # result; the host check must distinguish duplicate input.
            time.sleep(0.5)
            try:
                stable = path.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as error:
                raise SmokeFailure("remote_marker", "marker_read_failed") from error
            if stable == expected:
                return
            raise SmokeFailure("remote_marker", "marker_repeated")
        time.sleep(0.2)
    raise SmokeFailure("remote_marker", "marker_timeout")


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
            ("shell", "monkey", "-p", PACKAGE, "-c", "android.intent.category.LAUNCHER", "1"),
            stage,
            timeout=15.0,
        )
        connect_button = wait_for_node(device, stage, text="Connect")
        tap_node(device, connect_button, stage)
        completed.append("connect_form_open")

        fill_field(device, "Host", host, "host_input", scroll=False)
        fill_field(device, "Port", str(port), "port_input", scroll=False, clear_count=2)
        fill_field(device, "Username", username, "username_input")
        # Username's next action is wired to privateKeyRef in App.tsx. This
        # also lets the ScrollView bring the multiline key editor into view
        # while the Android keyboard is still active.
        device.input_keyevent(KEYCODE_ENTER, "username_input")
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
        wait_for_node(device, stage, text="Connected", timeout=45.0)
        completed.append("connected")

        stage = "terminal_focus"
        nodes = device.dump_ui()
        terminal = find_node(nodes, class_fragment="MeetermTerminalView")
        if terminal is None:
            terminal = find_node(nodes, class_fragment="GLSurfaceView")
        if terminal is None:
            # Expo may expose only the native child in accessibility. Choose
            # the largest non-text area below the toolbar without guessing a
            # coordinate from a screenshot.
            candidates = [
                node
                for node in nodes
                if node.bounds[1] > 50
                and "TextView" not in node.class_name
                and "EditText" not in node.class_name
            ]
            terminal = max(
                candidates,
                key=lambda node: (node.bounds[2] - node.bounds[0])
                * (node.bounds[3] - node.bounds[1]),
                default=None,
            )
        if terminal is None:
            raise SmokeFailure(stage, "terminal_view_unavailable")
        tap_node(device, terminal, stage)
        completed.append("terminal_focused")

        stage = "remote_marker"
        terminal_line(device, "exec /bin/sh -i")
        time.sleep(0.5)
        terminal_line(
            device,
            f"stty -echo; printf '{printf_octal(SYNC_MARKER)}\\n'",
        )
        time.sleep(0.7)
        marker_command = f"printf '{marker_value}\\n' >> {shell_quote(str(marker_path))}"
        terminal_line(device, marker_command)
        wait_for_marker(marker_path, marker_value)
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

        stage = "process_alive"
        device.assert_process_alive(stage)
        completed.append("process_alive")

        stage = "screenshot"
        output_path = args.artifact_dir / "ssh-terminal.png"
        try:
            device.assert_foreground(stage)
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
