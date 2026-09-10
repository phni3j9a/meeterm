"""Exercise the actual launcher patch, including its evidence-loss latch."""

from pathlib import Path
import runpy
import unittest
from unittest import mock


class LauncherForegroundTests(unittest.TestCase):
    def setUp(self):
        directory = Path(__file__).parent
        self.module = runpy.run_path(str(directory / "android_smoke_impl.py"), run_name="foreground_probe")
        launcher = runpy.run_path(str(directory / "android-smoke.py"), run_name="launcher_probe")
        launcher["patch_foreground_check"](self.module)
        self.device = self.module["AndroidDevice"]("emulator-5554", "adb")
        self.failure = self.module["SmokeFailure"]

    def test_detected_interruption_remains_latched_after_return(self):
        self.device.run = mock.Mock(side_effect=[
            b"mCurrentFocus=Window{other.app/.Main}",
            b"mCurrentFocus=Window{dev.meeterm.app/.MainActivity}",
        ])
        with self.assertRaises(self.failure):
            self.device.assert_foreground("copy")
        self.device.assert_foreground("finish")
        self.assertTrue(self.device.foreground_evidence_lost)

    def test_anr_and_unknown_focus_latch_loss(self):
        for responses in ([b"Application Not Responding: dev.meeterm.app"], [b"", b""]):
            with self.subTest(responses=responses):
                self.device.foreground_evidence_lost = False
                self.device.run = mock.Mock(side_effect=responses)
                with self.assertRaises(self.failure):
                    self.device.assert_foreground("copy")
                self.assertTrue(self.device.foreground_evidence_lost)

    def test_both_window_and_activity_query_failures_latch_loss(self):
        for responses in ([self.failure("probe", "adb_failed")], [b"", self.failure("probe", "adb_failed")]):
            with self.subTest(query_count=len(responses)):
                self.device.foreground_evidence_lost = False
                self.device.run = mock.Mock(side_effect=responses)
                with self.assertRaises(self.failure):
                    self.device.assert_foreground("copy")
                self.assertTrue(self.device.foreground_evidence_lost)

    def test_activity_fallback_accepts_meeterm_without_latching_loss(self):
        self.device.run = mock.Mock(side_effect=[
            b"", b"topResumedActivity: ActivityRecord{dev.meeterm.app/.MainActivity}",
        ])
        self.device.assert_foreground("copy")
        self.assertFalse(self.device.foreground_evidence_lost)


if __name__ == "__main__":
    unittest.main()
