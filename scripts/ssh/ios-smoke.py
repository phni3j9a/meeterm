#!/usr/bin/env python3
"""Run a selected iOS Simulator UI smoke suite.

The optional SSH suites use a fixture that owns the host, keys, trust store,
and ordinary tmux server. This driver prepares its disposable topology,
invokes the generated XCUITest, and writes sanitized evidence. XCTest's raw
log and xcresult remain under RUNNER_TEMP.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import pty
import plistlib
import re
import select
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time


IOS_SUITES = ("standard", "ssh", "full", "forms", "native", "names")
SUITE_TIMEOUT_SECONDS = {
    "standard": 900.0,
    "ssh": 900.0,
    "full": 1800.0,
    "forms": 900.0,
    "native": 600.0,
    "names": 900.0,
}
STANDARD_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testStandardSeededScreensAndFoundation"
)
SSH_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testShortSshInputAndDisconnect"
)
FULL_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testRealSshWorkspacePaneInputDisconnectReconnectAndHandoff"
)
FORMS_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testConnectionFormControlsWithoutSecrets"
)
NATIVE_TEST_SELECTOR = "-only-testing:meetermTests/TerminalInputViewTests"
NAMES_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testRealSshNameOperations"
)
STORAGE_TEST_SELECTOR = "-only-testing:meetermStorageTests"
STORAGE_CASES = (
    "interrupted_write_cleanup",
    "credential_endpoint_binding",
    "remove_saved_credential",
    "preferences_validation",
)
NATIVE_INPUT_CASES = (
    "multiline",
    "rebind",
    "unmount",
    "control_one_shot",
    "hardware_control",
    "hardware_shift_combinations",
    "marked_commit",
)
RUNTIME_ENVIRONMENT_NAMES = (
    "MEETERM_SSH_HOST",
    "MEETERM_SSH_PORT",
    "MEETERM_SSH_USERNAME",
    "MEETERM_SSH_FINGERPRINT",
    "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE",
    "MEETERM_SSH_PRIVATE_KEY_FILE",
    "MEETERM_SSH_PASSPHRASE",
    "MEETERM_SSH_KNOWN_HOSTS_FILE",
    "MEETERM_SSH_HOST_KEY_FILE",
    "MEETERM_SSH_ALTERNATE_HOST_KEY_FILE",
    "MEETERM_IOS_ARTIFACT_DIR",
    "MEETERM_IOS_STAGE_PATH",
    "MEETERM_IOS_MARKER_PATH",
    "MEETERM_IOS_MARKER_VALUE",
    "MEETERM_IOS_HANDOFF_VALUE",
)
COMMON_TEST_ENVIRONMENT_NAMES = (
    "MEETERM_IOS_ARTIFACT_DIR",
    "MEETERM_IOS_STAGE_PATH",
    "MEETERM_IOS_MARKER_PATH",
)
FULL_TEST_ENVIRONMENT_NAMES = (
    "MEETERM_SSH_HOST",
    "MEETERM_SSH_PORT",
    "MEETERM_SSH_USERNAME",
    "MEETERM_SSH_FINGERPRINT",
    "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE",
    "MEETERM_IOS_ARTIFACT_DIR",
    "MEETERM_IOS_STAGE_PATH",
    "MEETERM_IOS_MARKER_PATH",
    "MEETERM_IOS_MARKER_VALUE",
    "MEETERM_IOS_HANDOFF_VALUE",
)
NAMES_TEST_ENVIRONMENT_NAMES = (
    "MEETERM_SSH_HOST",
    "MEETERM_SSH_PORT",
    "MEETERM_SSH_USERNAME",
    "MEETERM_SSH_FINGERPRINT",
    "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE",
    "MEETERM_IOS_ARTIFACT_DIR",
    "MEETERM_IOS_STAGE_PATH",
    "MEETERM_IOS_MARKER_PATH",
)
SSH_TEST_ENVIRONMENT_NAMES = (
    *NAMES_TEST_ENVIRONMENT_NAMES,
    "MEETERM_IOS_MARKER_VALUE",
)

CONNECTION_FAILURE_DIAGNOSTICS_NAME = "ios-ui-connection-diagnostics.txt"
INPUT_DIAGNOSTICS_NAME = "ios-ssh-input-diagnostics.json"
SSH_PROBE_NONCE = "meeterm-ios-ssh-probe-v1"
FIXTURE_DIAGNOSTIC_ENVIRONMENT_NAMES = (
    "MEETERM_SSH_HOST",
    "MEETERM_SSH_PORT",
    "MEETERM_SSH_USERNAME",
    "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE",
    "MEETERM_SSH_HOST_KEY_FILE",
)


class SmokeFailure(RuntimeError):
    def __init__(self, stage: str, reason: str) -> None:
        super().__init__(reason)
        self.stage = stage
        self.reason = reason


def required(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise SmokeFailure("fixture_environment", "missing_required_value")
    return value


def fixture_socket() -> Path:
    value = Path(required("MEETERM_TMUX_SOCKET"))
    if (
        value.name != "default"
        or value.parent.name != f"tmux-{os.getuid()}"
        or value.parent.parent.name != "tmux"
        or not value.parent.parent.parent.name.startswith("meeterm-ssh-fixture-")
    ):
        raise SmokeFailure("fixture_environment", "socket_path_invalid")
    return value


def sanitized_environment(socket_path: Path) -> dict[str, str]:
    environment = dict(os.environ)
    environment.pop("TMUX", None)
    environment.pop("TMUX_PANE", None)
    environment["TMUX_TMPDIR"] = str(socket_path.parent.parent)
    environment["TERM"] = "xterm-256color"
    return environment


def run_tmux(
    socket_path: Path,
    arguments: tuple[str, ...],
    stage: str,
    *,
    allow_failure: bool = False,
) -> subprocess.CompletedProcess[str]:
    tmux = shutil.which("tmux")
    if tmux is None:
        raise SmokeFailure(stage, "tmux_unavailable")
    command = [tmux, "-f", "/dev/null", "-S", str(socket_path), *arguments]
    try:
        result = subprocess.run(
            command,
            env=sanitized_environment(socket_path),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SmokeFailure(stage, "tmux_command_failed") from error
    if result.returncode != 0 and not allow_failure:
        raise SmokeFailure(stage, "tmux_command_failed")
    return result


def prepare_topology(socket_path: Path) -> tuple[int, int]:
    existing = run_tmux(
        socket_path,
        ("list-sessions", "-F", "#{session_name}"),
        "tmux_fixture",
        allow_failure=True,
    )
    if existing.returncode == 0 and existing.stdout.strip():
        raise SmokeFailure("tmux_fixture", "session_already_exists")

    run_tmux(
        socket_path,
        ("new-session", "-d", "-s", "meeterm", "-n", "ios-main", "/bin/sh", "-i"),
        "tmux_fixture",
    )
    first_pane_output = run_tmux(
        socket_path,
        ("list-panes", "-t", "=meeterm:ios-main", "-F", "#{pane_id}"),
        "tmux_fixture",
    )
    first_pane = first_pane_output.stdout.strip()
    if not first_pane.startswith("%"):
        raise SmokeFailure("tmux_fixture", "initial_pane_unavailable")

    run_tmux(
        socket_path,
        ("split-window", "-h", "-t", first_pane, "/bin/sh", "-i"),
        "tmux_fixture",
    )
    run_tmux(
        socket_path,
        ("new-window", "-t", "=meeterm", "-n", "ios-side", "/bin/sh", "-i"),
        "tmux_fixture",
    )
    run_tmux(
        socket_path,
        ("select-window", "-t", "=meeterm:ios-main"),
        "tmux_fixture",
    )
    run_tmux(
        socket_path,
        ("select-pane", "-t", first_pane),
        "tmux_fixture",
    )
    workspaces = run_tmux(
        socket_path,
        ("list-windows", "-t", "=meeterm", "-F", "#{window_name}"),
        "tmux_fixture",
    ).stdout.splitlines()
    panes = run_tmux(
        socket_path,
        ("list-panes", "-t", "=meeterm", "-a", "-F", "#{pane_id}"),
        "tmux_fixture",
    ).stdout.splitlines()
    if set(workspaces) != {"ios-main", "ios-side"} or len(panes) != 3:
        raise SmokeFailure("tmux_fixture", "topology_invalid")
    return len(workspaces), len(panes)


def write_short_ssh_input_diagnostics(
    artifact_dir: Path, socket_path: Path, marker_path: Path
) -> None:
    """Observe only the disposable, post-auth SSH fixture; never publish its text.

    The keyboard prefix, pasted suffix and Return have separate native paths.
    Echo evidence helps locate a missing command without sending more input or
    turning a failed marker assertion into a pass. Unexpected clipboard/terminal
    contents may contain credentials, so the artifact contains only fixed keys,
    booleans and counts, including on a successful run for comparison.
    """
    stages = (artifact_dir / "ios-ui-stages.txt").read_text().splitlines()
    if not {"ssh_connected", "ssh_native_input_await_remote_marker"}.issubset(stages):
        return
    if socket_path != fixture_socket():
        raise SmokeFailure("input_diagnostics", "socket_path_invalid")
    marker = required("MEETERM_IOS_MARKER_VALUE")
    if not re.fullmatch(r"ios-ssh-input-[0-9a-f]{16}", marker):
        raise SmokeFailure("input_diagnostics", "marker_invalid")
    quoted_path = "'" + str(marker_path).replace("'", "'\\''") + "'"
    suffix = f" '%s\\n' '{marker}' > {quoted_path}"
    command = "printf" + suffix
    pane_ids = run_tmux(
        socket_path, ("list-panes", "-s", "-t", "=meeterm", "-F", "#{pane_id}"),
        "input_diagnostics",
    ).stdout.splitlines()
    if len(pane_ids) != 3 or any(not re.fullmatch(r"%[0-9]+", pane) for pane in pane_ids):
        raise SmokeFailure("input_diagnostics", "topology_invalid")
    evidence = []
    for pane in pane_ids:
        capture = run_tmux(
            socket_path, ("capture-pane", "-p", "-J", "-S", "-30", "-t", pane),
            "input_diagnostics",
        ).stdout
        before_suffix = capture.split(suffix, 1)[0] if suffix in capture else ""
        prefix_length = max(
            (length for length in range(1, 7) if before_suffix.endswith("printf"[-length:])),
            default=0,
        )
        evidence.append({
            "capture_char_count": len(capture),
            "capture_line_count": len(capture.splitlines()),
            "command_echo_seen": command in capture,
            "keyboard_word_seen": "printf" in capture,
            "paste_body_seen": suffix in capture,
            "keyboard_prefix_suffix_length": prefix_length,
            "marker_echo_seen": marker in capture,
            "marker_path_echo_seen": str(marker_path) in capture,
            "shell_command_not_found": "not found" in capture,
            "shell_permission_denied": "Permission denied" in capture,
            "shell_syntax_error": "syntax error" in capture.lower(),
        })
    marker_exists = marker_path.is_file()
    marker_matches = marker_exists and marker_path.read_text() == marker + "\n"
    write_text(artifact_dir / INPUT_DIAGNOSTICS_NAME, json.dumps({
        "marker_file_exists": marker_exists,
        "marker_file_matches": marker_matches,
        "panes": evidence,
    }, indent=2) + "\n")


def ordinary_desktop_attach(socket_path: Path) -> None:
    """Attach through the default tmux command and detach with Ctrl-b d."""

    tmux = shutil.which("tmux")
    if tmux is None:
        raise SmokeFailure("desktop_handoff", "tmux_unavailable")
    environment = sanitized_environment(socket_path)
    master, slave = pty.openpty()
    process: subprocess.Popen[bytes] | None = None
    try:
        os.set_blocking(master, False)
        process = subprocess.Popen(
            [tmux, "-f", "/dev/null", "attach-session", "-t", "meeterm"],
            env=environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            close_fds=True,
        )
        os.close(slave)
        slave = -1
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            status = process.poll()
            if status is not None:
                raise SmokeFailure("desktop_handoff", "attach_exited_early")
            try:
                readable, _, _ = select.select([master], [], [], 0.1)
            except (OSError, ValueError) as error:
                raise SmokeFailure("desktop_handoff", "attach_read_failed") from error
            if readable:
                try:
                    os.read(master, 8192)
                except (BlockingIOError, OSError):
                    pass
        os.write(master, b"\x02d")
        try:
            status = process.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=5)
            raise SmokeFailure("desktop_handoff", "detach_timeout") from error
        if status != 0:
            raise SmokeFailure("desktop_handoff", "detach_failed")
    except OSError as error:
        raise SmokeFailure("desktop_handoff", "attach_failed") from error
    finally:
        if slave >= 0:
            os.close(slave)
        os.close(master)


def write_text(path: Path, contents: str) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
    except OSError:
        # Keep artifact handling best-effort and avoid replacing a meaningful
        # XCUITest failure with a traceback containing environment paths.
        pass


def _fixture_diagnostics_available(suite: str) -> bool:
    return suite in ("full", "ssh", "names") and all(
        os.environ.get(name) for name in FIXTURE_DIAGNOSTIC_ENVIRONMENT_NAMES
    )


def _simulator_data_container(
    simulator_udid: str,
    bundle_id: str,
) -> tuple[str, Path | None]:
    """Resolve the installed app's data container without exposing its path."""

    xcrun = shutil.which("xcrun")
    if xcrun is None:
        return "unavailable", None
    try:
        completed = subprocess.run(
            [xcrun, "simctl", "get_app_container", simulator_udid, bundle_id, "data"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return "timeout", None
    except OSError:
        return "unavailable", None
    if completed.returncode != 0 or not isinstance(completed.stdout, str):
        return "failed", None
    candidate = completed.stdout.strip().splitlines()
    if not candidate:
        return "failed", None
    value = candidate[-1].strip()
    path = Path(value)
    if not path.is_absolute():
        return "failed", None
    return "passed", path


def _profile_metadata_lines(
    container_status: str,
    container: Path | None,
    *,
    suite: str,
) -> list[str]:
    metadata_file = "unavailable"
    metadata_json = "unavailable"
    profile_count = "unavailable"
    count_match = 0
    field_matches = {
        "host": 0,
        "port": 0,
        "username": 0,
        "name": 0,
        "auth_method": 0,
        "credential_saved": 0,
    }
    profile_match = 0

    if container_status == "passed" and container is not None:
        path = container / "Library" / "Application Support" / "meeterm" / "client-v1.json"
        try:
            if not path.is_file():
                metadata_file = "missing"
            elif path.stat().st_size > 16 * 1024 * 1024:
                metadata_file = "present"
                metadata_json = "invalid"
            else:
                metadata_file = "present"
                document = json.loads(path.read_text(encoding="utf-8"))
                profiles = document.get("profiles") if isinstance(document, dict) else None
                if (
                    not isinstance(document, dict)
                    or document.get("version") != 1
                    or not isinstance(profiles, list)
                    or len(profiles) > 100
                ):
                    metadata_json = "invalid"
                else:
                    metadata_json = "valid"
                    profile_count = str(len(profiles))
                    count_match = int(len(profiles) == 1)
                    expected_host = os.environ.get("MEETERM_SSH_HOST", "")
                    expected_username = os.environ.get("MEETERM_SSH_USERNAME", "")
                    try:
                        expected_port = int(os.environ.get("MEETERM_SSH_PORT", ""))
                    except ValueError:
                        expected_port = -1
                    expected_name = expected_host if suite == "names" else "Daily fixture"
                    expected_credential_saved = suite == "full"
                    profile = profiles[0] if len(profiles) == 1 else None
                    if isinstance(profile, dict):
                        field_matches["host"] = int(profile.get("host") == expected_host)
                        field_matches["port"] = int(
                            isinstance(profile.get("port"), int)
                            and not isinstance(profile.get("port"), bool)
                            and profile.get("port") == expected_port
                        )
                        field_matches["username"] = int(
                            profile.get("username") == expected_username
                        )
                        field_matches["name"] = int(profile.get("name") == expected_name)
                        field_matches["auth_method"] = int(
                            profile.get("authMethod") == "publicKey"
                        )
                        field_matches["credential_saved"] = int(
                            (
                                isinstance(profile.get("credentialID"), str)
                                and bool(profile.get("credentialID"))
                            )
                            == expected_credential_saved
                        )
                        profile_match = int(
                            count_match == 1
                            and all(value == 1 for value in field_matches.values())
                        )
        except (OSError, TypeError, ValueError, UnicodeError, RecursionError):
            metadata_json = "invalid" if metadata_file == "present" else metadata_file

    return [
        f"metadata_container={container_status}",
        f"metadata_file={metadata_file}",
        f"metadata_json={metadata_json}",
        f"metadata_profile_count={profile_count}",
        f"metadata_profile_count_match={count_match}",
        f"metadata_profile_host_match={field_matches['host']}",
        f"metadata_profile_port_match={field_matches['port']}",
        f"metadata_profile_username_match={field_matches['username']}",
        f"metadata_profile_name_match={field_matches['name']}",
        f"metadata_profile_auth_method_match={field_matches['auth_method']}",
        f"metadata_profile_credential_saved_match={field_matches['credential_saved']}",
        f"metadata_profile_match={profile_match}",
    ]


def _probe_fixture_ssh() -> str:
    """Run a fixed, strict-host-key SSH health probe against the live fixture."""

    host = os.environ.get("MEETERM_SSH_HOST", "")
    port_text = os.environ.get("MEETERM_SSH_PORT", "")
    username = os.environ.get("MEETERM_SSH_USERNAME", "")
    client_key = os.environ.get("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE", "")
    host_key = os.environ.get("MEETERM_SSH_HOST_KEY_FILE", "")
    if not all((host, port_text, username, client_key, host_key)):
        return "unavailable"
    try:
        port = int(port_text)
        if not 1 <= port <= 65535:
            return "unavailable"
        public_key = Path(host_key).read_text(encoding="utf-8").strip()
        if not public_key or "\n" in public_key or "\r" in public_key:
            return "unavailable"
        if not Path(client_key).is_file():
            return "unavailable"
    except (OSError, ValueError, UnicodeError):
        return "unavailable"

    ssh = shutil.which("ssh")
    if ssh is None:
        return "unavailable"

    known_hosts_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            prefix="meeterm-ios-known-hosts-",
            delete=False,
        ) as stream:
            known_hosts_path = Path(stream.name)
            os.chmod(known_hosts_path, 0o600)
            stream.write(f"[{host}]:{port} {public_key}\n")
        completed = subprocess.run(
            [
                ssh,
                "-F",
                "/dev/null",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "IdentityAgent=none",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=1",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "GlobalKnownHostsFile=/dev/null",
                "-o",
                f"UserKnownHostsFile={known_hosts_path}",
                "-i",
                client_key,
                "-p",
                str(port),
                "-l",
                username,
                host,
                "tmux -V >/dev/null 2>&1 && printf '%s\\n' 'meeterm-ios-ssh-probe-v1'",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=15,
            check=False,
        )
        if completed.returncode == 0 and completed.stdout == f"{SSH_PROBE_NONCE}\n":
            return "passed"
        return "failed"
    except subprocess.TimeoutExpired:
        return "timeout"
    except OSError:
        return "unavailable"
    finally:
        if known_hosts_path is not None:
            try:
                known_hosts_path.unlink()
            except OSError:
                pass


def write_connection_failure_diagnostics(
    artifact_dir: Path,
    simulator_udid: str,
    *,
    suite: str,
    bundle_id: str = "dev.meeterm.app",
) -> None:
    """Write fixed, failure-only endpoint and fixture-health observations."""

    if not _fixture_diagnostics_available(suite):
        return
    try:
        container_status, container = _simulator_data_container(simulator_udid, bundle_id)
    except Exception:
        container_status, container = "unavailable", None
    try:
        metadata_lines = _profile_metadata_lines(container_status, container, suite=suite)
    except Exception:
        metadata_lines = [
            "metadata_container=unavailable",
            "metadata_file=unavailable",
            "metadata_json=unavailable",
            "metadata_profile_count=unavailable",
            "metadata_profile_count_match=0",
            "metadata_profile_host_match=0",
            "metadata_profile_port_match=0",
            "metadata_profile_username_match=0",
            "metadata_profile_name_match=0",
            "metadata_profile_auth_method_match=0",
            "metadata_profile_credential_saved_match=0",
            "metadata_profile_match=0",
        ]
    lines = [f"suite={suite}", *metadata_lines]
    try:
        probe = _probe_fixture_ssh()
    except Exception:
        probe = "unavailable"
    lines.append(f"ssh_probe={probe}")
    write_text(
        artifact_dir / CONNECTION_FAILURE_DIAGNOSTICS_NAME,
        "\n".join(lines) + "\n",
    )


def selection_copy_contract(marker_path: Path, marker_value: str) -> tuple[Path, Path, str, str]:
    """Derive the per-run clipboard handshake without another environment contract."""

    return (
        Path(str(marker_path) + ".selection-copy-request"),
        Path(str(marker_path) + ".selection-copy-result"),
        f"{marker_value}-selection-copy-request\n",
        f"{marker_value}-selection-copy-passed\n",
    )


def _write_atomic_result(path: Path, value: str) -> None:
    temporary = Path(str(path) + ".tmp")
    try:
        temporary.write_text(value, encoding="utf-8")
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


@contextmanager
def observe_selection_copy(
    simulator_udid: str,
    marker_path: Path,
    marker_value: str,
    diagnostics_path: Path,
):
    """Validate the public copied text without reading it in the XCTest runner."""

    request_path, result_path, request_token, passed_token = selection_copy_contract(
        marker_path, marker_value
    )
    temporary_result = Path(str(result_path) + ".tmp")
    for path in (request_path, result_path, temporary_result):
        path.unlink(missing_ok=True)
    diagnostics_path.unlink(missing_ok=True)
    stopped = threading.Event()

    def finish(status: str) -> None:
        result = "passed" if status == "passed" else "failed"
        reason = "none" if status == "passed" else status
        write_text(diagnostics_path, f"result={result}\nreason={reason}\n")
        try:
            token = passed_token if status == "passed" else f"{marker_value}-selection-copy-{status}\n"
            _write_atomic_result(result_path, token)
        except OSError:
            # A missing result makes the XCTest gate fail closed.
            write_text(diagnostics_path, "result=failed\nreason=result_write_failed\n")

    def monitor() -> None:
        while not stopped.wait(0.1):
            try:
                request = request_path.read_text(encoding="utf-8")
            except FileNotFoundError:
                continue
            except (OSError, UnicodeError):
                finish("request_rejected")
                return
            if request != request_token:
                finish("request_rejected")
                return

            xcrun = shutil.which("xcrun")
            if xcrun is None:
                finish("command_unavailable")
                return
            try:
                completed = subprocess.run(
                    [xcrun, "simctl", "pbpaste", simulator_udid],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    timeout=10,
                    check=False,
                )
            except subprocess.TimeoutExpired:
                finish("command_timeout")
                return
            except OSError:
                finish("command_failed")
                return
            if completed.returncode != 0:
                finish("command_failed")
            elif not completed.stdout:
                finish("clipboard_empty")
            elif b"COPY" not in completed.stdout or "日本語".encode() not in completed.stdout:
                finish("clipboard_mismatch")
            else:
                finish("passed")
            return

    thread = threading.Thread(target=monitor, name="selection-copy-observer", daemon=True)
    thread.start()
    try:
        yield
    finally:
        stopped.set()
        thread.join(timeout=15)
        if not diagnostics_path.exists():
            write_text(diagnostics_path, "result=unavailable\nreason=request_not_observed\n")
        for path in (request_path, result_path, temporary_result):
            path.unlink(missing_ok=True)


def validation_lines(
    *,
    result: str,
    workspaces: int,
    panes: int,
    stage: str,
    reason: str,
    ui_stage: str,
) -> str:
    return (
        f"result={result}\n"
        "fixture=disposable-openssh-tmux\n"
        f"workspace_count={workspaces}\n"
        f"pane_count={panes}\n"
        f"stage={stage}\n"
        f"reason={reason}\n"
        f"ui_last_stage={ui_stage}\n"
    )


def focused_validation_lines(*, result: str, suite: str, stage: str, reason: str, ui_stage: str) -> str:
    return (
        f"result={result}\n"
        f"suite={suite}\n"
        f"stage={stage}\n"
        f"reason={reason}\n"
        f"ui_last_stage={ui_stage}\n"
    )


def suite_validation_lines(
    *,
    suite: str,
    result: str,
    workspaces: int,
    panes: int,
    stage: str,
    reason: str,
    ui_stage: str,
) -> str:
    if suite == "full":
        return validation_lines(
            result=result,
            workspaces=workspaces,
            panes=panes,
            stage=stage,
            reason=reason,
            ui_stage=ui_stage,
        )
    return focused_validation_lines(
        result=result,
        suite=suite,
        stage=stage,
        reason=reason,
        ui_stage=ui_stage,
    )


def last_ui_stage(path: Path) -> str:
    """Return only the final allowlisted XCTest stage for a failed run."""

    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return "unavailable"
    for line in reversed(lines):
        candidate = line.strip()
        if re.fullmatch(r"[A-Za-z0-9_-]{1,80}", candidate):
            return candidate
    return "unavailable"


def write_xcuitest_diagnostics(
    raw_log: Path,
    destination: Path,
    exit_code: int | None,
    *,
    xcodebuild_started_at: float | None = None,
    xcodebuild_elapsed_seconds: float | None = None,
    xcodebuild_timeout_seconds: float | None = None,
) -> None:
    """Keep runner failures observable without copying XCTest's credential text."""
    try:
        log = raw_log.read_text(encoding="utf-8", errors="replace").lower()
        available = True
    except OSError:
        log = ""
        available = False
    signals = {
        "test_case_started": r"test case .+ started",
        "test_suite_started": r"test suite .+ started",
        "runner_launch_failed": r"failed to launch|unable to launch|failed to start.*test runner",
        "runner_early_exit": r"early unexpected exit|test runner.*crash|runner.*exited",
        "runner_connection_failed": r"failed to establish.*connection|lost.*connection|failed to get test.*ready|never began executing",
        "test_bundle_load_failed": r"failed to load.*bundle|could not load.*bundle|dlopen|symbol not found",
        "swift_cast_failed": r"could not cast value of type",
        "simulator_boot_failed": r"failed to boot|unable to boot",
        "device_preparation_failed": r"failed to prepare.*device|failed to install|unable to install",
        "disk_full": r"no space left on device",
        "testing_cancelled": r"testing cancel(?:led|ed)|test execution was interrupted",
    }
    lines = [
        f"raw_log_available={int(available)}",
        f"exit_code={exit_code if exit_code is not None else 'unavailable'}",
    ]
    lines.extend(f"{name}={int(re.search(pattern, log) is not None)}" for name, pattern in signals.items())
    if xcodebuild_started_at is not None:
        lines.extend(
            (
                f"xcodebuild_start_epoch_ms={int(xcodebuild_started_at * 1000)}",
                f"xcodebuild_elapsed_ms={int(max(0.0, xcodebuild_elapsed_seconds or 0.0) * 1000)}",
                f"xcodebuild_timeout_ms={int(max(0.0, xcodebuild_timeout_seconds or 0.0) * 1000)}",
            )
        )
    try:
        write_text(destination, "\n".join(lines) + "\n")
    except OSError:
        # Diagnostics must not replace the original test failure.
        pass


@contextmanager
def record_daily_interactions(simulator_udid: str, stage_path: Path, artifact_dir: Path):
    """Record only the post-authentication daily-use section, never the form."""
    stopped = threading.Event()

    def monitor() -> None:
        recorder = None
        result = "unavailable"
        reason = "daily_section_not_reached"
        video = artifact_dir / "daily-interactions.mp4"
        try:
            while not stopped.wait(0.5):
                try:
                    stages = stage_path.read_text(encoding="utf-8").splitlines()
                except OSError:
                    continue
                # These markers are emitted after authentication, with the
                # native terminal/workspace UI already visible. No later
                # focused operation opens an authentication form.
                if not any(marker in stages for marker in ("daily_selection", "names_started")):
                    continue
                xcrun = shutil.which("xcrun")
                if xcrun is None:
                    reason = "xcrun_unavailable"
                    break
                recorder = subprocess.Popen(
                    [xcrun, "simctl", "io", simulator_udid, "recordVideo", "--codec=h264", str(video)],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                deadline = time.monotonic() + 180
                while not stopped.wait(0.5) and time.monotonic() < deadline:
                    if recorder.poll() is not None:
                        break
                    try:
                        completed_stages = stage_path.read_text(encoding="utf-8").splitlines()
                        if any(marker in completed_stages for marker in ("daily_complete", "names_complete")):
                            break
                    except OSError:
                        pass
                reason = "capture_failed"
                break
        except OSError:
            reason = "capture_unavailable"
        finally:
            if recorder is not None:
                if recorder.poll() is None:
                    try:
                        recorder.send_signal(signal.SIGINT)
                    except ProcessLookupError:
                        pass
                try:
                    recorder.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    recorder.kill()
                    recorder.wait(timeout=5)
                if recorder.returncode == 0 and video.is_file() and video.stat().st_size > 0:
                    result, reason = "captured", "none"
                else:
                    video.unlink(missing_ok=True)
            write_text(artifact_dir / "daily-recording.txt", f"recording={result}\nreason={reason}\n")

    thread = threading.Thread(target=monitor, name="daily-interaction-evidence", daemon=True)
    thread.start()
    try:
        yield
    finally:
        stopped.set()
        thread.join(timeout=25)


def _copy_xctestrun(source: Path) -> Path:
    """Copy the pristine test configuration before adding per-run variables."""

    temporary_path: Path | None = None
    try:
        descriptor, name = tempfile.mkstemp(
            prefix=".meeterm-",
            suffix=".xctestrun",
            dir=source.parent,
        )
        os.close(descriptor)
        temporary_path = Path(name)
        shutil.copyfile(source, temporary_path)
        return temporary_path
    except OSError as error:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
        raise SmokeFailure("xcuitest_setup", "xctestrun_copy_failed") from error


def _require_case_markers(path: Path, cases: tuple[str, ...], stage: str) -> None:
    expected = {f"case={case} result=passed" for case in cases}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        lines = []
    # Length matters as well as membership: duplicate success lines must not
    # hide a repeated test or a missing case.
    if len(lines) != len(expected) or set(lines) != expected:
        raise SmokeFailure(stage, "cases_incomplete")


def _require_stage_markers(path: Path, required: tuple[str, ...], stage: str) -> None:
    try:
        observed = set(path.read_text(encoding="utf-8").splitlines())
    except (OSError, UnicodeError):
        observed = set()
    if not set(required).issubset(observed):
        raise SmokeFailure(stage, "completion_marker_missing")


def _test_steps(
    suite: str,
    result_bundle: Path,
    raw_log: Path,
    diagnostics_path: Path,
) -> tuple[tuple[str, tuple[str, ...], Path, Path, Path, Path | None, tuple[str, ...]], ...]:
    """Return ordered selectors and their fixed completion contracts."""

    storage = (
        "xcuitest_storage",
        (STORAGE_TEST_SELECTOR,),
        result_bundle.with_name(result_bundle.stem + "-storage.xcresult"),
        raw_log.with_name(raw_log.stem + "-storage.log"),
        diagnostics_path.with_name("ios-storage-xctest-runner-diagnostics.txt"),
        diagnostics_path.parent / "ios-native-storage-validation.txt",
        STORAGE_CASES,
    )
    native = (
        "xcuitest_native",
        (NATIVE_TEST_SELECTOR,),
        result_bundle.with_name(result_bundle.stem + "-native.xcresult"),
        raw_log.with_name(raw_log.stem + "-native.log"),
        diagnostics_path.with_name("ios-native-xctest-runner-diagnostics.txt"),
        diagnostics_path.parent / "ios-native-input-validation.txt",
        NATIVE_INPUT_CASES,
    )
    standard = (
        "xcuitest_standard",
        (STANDARD_TEST_SELECTOR, NATIVE_TEST_SELECTOR),
        result_bundle,
        raw_log,
        diagnostics_path,
        diagnostics_path.parent / "ios-ui-standard-validation.txt",
        ("standard",),
    )
    ssh = (
        "xcuitest_ssh",
        (SSH_TEST_SELECTOR,),
        result_bundle,
        raw_log,
        diagnostics_path,
        diagnostics_path.parent / "ios-ui-ssh-validation.txt",
        ("ssh",),
    )
    if suite == "standard":
        return (storage, standard)
    if suite == "ssh":
        return (ssh,)
    if suite == "forms":
        return (
            (
                "xcuitest_forms",
                (FORMS_TEST_SELECTOR,),
                result_bundle.with_name(result_bundle.stem + "-forms.xcresult"),
                raw_log.with_name(raw_log.stem + "-forms.log"),
                diagnostics_path.with_name("ios-forms-xctest-runner-diagnostics.txt"),
                diagnostics_path.parent / "ios-ui-forms-validation.txt",
                ("forms",),
            ),
        )
    if suite == "names":
        return (
            (
                "xcuitest_names",
                (NAMES_TEST_SELECTOR,),
                result_bundle.with_name(result_bundle.stem + "-names.xcresult"),
                raw_log.with_name(raw_log.stem + "-names.log"),
                diagnostics_path.with_name("ios-names-xctest-runner-diagnostics.txt"),
                diagnostics_path.parent / "ios-ui-names-validation.txt",
                ("names",),
            ),
        )
    if suite == "native":
        return (storage, native)
    return (
        storage,
        (
            "xcuitest",
            (FULL_TEST_SELECTOR, NATIVE_TEST_SELECTOR),
            result_bundle,
            raw_log,
            diagnostics_path,
            diagnostics_path.parent / "ios-native-input-validation.txt",
            NATIVE_INPUT_CASES,
        ),
    )


def run_xcuitest(
    *,
    derived_data: Path,
    simulator_udid: str,
    result_bundle: Path,
    raw_log: Path,
    diagnostics_path: Path,
    suite: str = "full",
) -> int:
    if suite not in IOS_SUITES:
        raise SmokeFailure("xcuitest_setup", "unknown_suite")
    products = derived_data / "Build" / "Products"
    bundles = sorted(
        path for path in products.glob("*.xctestrun")
        if not path.name.startswith(".")
    )
    if len(bundles) != 1:
        raise SmokeFailure("xcuitest_setup", "xctestrun_unavailable")
    xctestrun_path = _copy_xctestrun(bundles[0])
    xcodebuild = shutil.which("xcodebuild")
    if xcodebuild is None:
        xctestrun_path.unlink(missing_ok=True)
        raise SmokeFailure("xcuitest_setup", "xcodebuild_unavailable")

    command = [
        xcodebuild,
        "test-without-building",
        "-xctestrun",
        str(xctestrun_path),
        "-destination",
        f"platform=iOS Simulator,id={simulator_udid}",
        "-quiet",
        "CODE_SIGNING_ALLOWED=NO",
        "CODE_SIGNING_REQUIRED=NO",
    ]
    raw_log.parent.mkdir(parents=True, exist_ok=True)
    runner_environment = dict(os.environ)
    # The UI test receives only the explicit, non-secret fixture contract via
    # the xctestrun plist. Do not let xcodebuild inherit the fixture's
    # passphrase or alternate private-key paths from the sourced env file.
    for environment_name in list(runner_environment):
        if (
            environment_name.startswith("MEETERM_SSH_")
            or environment_name in RUNTIME_ENVIRONMENT_NAMES
        ):
            runner_environment.pop(environment_name, None)
    try:
        inject_test_environment(xctestrun_path, suite=suite)
    except BaseException:
        xctestrun_path.unlink(missing_ok=True)
        raise
    # Storage runs inside the entitled app. Its cleanup must finish before the
    # UI runner launches the same app. Separate invocations enforce that order
    # while the selected suite retains its own bounded execution budget.
    deadline = time.monotonic() + SUITE_TIMEOUT_SECONDS[suite]
    steps = _test_steps(suite, result_bundle, raw_log, diagnostics_path)
    storage_validation = diagnostics_path.parent / "ios-native-storage-validation.txt"
    native_validation = diagnostics_path.parent / "ios-native-input-validation.txt"
    standard_validation = diagnostics_path.parent / "ios-ui-standard-validation.txt"
    ssh_validation = diagnostics_path.parent / "ios-ui-ssh-validation.txt"
    forms_validation = diagnostics_path.parent / "ios-ui-forms-validation.txt"
    names_validation = diagnostics_path.parent / "ios-ui-names-validation.txt"
    for path in (
        storage_validation,
        native_validation,
        standard_validation,
        ssh_validation,
        forms_validation,
        names_validation,
    ):
        path.unlink(missing_ok=True)
    # Completion records are per invocation. Do not let a previous focused or
    # full run satisfy the current suite's stage contract.
    (diagnostics_path.parent / "ios-ui-stages.txt").unlink(missing_ok=True)
    exit_code = 0
    try:
        for stage, selections, bundle, log_path, diagnostic_path, validation_path, cases in steps:
            exit_code = None
            xcodebuild_started_at = None
            xcodebuild_elapsed_seconds = None
            xcodebuild_timeout_seconds = None
            try:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise SmokeFailure(stage, "xcodebuild_timeout")
                xcodebuild_started_at = time.time()
                xcodebuild_timeout_seconds = remaining
                with log_path.open("w", encoding="utf-8") as stream:
                    completed = subprocess.run(
                        [*command, *selections, "-resultBundlePath", str(bundle)],
                        env=runner_environment,
                        stdin=subprocess.DEVNULL,
                        stdout=stream,
                        stderr=subprocess.STDOUT,
                        timeout=remaining,
                        check=False,
                    )
                    exit_code = completed.returncode
            except subprocess.TimeoutExpired as error:
                raise SmokeFailure(stage, "xcodebuild_timeout") from error
            except OSError as error:
                raise SmokeFailure(stage, "xcodebuild_failed") from error
            finally:
                if xcodebuild_started_at is not None:
                    xcodebuild_elapsed_seconds = time.time() - xcodebuild_started_at
                write_xcuitest_diagnostics(
                    log_path,
                    diagnostic_path,
                    exit_code,
                    xcodebuild_started_at=xcodebuild_started_at,
                    xcodebuild_elapsed_seconds=xcodebuild_elapsed_seconds,
                    xcodebuild_timeout_seconds=xcodebuild_timeout_seconds,
                )
            if exit_code != 0:
                reason = {
                    "xcuitest_storage": "storage_tests_failed",
                    "xcuitest_native": "native_tests_failed",
                    "xcuitest_forms": "forms_tests_failed",
                    "xcuitest_names": "names_tests_failed",
                }.get(stage, "ui_test_failed")
                raise SmokeFailure(stage, reason)
            if validation_path is not None:
                try:
                    _require_case_markers(validation_path, cases, stage)
                except SmokeFailure as error:
                    reason = {
                        "xcuitest_storage": "storage_cases_incomplete",
                        "xcuitest_native": "native_cases_incomplete",
                        "xcuitest_forms": "forms_cases_incomplete",
                        "xcuitest_names": "names_cases_incomplete",
                        "xcuitest": "native_cases_incomplete",
                    }.get(stage, "cases_incomplete")
                    raise SmokeFailure(stage, reason) from error
        if suite == "standard":
            # The standard UI invocation also selects the native input suite.
            # Keep its seven-case contract separate from the UI route marker so
            # a test that only launches the screen flow cannot satisfy input
            # coverage accidentally.
            try:
                _require_case_markers(native_validation, NATIVE_INPUT_CASES, "xcuitest_standard")
            except SmokeFailure as error:
                raise SmokeFailure("xcuitest_standard", "native_cases_incomplete") from error
            _require_stage_markers(
                diagnostics_path.parent / "ios-ui-stages.txt",
                ("standard_complete", "foundation_verified"),
                "xcuitest_standard",
            )
        elif suite == "ssh":
            _require_stage_markers(
                diagnostics_path.parent / "ios-ui-stages.txt",
                ("ssh_complete",),
                "xcuitest_ssh",
            )
        elif suite == "forms":
            _require_stage_markers(
                diagnostics_path.parent / "ios-ui-stages.txt",
                ("forms_complete",),
                "xcuitest_forms",
            )
        elif suite == "names":
            _require_stage_markers(
                diagnostics_path.parent / "ios-ui-stages.txt",
                ("names_complete",),
                "xcuitest_names",
            )
        elif suite == "full":
            _require_stage_markers(
                diagnostics_path.parent / "ios-ui-stages.txt",
                ("daily_complete", "foundation_verified"),
                "xcuitest",
            )
        return exit_code if exit_code is not None else 0
    finally:
        xctestrun_path.unlink(missing_ok=True)


def inject_test_environment(xctestrun_path: Path, *, suite: str = "full") -> None:
    """Pass fixture paths and sanitized stage locations into the test runner.

    `xcodebuild test-without-building -xctestrun` does not consistently pass
    arbitrary shell variables through to XCTest across Xcode releases. The
    xctestrun plist is generated under RUNNER_TEMP, so adding the small test
    contract there keeps the UI test deterministic without putting it in an
    artifact or command line.
    """

    if suite not in IOS_SUITES:
        raise SmokeFailure("xcuitest_setup", "unknown_suite")
    if suite == "full":
        names = FULL_TEST_ENVIRONMENT_NAMES
    elif suite == "ssh":
        names = SSH_TEST_ENVIRONMENT_NAMES
    elif suite == "names":
        names = NAMES_TEST_ENVIRONMENT_NAMES
    else:
        names = COMMON_TEST_ENVIRONMENT_NAMES
    environment = {
        name: os.environ[name]
        for name in names
        if os.environ.get(name)
    }
    try:
        with xctestrun_path.open("rb") as stream:
            document = plistlib.load(stream)
    except (OSError, plistlib.InvalidFileException) as error:
        raise SmokeFailure("xcuitest_setup", "xctestrun_invalid") from error

    targets: list[dict[str, object]] = []

    def visit(value: object) -> None:
        if isinstance(value, dict):
            if "TestBundlePath" in value or "UITargetAppPath" in value:
                targets.append(value)
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)

    visit(document)
    if not targets:
        raise SmokeFailure("xcuitest_setup", "test_target_unavailable")
    for target in targets:
        for variable_key in ("TestingEnvironmentVariables", "EnvironmentVariables"):
            existing = target.get(variable_key)
            if not isinstance(existing, dict):
                existing = {}
            for name in list(existing):
                if (
                    name.startswith("MEETERM_SSH_")
                    or name in RUNTIME_ENVIRONMENT_NAMES
                ):
                    existing.pop(name, None)
            existing.update(environment)
            target[variable_key] = existing
    try:
        with xctestrun_path.open("wb") as stream:
            plistlib.dump(document, stream, sort_keys=False)
    except OSError as error:
        raise SmokeFailure("xcuitest_setup", "xctestrun_write_failed") from error


def main() -> int:
    parser = argparse.ArgumentParser(description="Run a selected iOS Simulator UI smoke suite.")
    parser.add_argument("--artifact-dir", type=Path, required=True)
    parser.add_argument("--derived-data", type=Path, required=True)
    parser.add_argument("--simulator-udid", required=True)
    parser.add_argument(
        "--suite",
        choices=IOS_SUITES,
        default=os.environ.get("MEETERM_IOS_SUITE", "standard"),
    )
    args = parser.parse_args()

    suite = args.suite
    workspaces = 0
    panes = 0
    stage = "startup"
    reason = "unexpected"
    ui_stage = "unavailable"
    socket_path: Path | None = None
    marker_path: Path | None = None
    validation_filename = {
        "full": "ios-validation.txt",
        "standard": "ios-standard-validation.txt",
        "ssh": "ios-ssh-validation.txt",
    }.get(suite, f"ios-{suite}-validation.txt")
    validation_path = args.artifact_dir / validation_filename
    try:
        args.artifact_dir.mkdir(parents=True, exist_ok=True)
        (args.artifact_dir / INPUT_DIAGNOSTICS_NAME).unlink(missing_ok=True)
        try:
            (args.artifact_dir / CONNECTION_FAILURE_DIAGNOSTICS_NAME).unlink()
        except FileNotFoundError:
            pass
        except OSError:
            pass
        stage_path = args.artifact_dir / "ios-ui-stages.txt"
        try:
            stage_path.unlink()
        except FileNotFoundError:
            pass
        marker_root = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
        marker_path = marker_root / f"meeterm-ios-marker-{os.getpid()}-{secrets.token_hex(6)}"
        os.environ["MEETERM_IOS_ARTIFACT_DIR"] = str(args.artifact_dir)
        os.environ["MEETERM_IOS_STAGE_PATH"] = str(stage_path)
        os.environ["MEETERM_IOS_MARKER_PATH"] = str(marker_path)
        if suite in ("full", "ssh", "names"):
            socket_path = fixture_socket()
            required("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
            required("MEETERM_SSH_FINGERPRINT")
            if suite in ("full", "ssh"):
                marker_value = (
                    f"ios-input-{secrets.token_hex(8)}"
                    if suite == "full"
                    else f"ios-ssh-input-{secrets.token_hex(8)}"
                )
                os.environ["MEETERM_IOS_MARKER_VALUE"] = marker_value
                if suite == "full":
                    handoff_value = f"ios-handoff-{secrets.token_hex(8)}"
                    os.environ["MEETERM_IOS_HANDOFF_VALUE"] = handoff_value

            stage = "tmux_fixture"
            workspaces, panes = prepare_topology(socket_path)
            write_text(
                args.artifact_dir / "fixture-validation.txt",
                "fixture=ready\nworkspace_count=2\npane_count=3\n",
            )

            stage = "xcuitest"
            if suite == "full":
                with record_daily_interactions(args.simulator_udid, stage_path, args.artifact_dir), \
                     observe_selection_copy(
                         args.simulator_udid,
                         marker_path,
                         marker_value,
                         args.artifact_dir / "ios-native-copy-validation.txt",
                     ):
                    run_status = run_xcuitest(
                        derived_data=args.derived_data,
                        simulator_udid=args.simulator_udid,
                        result_bundle=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                        / "meeterm-ios-ui.xcresult",
                        raw_log=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                        / "meeterm-ios-ui-xcodebuild.log",
                        diagnostics_path=args.artifact_dir / "ios-xctest-runner-diagnostics.txt",
                        suite=suite,
                    )
                if run_status != 0:
                    ui_stage = last_ui_stage(Path(os.environ["MEETERM_IOS_STAGE_PATH"]))
                    raise SmokeFailure("xcuitest", "ui_test_failed")

                stage = "handoff"
                ordinary_desktop_attach(socket_path)
                write_text(
                    args.artifact_dir / "handoff-validation.txt",
                    "desktop_attach=passed\nsession=meeterm\n",
                )

                stage = "marker"
                if marker_path is None or not marker_path.is_file():
                    raise SmokeFailure(stage, "marker_unavailable")
                marker_lines = marker_path.read_text(encoding="utf-8").splitlines()
                if marker_lines != [marker_value, handoff_value, handoff_value]:
                    raise SmokeFailure(stage, "marker_sequence_invalid")

                write_text(
                    validation_path,
                    validation_lines(
                        result="passed",
                        workspaces=workspaces,
                        panes=panes,
                        stage="complete",
                        reason="none",
                        ui_stage="complete",
                    ),
                )
                print("iOS real SSH UI smoke passed.")
                return 0

            if suite == "ssh":
                # The short SSH suite keeps only connection, one native input
                # acknowledgement, and explicit disconnect. It deliberately
                # omits full-flow handoff, copy, reconnect, and daily CRUD.
                run_status = run_xcuitest(
                    derived_data=args.derived_data,
                    simulator_udid=args.simulator_udid,
                    result_bundle=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                    / "meeterm-ios-ssh.xcresult",
                    raw_log=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                    / "meeterm-ios-ssh-xcodebuild.log",
                    diagnostics_path=args.artifact_dir / "ios-ssh-xctest-runner-diagnostics.txt",
                    suite=suite,
                )
                if run_status != 0:
                    ui_stage = last_ui_stage(Path(os.environ["MEETERM_IOS_STAGE_PATH"]))
                    raise SmokeFailure("xcuitest_ssh", "ssh_tests_failed")

                stage = "marker"
                if marker_path is None or not marker_path.is_file():
                    raise SmokeFailure(stage, "marker_unavailable")
                marker_lines = marker_path.read_text(encoding="utf-8").splitlines()
                if marker_lines != [marker_value]:
                    raise SmokeFailure(stage, "marker_sequence_invalid")

                write_text(
                    validation_path,
                    suite_validation_lines(
                        result="passed",
                        suite=suite,
                        workspaces=workspaces,
                        panes=panes,
                        stage="complete",
                        reason="none",
                        ui_stage="complete",
                    ),
                )
                print("iOS short SSH UI smoke passed.")
                return 0

            # The names suite shares only the real SSH fixture and topology
            # setup. It deliberately has no desktop handoff or copy observer.
            with record_daily_interactions(args.simulator_udid, stage_path, args.artifact_dir):
                run_status = run_xcuitest(
                    derived_data=args.derived_data,
                    simulator_udid=args.simulator_udid,
                    result_bundle=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                    / "meeterm-ios-names.xcresult",
                    raw_log=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                    / "meeterm-ios-names-xcodebuild.log",
                    diagnostics_path=args.artifact_dir / "ios-names-xctest-runner-diagnostics.txt",
                    suite=suite,
                )
            if run_status != 0:
                ui_stage = last_ui_stage(Path(os.environ["MEETERM_IOS_STAGE_PATH"]))
                raise SmokeFailure("xcuitest_names", "names_tests_failed")
            write_text(
                validation_path,
                suite_validation_lines(
                    result="passed",
                    suite=suite,
                    workspaces=workspaces,
                    panes=panes,
                    stage="complete",
                    reason="none",
                    ui_stage="complete",
                ),
            )
            print("iOS names UI smoke passed.")
            return 0

        # Standard/forms/native runs intentionally have no fixture contract.
        # They still receive a fresh marker path because the UI test setup uses
        # it as a per-run cleanup anchor; no SSH values are injected.
        for environment_name in RUNTIME_ENVIRONMENT_NAMES:
            if environment_name not in COMMON_TEST_ENVIRONMENT_NAMES:
                os.environ.pop(environment_name, None)
        stage = "xcuitest"
        run_status = run_xcuitest(
            derived_data=args.derived_data,
            simulator_udid=args.simulator_udid,
            result_bundle=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
            / f"meeterm-ios-{suite}.xcresult",
            raw_log=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
            / f"meeterm-ios-{suite}-xcodebuild.log",
            diagnostics_path=args.artifact_dir / f"ios-{suite}-xctest-runner-diagnostics.txt",
            suite=suite,
        )
        if run_status != 0:
            ui_stage = last_ui_stage(Path(os.environ["MEETERM_IOS_STAGE_PATH"]))
            raise SmokeFailure("xcuitest", "focused_tests_failed")
        write_text(
            validation_path,
            suite_validation_lines(
                result="passed",
                suite=suite,
                workspaces=workspaces,
                panes=panes,
                stage="complete",
                reason="none",
                ui_stage="complete",
            ),
        )
        print(f"iOS {suite} UI smoke passed.")
        return 0
    except SmokeFailure as error:
        stage = error.stage
        reason = error.reason
        if stage.startswith("xcuitest"):
            ui_stage = last_ui_stage(Path(os.environ.get("MEETERM_IOS_STAGE_PATH", "")))
        if _fixture_diagnostics_available(suite):
            try:
                write_connection_failure_diagnostics(
                    args.artifact_dir,
                    args.simulator_udid,
                    suite=suite,
                )
            except Exception:
                pass
        print(f"iOS UI smoke failed at {stage}: {reason}", file=sys.stderr)
        write_text(
            validation_path,
            suite_validation_lines(
                suite=suite,
                result="failed",
                workspaces=workspaces,
                panes=panes,
                stage=stage,
                reason=reason,
                ui_stage=ui_stage,
            ),
        )
        return 1
    except (OSError, ValueError):
        if _fixture_diagnostics_available(suite):
            try:
                write_connection_failure_diagnostics(
                    args.artifact_dir,
                    args.simulator_udid,
                    suite=suite,
                )
            except Exception:
                pass
        print(f"iOS UI smoke failed at {stage}: driver_error", file=sys.stderr)
        write_text(
            validation_path,
            suite_validation_lines(
                suite=suite,
                result="failed",
                workspaces=workspaces,
                panes=panes,
                stage=stage,
                reason="driver_error",
                ui_stage=ui_stage,
            ),
        )
        return 1
    finally:
        if suite == "ssh" and socket_path is not None and marker_path is not None:
            try:
                write_short_ssh_input_diagnostics(args.artifact_dir, socket_path, marker_path)
            except Exception:
                # Diagnostic failures must preserve the original test outcome.
                write_text(args.artifact_dir / INPUT_DIAGNOSTICS_NAME,
                           '{"unavailable": true}\n')
        if marker_path is not None:
            for path in (
                marker_path,
                Path(str(marker_path) + ".selection-copy-request"),
                Path(str(marker_path) + ".selection-copy-result"),
            ):
                try:
                    path.unlink()
                except FileNotFoundError:
                    pass
                except OSError:
                    pass


if __name__ == "__main__":
    raise SystemExit(main())
