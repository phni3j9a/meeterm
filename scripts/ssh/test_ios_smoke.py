"""Privacy and failure-path checks for the iOS XCTest runner diagnostics."""

import importlib.util
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("ios_smoke", Path(__file__).with_name("ios-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class SelectionCopyObserverTests(unittest.TestCase):
    @staticmethod
    def wait_for_result(path: Path) -> str:
        deadline = smoke.time.monotonic() + 2
        while smoke.time.monotonic() < deadline:
            try:
                return path.read_text(encoding="utf-8")
            except FileNotFoundError:
                smoke.time.sleep(0.01)
        raise AssertionError("selection copy observer did not write a result")

    def test_success_reads_clipboard_only_after_fresh_request_and_cleans_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "marker"
            request, result, request_token, passed_token = smoke.selection_copy_contract(
                marker, "run-token"
            )
            completed = subprocess.CompletedProcess(
                [], 0, stdout="COPY 日本語 selection\n".encode(), stderr=b"ignored-secret"
            )
            diagnostics = Path(directory) / "diagnostics.txt"
            with mock.patch.object(smoke.shutil, "which", return_value="/usr/bin/xcrun"), \
                 mock.patch.object(smoke.subprocess, "run", return_value=completed) as run:
                with smoke.observe_selection_copy("EXACT-UDID", marker, "run-token", diagnostics):
                    self.assertFalse(run.called)
                    smoke._write_atomic_result(request, request_token)
                    self.assertEqual(self.wait_for_result(result), passed_token)
                    self.assertEqual(self.wait_for_result(diagnostics), "result=passed\nreason=none\n")
            run.assert_called_once_with(
                ["/usr/bin/xcrun", "simctl", "pbpaste", "EXACT-UDID"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=10,
                check=False,
            )
            self.assertFalse(request.exists())
            self.assertFalse(result.exists())

    def test_no_request_never_reads_clipboard(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "marker"
            request, result, _, _ = smoke.selection_copy_contract(marker, "run-token")
            diagnostics = Path(directory) / "diagnostics.txt"
            with mock.patch.object(smoke.subprocess, "run") as run:
                with smoke.observe_selection_copy("EXACT-UDID", marker, "run-token", diagnostics):
                    smoke.time.sleep(0.15)
                    self.assertFalse(result.exists())
            run.assert_not_called()
            self.assertEqual(diagnostics.read_text(), "result=unavailable\nreason=request_not_observed\n")
            self.assertFalse(request.exists())
            self.assertFalse(result.exists())

    def test_stale_files_are_removed_and_cannot_trigger_clipboard_read(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "marker"
            request, result, request_token, passed_token = smoke.selection_copy_contract(
                marker, "run-token"
            )
            request.write_text(request_token, encoding="utf-8")
            result.write_text(passed_token, encoding="utf-8")
            diagnostics = Path(directory) / "diagnostics.txt"
            diagnostics.write_text("result=passed\nreason=stale\nPRIVATE-KEY-SECRET\n")
            with mock.patch.object(smoke.subprocess, "run") as run:
                with smoke.observe_selection_copy("EXACT-UDID", marker, "run-token", diagnostics):
                    smoke.time.sleep(0.15)
                    self.assertFalse(request.exists())
                    self.assertFalse(result.exists())
            run.assert_not_called()
            self.assertEqual(diagnostics.read_text(), "result=unavailable\nreason=request_not_observed\n")
            self.assertNotIn("SECRET", diagnostics.read_text())

    def test_wrong_request_fails_without_reading_clipboard(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "marker"
            request, result, _, _ = smoke.selection_copy_contract(marker, "run-token")
            diagnostics = Path(directory) / "diagnostics.txt"
            with mock.patch.object(smoke.subprocess, "run") as run:
                with smoke.observe_selection_copy("EXACT-UDID", marker, "run-token", diagnostics):
                    smoke._write_atomic_result(request, "stale-or-wrong-token\n")
                    observed = self.wait_for_result(result)
                    self.assertEqual(observed, "run-token-selection-copy-request_rejected\n")
                    self.assertNotIn("stale-or-wrong", observed)
                    self.assertEqual(self.wait_for_result(diagnostics), "result=failed\nreason=request_rejected\n")
            run.assert_not_called()

    def test_empty_or_wrong_clipboard_fails_without_persisting_contents(self):
        for stdout, status in (
            (b"", "clipboard_empty"),
            (b"PRIVATE-KEY-SECRET", "clipboard_mismatch"),
        ):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                marker = Path(directory) / "marker"
                request, result, request_token, _ = smoke.selection_copy_contract(
                    marker, "run-token"
                )
                diagnostics = Path(directory) / "diagnostics.txt"
                completed = subprocess.CompletedProcess([], 0, stdout=stdout, stderr=b"")
                with mock.patch.object(smoke.shutil, "which", return_value="/usr/bin/xcrun"), \
                     mock.patch.object(smoke.subprocess, "run", return_value=completed):
                    with smoke.observe_selection_copy("EXACT-UDID", marker, "run-token", diagnostics):
                        smoke._write_atomic_result(request, request_token)
                        observed = self.wait_for_result(result)
                        self.assertEqual(observed, f"run-token-selection-copy-{status}\n")
                        self.assertNotIn("SECRET", observed)
                        self.assertEqual(self.wait_for_result(diagnostics), f"result=failed\nreason={status}\n")
                        self.assertNotIn("SECRET", diagnostics.read_text())

    def test_clipboard_command_timeout_writes_only_fixed_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "marker"
            request, result, request_token, _ = smoke.selection_copy_contract(
                marker, "run-token"
            )
            diagnostics = Path(directory) / "diagnostics.txt"
            with mock.patch.object(smoke.shutil, "which", return_value="/usr/bin/xcrun"), \
                 mock.patch.object(
                     smoke.subprocess,
                     "run",
                     side_effect=subprocess.TimeoutExpired(
                         ["xcrun", "simctl", "pbpaste", "EXACT-UDID"], 10, output=b"SECRET"
                     ),
                 ):
                with smoke.observe_selection_copy("EXACT-UDID", marker, "run-token", diagnostics):
                    smoke._write_atomic_result(request, request_token)
                    observed = self.wait_for_result(result)
                    self.assertEqual(observed, "run-token-selection-copy-command_timeout\n")
                    self.assertNotIn("SECRET", observed)
                    self.assertEqual(self.wait_for_result(diagnostics), "result=failed\nreason=command_timeout\n")
                    self.assertNotIn("SECRET", diagnostics.read_text())


class RunnerDiagnosticsTests(unittest.TestCase):
    @staticmethod
    def write_storage_success(root):
        (root / "ios-native-storage-validation.txt").write_text(
            "case=interrupted_write_cleanup result=passed\n"
            "case=credential_endpoint_binding result=passed\n"
            "case=remove_saved_credential result=passed\n"
            "case=preferences_validation result=passed\n"
        )

    def test_runner_failure_reports_only_fixed_flags_and_actual_exit_code(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw, output = root / "raw.log", root / "diagnostics.txt"
            raw.write_text(
                "Failed to launch test runner: private-key-path-SECRET\n"
                "Could not cast value of type 'PASSPHRASE-SECRET'\n"
                "-----BEGIN OPENSSH PRIVATE KEY-----\nKEY-CONTENT-SECRET\n"
                "exit_code=0\nTest Case '-[SecretClass SecretMethod]' started.\n"
            )
            smoke.write_xcuitest_diagnostics(raw, output, 65)
            report = output.read_text()
            self.assertIn("exit_code=65\n", report)
            self.assertIn("runner_launch_failed=1\n", report)
            self.assertIn("swift_cast_failed=1\n", report)
            self.assertIn("test_case_started=1\n", report)
            self.assertNotIn("SECRET", report)
            self.assertNotIn("SecretClass", report)
            self.assertNotIn("OPENSSH", report)
            self.assertTrue(all(
                line == "exit_code=65" or re.fullmatch(r"[a-z_]+=[01]", line)
                for line in report.splitlines()
            ))

    def test_missing_log_is_explicit_and_not_a_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "diagnostics.txt"
            smoke.write_xcuitest_diagnostics(root / "missing.log", output, None)
            self.assertIn("raw_log_available=0\n", output.read_text())
            self.assertIn("exit_code=unavailable\n", output.read_text())

    def test_timeout_keeps_original_failure_and_emits_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=subprocess.TimeoutExpired("xcodebuild", 1800)):
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                    )
            self.assertEqual(failure.exception.reason, "xcodebuild_timeout")
            self.assertEqual(failure.exception.stage, "xcuitest_storage")
            self.assertIn("exit_code=unavailable\n", (root / "ios-storage-xctest-runner-diagnostics.txt").read_text())

    def test_storage_finishes_before_ui_without_repeating_cases_or_extending_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            def successful_run(command, **kwargs):
                if "-only-testing:meetermStorageTests" in command:
                    self.write_storage_success(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.time, "monotonic", side_effect=[100, 105, 165]), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run) as run:
                status = smoke.run_xcuitest(
                    derived_data=root,
                    simulator_udid="fixture-simulator",
                    result_bundle=root / "result.xcresult",
                    raw_log=root / "raw.log",
                    diagnostics_path=root / "diagnostics.txt",
                )
            self.assertEqual(status, 0)
            self.assertEqual(run.call_count, 2)
            storage, ui = run.call_args_list
            self.assertIn("-only-testing:meetermStorageTests", storage.args[0])
            self.assertIn("-skip-testing:meetermStorageTests", ui.args[0])
            self.assertIn(str(root / "result-storage.xcresult"), storage.args[0])
            self.assertIn(str(root / "result.xcresult"), ui.args[0])
            self.assertEqual(storage.kwargs["timeout"], 1795)
            self.assertEqual(ui.kwargs["timeout"], 1735)
            self.assertNotEqual(storage.kwargs["stdout"].name, ui.kwargs["stdout"].name)
            self.assertIn("exit_code=0\n", (root / "ios-storage-xctest-runner-diagnostics.txt").read_text())
            self.assertIn("exit_code=0\n", (root / "diagnostics.txt").read_text())

    def test_storage_failure_prevents_ui_launch_and_keeps_its_diagnostic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", return_value=subprocess.CompletedProcess([], 65)) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                    )
            self.assertEqual(run.call_count, 1)
            self.assertEqual((failure.exception.stage, failure.exception.reason), ("xcuitest_storage", "storage_tests_failed"))
            self.assertIn("exit_code=65\n", (root / "ios-storage-xctest-runner-diagnostics.txt").read_text())
            self.assertFalse((root / "raw.log").exists())

    def test_zero_exit_without_fresh_storage_cases_cannot_start_ui(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            # A previous result must not turn a no-tests-selected run green.
            self.write_storage_success(root)
            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                    )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(failure.exception.reason, "storage_cases_incomplete")
            self.assertFalse((root / "raw.log").exists())


if __name__ == "__main__":
    unittest.main()
