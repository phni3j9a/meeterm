"""Privacy and failure-path checks for the iOS XCTest runner diagnostics."""

import contextlib
import io
import importlib.util
import json
import plistlib
from pathlib import Path
import re
import subprocess
import sys
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

    @staticmethod
    def write_native_success(root):
        (root / "ios-native-input-validation.txt").write_text(
            "case=multiline result=passed\n"
            "case=rebind result=passed\n"
            "case=unmount result=passed\n"
            "case=control_one_shot result=passed\n"
            "case=hardware_control result=passed\n"
            "case=hardware_shift_combinations result=passed\n"
            "case=marked_commit result=passed\n"
        )

    @staticmethod
    def write_forms_success(root):
        (root / "ios-ui-forms-validation.txt").write_text("case=forms result=passed\n")

    @staticmethod
    def write_names_success(root):
        (root / "ios-ui-names-validation.txt").write_text("case=names result=passed\n")

    @staticmethod
    def write_standard_success(root):
        RunnerDiagnosticsTests.write_native_success(root)
        (root / "ios-ui-standard-validation.txt").write_text(
            "case=standard result=passed\n"
        )

    @staticmethod
    def write_standard_stages(root):
        (root / "ios-ui-stages.txt").write_text(
            "standard_complete\nfoundation_verified\n"
        )

    @staticmethod
    def write_ssh_success(root):
        (root / "ios-ui-ssh-validation.txt").write_text("case=ssh result=passed\n")

    @staticmethod
    def write_ssh_stages(root):
        (root / "ios-ui-stages.txt").write_text("ssh_complete\n")

    @staticmethod
    def write_full_stages(root):
        (root / "ios-ui-stages.txt").write_text("daily_complete\nfoundation_verified\n")

    @staticmethod
    def write_names_stages(root):
        (root / "ios-ui-stages.txt").write_text("names_complete\n")

    def test_standard_runs_storage_then_seeded_ui_and_all_native_input_cases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            standard_validation = root / "ios-ui-standard-validation.txt"
            native_validation = root / "ios-native-input-validation.txt"
            standard_validation.write_text("case=standard result=passed\n")
            native_validation.write_text("case=stale result=passed\n")

            def successful_run(command, **kwargs):
                if smoke.STORAGE_TEST_SELECTOR in command:
                    self.write_storage_success(root)
                if smoke.STANDARD_TEST_SELECTOR in command:
                    self.assertFalse(standard_validation.exists(), "the UI marker must be fresh")
                    self.assertFalse(native_validation.exists(), "the input marker must be fresh")
                    self.write_standard_success(root)
                    self.write_standard_stages(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run) as run, \
                 mock.patch.object(smoke.time, "monotonic", side_effect=[100.0, 100.25, 100.5]):
                status = smoke.run_xcuitest(
                    derived_data=root,
                    simulator_udid="fixture-simulator",
                    result_bundle=root / "result.xcresult",
                    raw_log=root / "raw.log",
                    diagnostics_path=root / "diagnostics.txt",
                    suite="standard",
                )

            self.assertEqual(status, 0)
            self.assertEqual(run.call_count, 2)
            storage, ui = run.call_args_list
            self.assertIn(smoke.STORAGE_TEST_SELECTOR, storage.args[0])
            self.assertIn(smoke.STANDARD_TEST_SELECTOR, ui.args[0])
            self.assertIn(smoke.NATIVE_TEST_SELECTOR, ui.args[0])
            self.assertNotIn(smoke.FULL_TEST_SELECTOR, ui.args[0])
            self.assertEqual(storage.kwargs["timeout"], 899.75)
            self.assertEqual(ui.kwargs["timeout"], 899.5)
            self.assertEqual(
                native_validation.read_text(),
                "".join(f"case={case} result=passed\n" for case in smoke.NATIVE_INPUT_CASES),
            )
            self.assertEqual(standard_validation.read_text(), "case=standard result=passed\n")
            self.assertEqual(
                (root / "ios-ui-stages.txt").read_text(),
                "standard_complete\nfoundation_verified\n",
            )

    def test_standard_missing_native_input_case_cannot_pass_with_ui_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def incomplete_run(command, **kwargs):
                if smoke.STORAGE_TEST_SELECTOR in command:
                    self.write_storage_success(root)
                if smoke.STANDARD_TEST_SELECTOR in command:
                    (root / "ios-native-input-validation.txt").write_text(
                        "".join(
                            f"case={case} result=passed\n"
                            for case in smoke.NATIVE_INPUT_CASES[:-1]
                        )
                    )
                    (root / "ios-ui-standard-validation.txt").write_text(
                        "case=standard result=passed\n"
                    )
                    self.write_standard_stages(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=incomplete_run):
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                        suite="standard",
                    )

            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_standard", "native_cases_incomplete"),
            )

    def test_ssh_runs_only_short_selector_with_fresh_stage_and_ui_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            ssh_validation = root / "ios-ui-ssh-validation.txt"
            ssh_validation.write_text("case=ssh result=passed\n")

            def successful_run(command, **kwargs):
                self.assertFalse(ssh_validation.exists(), "the SSH marker must be fresh")
                self.write_ssh_success(root)
                self.write_ssh_stages(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run) as run, \
                 mock.patch.object(smoke.time, "monotonic", side_effect=[100.0, 100.25]):
                status = smoke.run_xcuitest(
                    derived_data=root,
                    simulator_udid="fixture-simulator",
                    result_bundle=root / "result.xcresult",
                    raw_log=root / "raw.log",
                    diagnostics_path=root / "diagnostics.txt",
                    suite="ssh",
                )

            self.assertEqual(status, 0)
            self.assertEqual(run.call_count, 1)
            command = run.call_args.args[0]
            self.assertIn(smoke.SSH_TEST_SELECTOR, command)
            for selector in (
                smoke.STORAGE_TEST_SELECTOR,
                smoke.NATIVE_TEST_SELECTOR,
                smoke.FULL_TEST_SELECTOR,
                smoke.FORMS_TEST_SELECTOR,
                smoke.NAMES_TEST_SELECTOR,
            ):
                self.assertNotIn(selector, command)
            self.assertEqual(run.call_args.kwargs["timeout"], 899.75)
            self.assertEqual(ssh_validation.read_text(), "case=ssh result=passed\n")
            self.assertEqual((root / "ios-ui-stages.txt").read_text(), "ssh_complete\n")

    def test_ssh_timeout_stays_failed_even_with_fresh_marker_and_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def timed_out_run(command, **kwargs):
                self.write_ssh_success(root)
                self.write_ssh_stages(root)
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=timed_out_run) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                        suite="ssh",
                    )

            self.assertEqual(run.call_count, 1)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_ssh", "xcodebuild_timeout"),
            )
            self.assertIn("xcodebuild_timeout_ms=", (root / "diagnostics.txt").read_text())

    def test_ssh_injects_fixture_and_marker_allowlist_without_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            xctestrun = root / "fixture.xctestrun"
            xctestrun.write_bytes(plistlib.dumps({
                "Tests": {
                    "TestBundlePath": "meetermTests.xctest",
                    "EnvironmentVariables": {
                        "MEETERM_SSH_HOST": "stale-fixture-host",
                        "UNRELATED_TEST_FLAG": "preserved",
                    },
                },
            }))
            fixture_environment = {
                name: f"allowed-{name}"
                for name in smoke.SSH_TEST_ENVIRONMENT_NAMES
            }
            inherited_environment = {
                "MEETERM_SSH_PRIVATE_KEY_FILE": "private-key-secret",
                "MEETERM_SSH_PASSPHRASE": "passphrase-secret",
                "MEETERM_SSH_KNOWN_HOSTS_FILE": "known-hosts-secret",
                "MEETERM_IOS_HANDOFF_VALUE": "handoff-secret",
                "MEETERM_SSH_EXTRA_SENTINEL": "extra-secret",
            }
            with mock.patch.dict(
                smoke.os.environ,
                {**fixture_environment, **inherited_environment},
                clear=False,
            ):
                smoke.inject_test_environment(xctestrun, suite="ssh")
            document = plistlib.loads(xctestrun.read_bytes())
            environment = document["Tests"]["EnvironmentVariables"]
            for name, value in fixture_environment.items():
                self.assertEqual(environment[name], value)
            for name in inherited_environment:
                self.assertNotIn(name, environment)
            self.assertEqual(environment["UNRELATED_TEST_FLAG"], "preserved")

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
                if smoke.FULL_TEST_SELECTOR in command:
                    self.write_native_success(root)
                    self.write_full_stages(root)
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
            self.assertIn(smoke.FULL_TEST_SELECTOR, ui.args[0])
            self.assertIn(smoke.NATIVE_TEST_SELECTOR, ui.args[0])
            self.assertNotIn(smoke.FORMS_TEST_SELECTOR, ui.args[0])
            self.assertNotIn("-skip-testing:meetermStorageTests", ui.args[0])
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

    def test_forms_runs_only_public_form_selector_without_fixture_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def successful_run(command, **kwargs):
                self.write_forms_success(root)
                (root / "ios-ui-stages.txt").write_text("forms_complete\n")
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run) as run, \
                 mock.patch.dict(smoke.os.environ, {
                     "MEETERM_SSH_HOST": "fixture-secret-host",
                     "MEETERM_SSH_PRIVATE_KEY_FILE": "fixture-secret-key",
                     "MEETERM_SSH_EXTRA_SENTINEL": "fixture-secret-sentinel",
                 }, clear=False):
                monotonic = [100.0, 100.25]
                with mock.patch.object(smoke.time, "monotonic", side_effect=monotonic):
                    status = smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                        suite="forms",
                    )
            self.assertEqual(status, 0)
            self.assertEqual(run.call_count, 1)
            command = run.call_args.args[0]
            self.assertIn(smoke.FORMS_TEST_SELECTOR, command)
            self.assertNotIn(smoke.STORAGE_TEST_SELECTOR, command)
            self.assertNotIn(smoke.FULL_TEST_SELECTOR, command)
            environment = run.call_args.kwargs["env"]
            self.assertNotIn("MEETERM_SSH_HOST", environment)
            self.assertNotIn("MEETERM_SSH_PRIVATE_KEY_FILE", environment)
            self.assertNotIn("MEETERM_SSH_EXTRA_SENTINEL", environment)
            self.assertEqual(run.call_args.kwargs["timeout"], 899.75)
            self.assertTrue(list(products.glob(".meeterm-*.xctestrun")) == [])

    def test_forms_timeout_stays_failed_even_with_fresh_completion_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def timed_out_run(command, **kwargs):
                self.write_forms_success(root)
                (root / "ios-ui-stages.txt").write_text("forms_complete\n")
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=timed_out_run) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                        suite="forms",
                    )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_forms", "xcodebuild_timeout"),
            )
            self.assertEqual(
                (root / "ios-ui-forms-validation.txt").read_text(),
                "case=forms result=passed\n",
            )
            self.assertIn("xcodebuild_start_epoch_ms=", (root / "ios-forms-xctest-runner-diagnostics.txt").read_text())
            self.assertIn("xcodebuild_timeout_ms=", (root / "ios-forms-xctest-runner-diagnostics.txt").read_text())

    def test_names_runs_only_ssh_name_selector_with_fresh_marker_and_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            names_validation = root / "ios-ui-names-validation.txt"
            names_validation.write_text("case=names result=passed\n")

            def successful_run(command, **kwargs):
                self.assertFalse(names_validation.exists(), "the names marker must be fresh")
                self.write_names_success(root)
                self.write_names_stages(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run) as run, \
                 mock.patch.object(smoke.time, "monotonic", side_effect=[100.0, 100.25]):
                status = smoke.run_xcuitest(
                    derived_data=root,
                    simulator_udid="fixture-simulator",
                    result_bundle=root / "result.xcresult",
                    raw_log=root / "raw.log",
                    diagnostics_path=root / "diagnostics.txt",
                    suite="names",
                )
            self.assertEqual(status, 0)
            self.assertEqual(run.call_count, 1)
            command = run.call_args.args[0]
            self.assertIn(smoke.NAMES_TEST_SELECTOR, command)
            self.assertNotIn(smoke.STORAGE_TEST_SELECTOR, command)
            self.assertNotIn(smoke.NATIVE_TEST_SELECTOR, command)
            self.assertNotIn(smoke.FULL_TEST_SELECTOR, command)
            self.assertNotIn(smoke.FORMS_TEST_SELECTOR, command)
            self.assertEqual(run.call_args.kwargs["timeout"], 899.75)
            self.assertEqual(names_validation.read_text(), "case=names result=passed\n")
            self.assertEqual((root / "ios-ui-stages.txt").read_text(), "names_complete\n")

    def test_names_zero_exit_without_fresh_marker_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            # A prior marker and completion stage must not satisfy this run.
            (root / "ios-ui-names-validation.txt").write_text("case=names result=passed\n")
            self.write_names_stages(root)
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
                        suite="names",
                    )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_names", "names_cases_incomplete"),
            )
            self.assertFalse((root / "ios-ui-names-validation.txt").exists())

    def test_names_timeout_stays_failed_even_with_fresh_marker_and_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def timed_out_run(command, **kwargs):
                self.write_names_success(root)
                self.write_names_stages(root)
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=timed_out_run) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                        suite="names",
                    )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_names", "xcodebuild_timeout"),
            )
            diagnostics = root / "ios-names-xctest-runner-diagnostics.txt"
            self.assertIn("xcodebuild_start_epoch_ms=", diagnostics.read_text())
            self.assertIn("xcodebuild_timeout_ms=", diagnostics.read_text())

    def test_names_injects_fixture_allowlist_without_inheriting_extra_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            xctestrun = root / "fixture.xctestrun"
            xctestrun.write_bytes(plistlib.dumps({
                "Tests": {
                    "TestBundlePath": "meetermTests.xctest",
                    "EnvironmentVariables": {
                        "MEETERM_SSH_HOST": "stale-fixture-host",
                        "UNRELATED_TEST_FLAG": "preserved",
                    },
                },
            }))
            fixture_environment = {
                name: f"allowed-{name}"
                for name in smoke.NAMES_TEST_ENVIRONMENT_NAMES
            }
            inherited_environment = {
                "MEETERM_SSH_PRIVATE_KEY_FILE": "private-key-secret",
                "MEETERM_SSH_PASSPHRASE": "passphrase-secret",
                "MEETERM_SSH_KNOWN_HOSTS_FILE": "known-hosts-secret",
                "MEETERM_IOS_HANDOFF_VALUE": "handoff-secret",
                "MEETERM_SSH_EXTRA_SENTINEL": "extra-secret",
            }
            with mock.patch.dict(
                smoke.os.environ,
                {**fixture_environment, **inherited_environment},
                clear=False,
            ):
                smoke.inject_test_environment(xctestrun, suite="names")
            document = plistlib.loads(xctestrun.read_bytes())
            environment = document["Tests"]["EnvironmentVariables"]
            for name, value in fixture_environment.items():
                self.assertEqual(environment[name], value)
            for name in inherited_environment:
                self.assertNotIn(name, environment)
            self.assertEqual(environment["UNRELATED_TEST_FLAG"], "preserved")

    def test_native_runs_storage_then_input_and_requires_both_case_sets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def successful_run(command, **kwargs):
                if smoke.STORAGE_TEST_SELECTOR in command:
                    self.write_storage_success(root)
                if smoke.NATIVE_TEST_SELECTOR in command:
                    self.write_native_success(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run) as run, \
                 mock.patch.object(smoke.time, "monotonic", side_effect=[100.0, 100.25, 100.5]):
                status = smoke.run_xcuitest(
                    derived_data=root,
                    simulator_udid="fixture-simulator",
                    result_bundle=root / "result.xcresult",
                    raw_log=root / "raw.log",
                    diagnostics_path=root / "diagnostics.txt",
                    suite="native",
                )
            self.assertEqual(status, 0)
            self.assertEqual(run.call_count, 2)
            self.assertIn(smoke.STORAGE_TEST_SELECTOR, run.call_args_list[0].args[0])
            self.assertIn(smoke.NATIVE_TEST_SELECTOR, run.call_args_list[1].args[0])
            self.assertNotIn(smoke.FULL_TEST_SELECTOR, run.call_args_list[1].args[0])
            self.assertEqual(run.call_args_list[0].kwargs["timeout"], 599.75)
            self.assertEqual(run.call_args_list[1].kwargs["timeout"], 599.5)

    def test_forms_zero_exit_without_fresh_completion_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()
            # A prior focused result and stage must not satisfy this run.
            self.write_forms_success(root)
            (root / "ios-ui-stages.txt").write_text("forms_complete\n")
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
                        suite="forms",
                    )
            self.assertEqual(run.call_count, 1)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_forms", "forms_cases_incomplete"),
            )

    def test_native_storage_success_without_all_input_cases_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def incomplete_run(command, **kwargs):
                if smoke.STORAGE_TEST_SELECTOR in command:
                    self.write_storage_success(root)
                if smoke.NATIVE_TEST_SELECTOR in command:
                    self.write_native_success(root)
                    validation = root / "ios-native-input-validation.txt"
                    validation.write_text(
                        "\n".join(
                            line
                            for line in validation.read_text().splitlines()
                            if "case=marked_commit" not in line
                        )
                        + "\n"
                    )
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=incomplete_run) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                        suite="native",
                    )
            self.assertEqual(run.call_count, 2)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest_native", "native_cases_incomplete"),
            )

    def test_full_ui_success_without_fresh_input_cases_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            (products / "fixture.xctestrun").touch()

            def missing_native_run(command, **kwargs):
                if smoke.STORAGE_TEST_SELECTOR in command:
                    self.write_storage_success(root)
                if smoke.FULL_TEST_SELECTOR in command:
                    # Stale completion markers must not make a no-input run pass.
                    self.write_full_stages(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke, "inject_test_environment"), \
                 mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=missing_native_run) as run:
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                    )
            self.assertEqual(run.call_count, 2)
            self.assertEqual(
                (failure.exception.stage, failure.exception.reason),
                ("xcuitest", "native_cases_incomplete"),
            )

    def test_run_does_not_mutate_pristine_xctestrun_or_leave_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            products = root / "Build" / "Products"
            products.mkdir(parents=True)
            source = products / "fixture.xctestrun"
            source.write_bytes(plistlib.dumps({
                "Tests": {
                    "TestBundlePath": str(products / "meetermTests.xctest"),
                    "EnvironmentVariables": {
                        "MEETERM_SSH_HOST": "stale-fixture-host",
                        "UNRELATED_TEST_FLAG": "preserved",
                    },
                },
            }))
            pristine = source.read_bytes()

            def successful_run(command, **kwargs):
                if smoke.STORAGE_TEST_SELECTOR in command:
                    self.write_storage_success(root)
                if smoke.NATIVE_TEST_SELECTOR in command:
                    self.write_native_success(root)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(smoke.shutil, "which", return_value="/bin/xcodebuild"), \
                 mock.patch.object(smoke.subprocess, "run", side_effect=successful_run):
                status = smoke.run_xcuitest(
                    derived_data=root,
                    simulator_udid="fixture-simulator",
                    result_bundle=root / "result.xcresult",
                    raw_log=root / "raw.log",
                    diagnostics_path=root / "diagnostics.txt",
                    suite="native",
                )
            self.assertEqual(status, 0)
            self.assertEqual(source.read_bytes(), pristine)
            self.assertEqual(list(products.glob(".meeterm-*.xctestrun")), [])

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


