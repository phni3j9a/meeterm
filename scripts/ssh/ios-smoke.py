#!/usr/bin/env python3
"""Run the real iOS Simulator SSH/tmux UI smoke.

The fixture owns the host, keys, trust store, and ordinary tmux server. This
driver only prepares a disposable topology, invokes the generated XCUITest,
checks the same fixture through an ordinary tmux attach, and writes sanitized
evidence. XCTest's raw log and xcresult remain under RUNNER_TEMP.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
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


def write_xcuitest_diagnostics(raw_log: Path, destination: Path, exit_code: int | None) -> None:
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
                # This marker is emitted after the cold-restart saved-credential
                # reconnect, with the native terminal already visible. No later
                # daily-use operation opens an authentication form.
                if "daily_selection" not in stages:
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
                        if "daily_complete" in stage_path.read_text(encoding="utf-8").splitlines():
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


def run_xcuitest(
    *,
    derived_data: Path,
    simulator_udid: str,
    result_bundle: Path,
    raw_log: Path,
    diagnostics_path: Path,
) -> int:
    products = derived_data / "Build" / "Products"
    bundles = sorted(products.glob("*.xctestrun"))
    if len(bundles) != 1:
        raise SmokeFailure("xcuitest_setup", "xctestrun_unavailable")
    inject_test_environment(bundles[0])
    xcodebuild = shutil.which("xcodebuild")
    if xcodebuild is None:
        raise SmokeFailure("xcuitest_setup", "xcodebuild_unavailable")

    command = [
        xcodebuild,
        "test-without-building",
        "-xctestrun",
        str(bundles[0]),
        "-destination",
        f"platform=iOS Simulator,id={simulator_udid}",
        "-resultBundlePath",
        str(result_bundle),
        "-quiet",
        "CODE_SIGNING_ALLOWED=NO",
        "CODE_SIGNING_REQUIRED=NO",
    ]
    raw_log.parent.mkdir(parents=True, exist_ok=True)
    runner_environment = dict(os.environ)
    # The UI test receives only the explicit, non-secret fixture contract via
    # the xctestrun plist. Do not let xcodebuild inherit the fixture's
    # passphrase or alternate private-key paths from the sourced env file.
    for secret_name in (
        "MEETERM_SSH_PRIVATE_KEY_FILE",
        "MEETERM_SSH_PASSPHRASE",
        "MEETERM_SSH_KNOWN_HOSTS_FILE",
        "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE",
        "MEETERM_SSH_HOST_KEY_FILE",
        "MEETERM_SSH_ALTERNATE_HOST_KEY_FILE",
    ):
        runner_environment.pop(secret_name, None)
    exit_code = None
    try:
        with raw_log.open("w", encoding="utf-8") as stream:
            completed = subprocess.run(
                command,
                env=runner_environment,
                stdin=subprocess.DEVNULL,
                stdout=stream,
                stderr=subprocess.STDOUT,
                # The suite now includes native storage/input cases, a cold
                # saved-profile reconnect, settings, selection and tmux CRUD.
                # The partial Hosted run already took 13m34s; allow the full
                # sequence while retaining the shorter per-operation gates.
                timeout=1800,
                check=False,
            )
            exit_code = completed.returncode
    except subprocess.TimeoutExpired as error:
        raise SmokeFailure("xcuitest", "xcodebuild_timeout") from error
    except OSError as error:
        raise SmokeFailure("xcuitest", "xcodebuild_failed") from error
    finally:
        write_xcuitest_diagnostics(raw_log, diagnostics_path, exit_code)
    return completed.returncode


def inject_test_environment(xctestrun_path: Path) -> None:
    """Pass fixture paths and sanitized stage locations into the test runner.

    `xcodebuild test-without-building -xctestrun` does not consistently pass
    arbitrary shell variables through to XCTest across Xcode releases. The
    xctestrun plist is generated under RUNNER_TEMP, so adding the small test
    contract there keeps the UI test deterministic without putting it in an
    artifact or command line.
    """

    names = (
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
            existing.update(environment)
            target[variable_key] = existing
    try:
        with xctestrun_path.open("wb") as stream:
            plistlib.dump(document, stream, sort_keys=False)
    except OSError as error:
        raise SmokeFailure("xcuitest_setup", "xctestrun_write_failed") from error


def main() -> int:
    parser = argparse.ArgumentParser(description="Run the iOS real-SSH Simulator UI smoke.")
    parser.add_argument("--artifact-dir", type=Path, required=True)
    parser.add_argument("--derived-data", type=Path, required=True)
    parser.add_argument("--simulator-udid", required=True)
    args = parser.parse_args()

    workspaces = 0
    panes = 0
    stage = "startup"
    reason = "unexpected"
    ui_stage = "unavailable"
    socket_path: Path | None = None
    marker_path: Path | None = None
    validation_path = args.artifact_dir / "ios-validation.txt"
    try:
        args.artifact_dir.mkdir(parents=True, exist_ok=True)
        stage_path = args.artifact_dir / "ios-ui-stages.txt"
        try:
            stage_path.unlink()
        except FileNotFoundError:
            pass
        socket_path = fixture_socket()
        required("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
        required("MEETERM_SSH_FINGERPRINT")
        marker_root = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
        marker_path = marker_root / f"meeterm-ios-marker-{os.getpid()}-{secrets.token_hex(6)}"
        marker_value = f"ios-input-{secrets.token_hex(8)}"
        handoff_value = f"ios-handoff-{secrets.token_hex(8)}"
        os.environ["MEETERM_IOS_ARTIFACT_DIR"] = str(args.artifact_dir)
        os.environ["MEETERM_IOS_STAGE_PATH"] = str(stage_path)
        os.environ["MEETERM_IOS_MARKER_PATH"] = str(marker_path)
        os.environ["MEETERM_IOS_MARKER_VALUE"] = marker_value
        os.environ["MEETERM_IOS_HANDOFF_VALUE"] = handoff_value

        stage = "tmux_fixture"
        workspaces, panes = prepare_topology(socket_path)
        write_text(
            args.artifact_dir / "fixture-validation.txt",
            "fixture=ready\nworkspace_count=2\npane_count=3\n",
        )

        stage = "xcuitest"
        with record_daily_interactions(args.simulator_udid, stage_path, args.artifact_dir):
            run_status = run_xcuitest(
                derived_data=args.derived_data,
                simulator_udid=args.simulator_udid,
                result_bundle=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                / "meeterm-ios-ui.xcresult",
                raw_log=Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))
                / "meeterm-ios-ui-xcodebuild.log",
                diagnostics_path=args.artifact_dir / "ios-xctest-runner-diagnostics.txt",
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
    except SmokeFailure as error:
        stage = error.stage
        reason = error.reason
        if stage == "xcuitest":
            ui_stage = last_ui_stage(Path(os.environ.get("MEETERM_IOS_STAGE_PATH", "")))
        print(f"iOS UI smoke failed at {stage}: {reason}", file=sys.stderr)
        write_text(
            validation_path,
            validation_lines(
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
        print(f"iOS UI smoke failed at {stage}: driver_error", file=sys.stderr)
        write_text(
            validation_path,
            validation_lines(
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
        if marker_path is not None:
            try:
                marker_path.unlink()
            except FileNotFoundError:
                pass
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main())
