#!/usr/bin/env python3
"""Launch the Android real-SSH smoke with API-36 foreground detection.

The smoke implementation stays in ``android_smoke_impl.py``.  This entrypoint
only aligns its foreground check with ``scripts/ci/android-smoke.sh``: prefer
WindowManager focus when present, then fall back to ActivityManager's resumed
activity when Android omits the window-focus fields.
"""

from __future__ import annotations

from pathlib import Path
import runpy


PACKAGE = "dev.meeterm.app"


def patch_foreground_check(module: dict[str, object]) -> None:
    AndroidDevice = module["AndroidDevice"]
    SmokeFailure = module["SmokeFailure"]

    def check_foreground(self, stage: str) -> None:
        window_output = self.run(
            # Android 11's `window windows` subcommand can omit both focus
            # summary fields even while the app is visibly foreground.  The
            # parent `window` dump retains the same concrete focus records and
            # is the stronger query on the physical Pixel 3.
            ("shell", "dumpsys", "window"),
            f"{stage}_foreground",
            timeout=10.0,
        ).decode("utf-8", errors="replace")
        if f"Application Not Responding: {PACKAGE}" in window_output:
            raise SmokeFailure(stage, "app_anr_window")

        current_focus_lines = [
            line for line in window_output.splitlines() if "mCurrentFocus" in line
        ]
        if current_focus_lines:
            if any(f"{PACKAGE}/" in line for line in current_focus_lines):
                return
            raise SmokeFailure(stage, "app_not_foreground")

        focused_app_lines = [
            line for line in window_output.splitlines() if "mFocusedApp" in line
        ]
        if focused_app_lines:
            if any(f"{PACKAGE}/" in line for line in focused_app_lines):
                return
            raise SmokeFailure(stage, "app_not_foreground")

        activity_output = self.run(
            ("shell", "dumpsys", "activity", "activities"),
            f"{stage}_foreground",
            timeout=10.0,
        ).decode("utf-8", errors="replace")
        resumed_activity_lines = [
            line
            for line in activity_output.splitlines()
            if "mResumedActivity" in line or "topResumedActivity" in line
        ]
        if resumed_activity_lines and any(
            f"{PACKAGE}/" in line for line in resumed_activity_lines
        ):
            return
        raise SmokeFailure(stage, "app_not_foreground")

    def assert_foreground(self, stage: str) -> None:
        try:
            check_foreground(self, stage)
        except SmokeFailure:
            # Preserve the implementation's evidence boundary when adding the
            # API-36 ActivityManager fallback. Returning later cannot make a
            # recording containing a detected interruption safe to retain.
            self.foreground_evidence_lost = True
            raise

    AndroidDevice.assert_foreground = assert_foreground


def main() -> int:
    implementation = Path(__file__).with_name("android_smoke_impl.py")
    module = runpy.run_path(
        str(implementation),
        run_name="meeterm_android_smoke_impl",
    )
    patch_foreground_check(module)
    return module["main"]()


if __name__ == "__main__":
    raise SystemExit(main())