class ConnectionFailureDiagnosticsTests(unittest.TestCase):
    @staticmethod
    def fixture_environment(root: Path, *, suite: str) -> dict[str, str]:
        host = "fixture-host-SECRET"
        username = "fixture-user-SECRET"
        client_key = root / "private-key-SECRET"
        host_key = root / "host-key-SECRET.pub"
        client_key.write_text("PRIVATE-KEY-SECRET\n", encoding="utf-8")
        host_key.write_text(
            "ssh-ed25519 AAAA-HOST-KEY-SECRET fixture-host\n", encoding="utf-8"
        )
        return {
            "MEETERM_SSH_HOST": host,
            "MEETERM_SSH_PORT": "43210",
            "MEETERM_SSH_USERNAME": username,
            "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE": str(client_key),
            "MEETERM_SSH_HOST_KEY_FILE": str(host_key),
            "MEETERM_IOS_SUITE": suite,
        }

    @staticmethod
    def write_metadata(container: Path, document: object) -> None:
        metadata = container / "Library" / "Application Support" / "meeterm" / "client-v1.json"
        metadata.parent.mkdir(parents=True, exist_ok=True)
        metadata.write_text(json.dumps(document), encoding="utf-8")

    @staticmethod
    def expected_profile(environment: dict[str, str], *, suite: str) -> dict[str, object]:
        profile = {
            "id": "PROFILE-ID-SECRET",
            "name": environment["MEETERM_SSH_HOST"] if suite == "names" else "Daily fixture",
            "host": environment["MEETERM_SSH_HOST"],
            "port": int(environment["MEETERM_SSH_PORT"]),
            "username": environment["MEETERM_SSH_USERNAME"],
            "authMethod": "publicKey",
        }
        if suite == "full":
            profile["credentialID"] = "CREDENTIAL-ID-SECRET"
        return profile

    def run_diagnostic(
        self,
        root: Path,
        environment: dict[str, str],
        container: Path | None,
        *,
        ssh_outcome: str = "passed",
        xcrun_outcome: str = "passed",
    ) -> tuple[str, list[tuple[list[str], dict[str, object]]], list[str]]:
        calls: list[tuple[list[str], dict[str, object]]] = []
        known_hosts_contents: list[str] = []

        def which(name: str) -> str | None:
            if name == "ssh":
                return "/usr/bin/ssh"
            if name == "xcrun" and xcrun_outcome != "unavailable":
                return "/usr/bin/xcrun"
            return None

        def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
            calls.append((command, kwargs))
            if command[:3] == ["/usr/bin/xcrun", "simctl", "get_app_container"]:
                if xcrun_outcome == "timeout":
                    raise subprocess.TimeoutExpired(command, 10, output=b"CONTAINER-SECRET")
                if xcrun_outcome == "failed":
                    return subprocess.CompletedProcess(command, 1, stdout="", stderr="SIM-SECRET")
                assert container is not None
                return subprocess.CompletedProcess(command, 0, stdout=f"{container}\n", stderr="SIM-SECRET")
            self.assertEqual(command[0], "/usr/bin/ssh")
            known_hosts_option = next(
                value for value in command if value.startswith("UserKnownHostsFile=")
            )
            known_hosts_contents.append(
                Path(known_hosts_option.split("=", 1)[1]).read_text(encoding="utf-8")
            )
            if ssh_outcome == "timeout":
                raise subprocess.TimeoutExpired(command, 15, output=b"PRIVATE-KEY-SECRET")
            if ssh_outcome == "failed":
                return subprocess.CompletedProcess(command, 255, stdout="", stderr="SSH-SECRET")
            return subprocess.CompletedProcess(
                command,
                0,
                stdout=f"{smoke.SSH_PROBE_NONCE}\n",
                stderr="SSH-SECRET",
            )

        with mock.patch.dict(smoke.os.environ, environment, clear=False), \
             mock.patch.object(smoke.shutil, "which", side_effect=which), \
             mock.patch.object(smoke.subprocess, "run", side_effect=run):
            smoke.write_connection_failure_diagnostics(
                root,
                "fixture-simulator",
                suite=environment["MEETERM_IOS_SUITE"],
            )
        report_path = root / smoke.CONNECTION_FAILURE_DIAGNOSTICS_NAME
        return report_path.read_text(encoding="utf-8"), calls, known_hosts_contents

    def test_names_success_is_fixed_and_preserves_ui_results(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="names")
            container = root / "simulator-data"
            self.write_metadata(
                container,
                {"version": 1, "profiles": [self.expected_profile(environment, suite="names")]},
            )
            stages = root / "ios-ui-stages.txt"
            validation = root / "ios-names-validation.txt"
            stages.write_text("host_trust_dismissed\nconnected_timeout_after_host_trust\n", encoding="utf-8")
            validation.write_text("case=names result=failed\n", encoding="utf-8")
            report, calls, known_hosts = self.run_diagnostic(root, environment, container)

            self.assertEqual(
                report,
                "\n".join(
                    [
                        "suite=names",
                        "metadata_container=passed",
                        "metadata_file=present",
                        "metadata_json=valid",
                        "metadata_profile_count=1",
                        "metadata_profile_count_match=1",
                        "metadata_profile_host_match=1",
                        "metadata_profile_port_match=1",
                        "metadata_profile_username_match=1",
                        "metadata_profile_name_match=1",
                        "metadata_profile_auth_method_match=1",
                        "metadata_profile_credential_saved_match=1",
                        "metadata_profile_match=1",
                        "ssh_probe=passed",
                        "",
                    ]
                ),
            )
            for secret in (
                "fixture-host-SECRET",
                "fixture-user-SECRET",
                "private-key-SECRET",
                "HOST-KEY-SECRET",
                "PROFILE-ID-SECRET",
                "PRIVATE-KEY-SECRET",
                "SSH-SECRET",
            ):
                self.assertNotIn(secret, report)
            self.assertEqual(stages.read_text(encoding="utf-8"), "host_trust_dismissed\nconnected_timeout_after_host_trust\n")
            self.assertEqual(validation.read_text(encoding="utf-8"), "case=names result=failed\n")
            self.assertEqual(len(calls), 2)
            ssh_command, ssh_kwargs = calls[1]
            self.assertIn("StrictHostKeyChecking=yes", ssh_command)
            self.assertIn("GlobalKnownHostsFile=/dev/null", ssh_command)
            self.assertIn("IdentitiesOnly=yes", ssh_command)
            self.assertIn("IdentityAgent=none", ssh_command)
            self.assertIn(environment["MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE"], ssh_command)
            self.assertEqual(ssh_command[-1], "tmux -V >/dev/null 2>&1 && printf '%s\\n' 'meeterm-ios-ssh-probe-v1'")
            self.assertEqual(ssh_kwargs["timeout"], 15)
            self.assertEqual(
                known_hosts,
                [
                    f"[{environment['MEETERM_SSH_HOST']}]:43210 "
                    "ssh-ed25519 AAAA-HOST-KEY-SECRET fixture-host\n"
                ],
            )
            self.assertFalse(any(
                value.startswith("UserKnownHostsFile=")
                and Path(value.split("=", 1)[1]).exists()
                for value in ssh_command
            ))

    def test_full_profile_requires_saved_credential_id_and_expected_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="full")
            container = root / "simulator-data"
            self.write_metadata(
                container,
                {"version": 1, "profiles": [self.expected_profile(environment, suite="full")]},
            )
            report, _, _ = self.run_diagnostic(root, environment, container)
            self.assertIn("metadata_profile_name_match=1\n", report)
            self.assertIn("metadata_profile_credential_saved_match=1\n", report)
            self.assertIn("metadata_profile_match=1\n", report)
            self.assertIn("ssh_probe=passed\n", report)
            self.assertNotIn("CREDENTIAL-ID-SECRET", report)

    def test_mismatched_fields_and_multiple_profiles_cannot_be_combined(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="names")
            container = root / "simulator-data"
            first = self.expected_profile(environment, suite="names")
            second = {**first, "host": "other-host-SECRET", "id": "OTHER-ID-SECRET"}
            self.write_metadata(container, {"version": 1, "profiles": [first, second]})
            report, _, _ = self.run_diagnostic(root, environment, container)
            self.assertIn("metadata_profile_count=2\n", report)
            self.assertIn("metadata_profile_count_match=0\n", report)
            self.assertIn("metadata_profile_host_match=0\n", report)
            self.assertIn("metadata_profile_port_match=0\n", report)
            self.assertIn("metadata_profile_username_match=0\n", report)
            self.assertIn("metadata_profile_name_match=0\n", report)
            self.assertIn("metadata_profile_auth_method_match=0\n", report)
            self.assertIn("metadata_profile_credential_saved_match=0\n", report)
            self.assertIn("metadata_profile_match=0\n", report)
            self.assertNotIn("other-host-SECRET", report)
            self.assertNotIn("OTHER-ID-SECRET", report)

    def test_unavailable_and_corrupt_metadata_are_explicit_without_raw_values(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="names")
            report, _, _ = self.run_diagnostic(
                root,
                environment,
                None,
                xcrun_outcome="unavailable",
            )
            self.assertIn("metadata_container=unavailable\n", report)
            self.assertIn("metadata_file=unavailable\n", report)
            self.assertIn("metadata_json=unavailable\n", report)
            self.assertIn("ssh_probe=passed\n", report)

            container = root / "corrupt-simulator-data"
            metadata = container / "Library" / "Application Support" / "meeterm" / "client-v1.json"
            metadata.parent.mkdir(parents=True, exist_ok=True)
            metadata.write_bytes(b"BROKEN-METADATA-SECRET")
            report, _, _ = self.run_diagnostic(root, environment, container)
            self.assertIn("metadata_container=passed\n", report)
            self.assertIn("metadata_file=present\n", report)
            self.assertIn("metadata_json=invalid\n", report)
            self.assertIn("metadata_profile_count=unavailable\n", report)
            self.assertIn("metadata_profile_match=0\n", report)
            self.assertNotIn("BROKEN-METADATA-SECRET", report)

    def test_probe_timeout_does_not_change_metadata_or_emit_probe_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="names")
            container = root / "simulator-data"
            self.write_metadata(
                container,
                {"version": 1, "profiles": [self.expected_profile(environment, suite="names")]},
            )
            report, _, _ = self.run_diagnostic(
                root,
                environment,
                container,
                ssh_outcome="timeout",
            )
            self.assertIn("metadata_profile_match=1\n", report)
            self.assertIn("ssh_probe=timeout\n", report)
            self.assertNotIn("PRIVATE-KEY-SECRET", report)

    def test_main_keeps_original_names_failure_when_diagnostics_raise(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact_dir = root / "artifacts"
            artifact_dir.mkdir()
            stale_diagnostic = artifact_dir / smoke.CONNECTION_FAILURE_DIAGNOSTICS_NAME
            stale_diagnostic.write_text("OLD-DIAGNOSTIC-SECRET\n", encoding="utf-8")
            environment = self.fixture_environment(root, suite="names")
            environment.update(
                {
                    "MEETERM_SSH_FINGERPRINT": "SHA256:fixture-fingerprint",
                    "MEETERM_TMUX_SOCKET": str(root / "fixture" / "tmux" / "tmux-0" / "default"),
                    "RUNNER_TEMP": str(root / "runner"),
                }
            )

            with mock.patch.dict(smoke.os.environ, environment, clear=False), \
                 mock.patch.object(sys, "argv", [
                     "ios-smoke.py",
                     "--artifact-dir",
                     str(artifact_dir),
                     "--derived-data",
                     str(root / "derived-data"),
                     "--simulator-udid",
                     "fixture-simulator",
                     "--suite",
                     "names",
                 ]), \
                 mock.patch.object(smoke, "fixture_socket", return_value=Path("fixture-socket")), \
                 mock.patch.object(smoke, "prepare_topology", return_value=(2, 3)), \
                 mock.patch.object(smoke, "record_daily_interactions", return_value=contextlib.nullcontext()), \
                 mock.patch.object(
                     smoke,
                     "run_xcuitest",
                     side_effect=smoke.SmokeFailure("xcuitest_names", "names_tests_failed"),
                 ), \
                 mock.patch.object(smoke, "last_ui_stage", return_value="connected_timeout_after_host_trust"), \
                 mock.patch.object(
                     smoke,
                     "write_connection_failure_diagnostics",
                     side_effect=RuntimeError("DIAGNOSTIC-SECRET"),
                 ) as write_diagnostics, \
                 contextlib.redirect_stderr(io.StringIO()) as stderr:
                status = smoke.main()

            self.assertEqual(status, 1)
            validation = artifact_dir / "ios-names-validation.txt"
            contents = validation.read_text(encoding="utf-8")
            self.assertIn("result=failed\n", contents)
            self.assertIn("stage=xcuitest_names\n", contents)
            self.assertIn("reason=names_tests_failed\n", contents)
            self.assertIn("ui_last_stage=connected_timeout_after_host_trust\n", contents)
            self.assertNotIn("DIAGNOSTIC-SECRET", contents)
            self.assertNotIn("DIAGNOSTIC-SECRET", stderr.getvalue())
            self.assertNotIn("OLD-DIAGNOSTIC-SECRET", contents)
            self.assertFalse(stale_diagnostic.exists())
            write_diagnostics.assert_called_once_with(
                artifact_dir,
                "fixture-simulator",
                suite="names",
            )

    def test_unexpected_metadata_error_becomes_fixed_unavailable_diagnostic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="names")
            with mock.patch.object(smoke, "_profile_metadata_lines", side_effect=RuntimeError("METADATA-SECRET")):
                report, _, _ = self.run_diagnostic(root, environment, root / "unused")
            self.assertIn("metadata_container=unavailable\n", report)
            self.assertIn("metadata_json=unavailable\n", report)
            self.assertIn("metadata_profile_match=0\n", report)
            self.assertIn("ssh_probe=passed\n", report)
            self.assertNotIn("METADATA-SECRET", report)

    def test_non_fixture_suite_never_writes_connection_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = self.fixture_environment(root, suite="forms")
            with mock.patch.dict(smoke.os.environ, environment, clear=False):
                smoke.write_connection_failure_diagnostics(root, "fixture-simulator", suite="forms")
            self.assertFalse((root / smoke.CONNECTION_FAILURE_DIAGNOSTICS_NAME).exists())


if __name__ == "__main__":
    unittest.main()
