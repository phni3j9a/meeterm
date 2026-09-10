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


IOS_SUITES = ("full", "forms", "native")
SUITE_TIMEOUT_SECONDS = {
    "full": 1800.0,
    "forms": 600.0,
    "native": 600.0,
}
FULL_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testRealSshWorkspacePaneInputDisconnectReconnectAndHandoff"
)
FORMS_TEST_SELECTOR = (
    "-only-testing:meetermTests/"
    "MeetermSmokeUITests/testConnectionFormControlsWithoutSecrets"
)
NATIVE_TEST_SELECTOR = "-only-testing:meetermTests/TerminalInputViewTests"
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
    forms_validation = diagnostics_path.parent / "ios-ui-forms-validation.txt"
    for path in (storage_validation, native_validation, forms_validation):
        path.unlink(missing_ok=True)
    exit_code = 0
    try:
        for stage, selections, bundle, log_path, diagnostic_path, validation_path, cases in steps:
            exit_code = None
            try:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise SmokeFailure(stage, "xcodebuild_timeout")
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
                write_xcuitest_diagnostics(log_path, diagnostic_path, exit_code)
            if exit_code != 0:
                reason = {
                    "xcuitest_storage": "storage_tests_failed",
                    "xcuitest_native": "native_tests_failed",
                    "xcuitest_forms": "forms_tests_failed",
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
                        "xcuitest": "native_cases_incomplete",
                    }.get(stage, "cases_incomplete")
                    raise SmokeFailure(stage, reason) from error
        if suite == "forms":
            _require_stage_markers(
                diagnostics_path.parent / "ios-ui-stages.txt",
                ("forms_complete",),
                "xcuitest_forms",
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
    names = FULL_TEST_ENVIRONMENT_NAMES if suite == "full" else COMMON_TEST_ENVIRONMENT_NAMES
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
    parser.add_argument("--suite", choices=IOS_SUITES, default=os.environ.get("MEETERM_IOS_SUITE", "full"))
    args = parser.parse_args()

    suite = args.suite
    workspaces = 0
    panes = 0
    stage = "startup"
    reason = "unexpected"
    ui_stage = "unavailable"
    socket_path: Path | None = None
    marker_path: Path | None = None
    validation_path = args.artifact_dir / (
        "ios-validation.txt" if suite == "full" else f"ios-{suite}-validation.txt"
    )
    try:
        args.artifact_dir.mkdir(parents=True, exist_ok=True)
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
        if suite == "full":
            socket_path = fixture_socket()
            required("MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE")
            required("MEETERM_SSH_FINGERPRINT")
            marker_value = f"ios-input-{secrets.token_hex(8)}"
            handoff_value = f"ios-handoff-{secrets.token_hex(8)}"
            os.environ["MEETERM_IOS_MARKER_VALUE"] = marker_value
            os.environ["MEETERM_IOS_HANDOFF_VALUE"] = handoff_value

            stage = "tmux_fixture"
            workspaces, panes = prepare_topology(socket_path)
            write_text(
                args.artifact_dir / "fixture-validation.txt",
                "fixture=ready\nworkspace_count=2\npane_count=3\n",
            )

            stage = "xcuitest"
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

        # Focused forms/native runs intentionally have no fixture contract.
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
