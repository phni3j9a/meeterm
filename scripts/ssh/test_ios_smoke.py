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


class RunnerDiagnosticsTests(unittest.TestCase):
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
                 mock.patch.object(smoke.subprocess, "run", side_effect=subprocess.TimeoutExpired("xcodebuild", 900)):
                with self.assertRaises(smoke.SmokeFailure) as failure:
                    smoke.run_xcuitest(
                        derived_data=root,
                        simulator_udid="fixture-simulator",
                        result_bundle=root / "result.xcresult",
                        raw_log=root / "raw.log",
                        diagnostics_path=root / "diagnostics.txt",
                    )
            self.assertEqual(failure.exception.reason, "xcodebuild_timeout")
            self.assertIn("exit_code=unavailable\n", (root / "diagnostics.txt").read_text())


if __name__ == "__main__":
    unittest.main()
