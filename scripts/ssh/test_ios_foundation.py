"""Regression tests for the sanitized iOS foundation validator."""

from datetime import datetime, timezone
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).parents[1] / "ci" / "ios-validate-foundation.py"


def compact_marker(epoch: float, pid: int, name: str) -> str:
    stamp = datetime.fromtimestamp(epoch, timezone.utc)
    return (
        f"{stamp:%Y-%m-%d %H:%M:%S}.{stamp.microsecond // 1000:03d} Df "
        f"meeterm[{pid}:42af0] (Foundation) {name}"
    )


class FoundationValidatorTests(unittest.TestCase):
    def run_case(
        self,
        lines: list[str],
        *,
        launch: float = 100.0,
        start: float = 101.0,
        end: float = 111.0,
        observation: str | None = None,
    ) -> tuple[subprocess.CompletedProcess[str], Path]:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        if observation is None:
            observation = (
                f'{{"launch_epoch": {launch}, "survival_start_epoch": {start}, '
                f'"survival_end_epoch": {end}}}\n'
            )
        (root / "ios-foundation-observation.json").write_text(observation, encoding="utf-8")
        (root / "simulator.log").write_text("\n".join(lines) + "\n", encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--artifact-dir", str(root)],
            capture_output=True,
            text=True,
            check=False,
        )
        return result, root

    def assert_reason(self, result: subprocess.CompletedProcess[str], root: Path, reason: str) -> None:
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stderr, f"iOS foundation validation failed: {reason}\n")
        self.assertEqual(
            (root / "ios-foundation-validation.txt").read_text(encoding="utf-8"),
            f"result=failed\nreason={reason}\nrenderer_backend=unavailable\n",
        )

    def test_accepts_metal_and_appends_fixed_backend_metadata(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ])
        self.assertEqual(result.returncode, 0)
        self.assertEqual((root / "ios-foundation-validation.txt").read_text(),
                         "result=passed\nreason=none\nrenderer_backend=metal\n")
        self.assertEqual((root / "metadata.txt").read_text(), "renderer_backend=metal\n")

    def test_accepts_software_fallback(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_SOFTWARE"),
        ])
        self.assertEqual(result.returncode, 0)
        self.assertIn("renderer_backend=software-simulator-fallback\n",
                      (root / "ios-foundation-validation.txt").read_text())

    def test_rejects_marker_after_required_survival_window(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(106.1, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ])
        self.assert_reason(result, root, "late_marker")

    def test_rejects_stale_marker(self) -> None:
        result, root = self.run_case([
            compact_marker(99.9, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ])
        self.assert_reason(result, root, "stale_marker")

    def test_rejects_short_foreground_observation(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ], end=105.9)
        self.assert_reason(result, root, "survival_too_short")

    def test_accepts_exactly_five_seconds(self) -> None:
        result, root = self.run_case([
            compact_marker(100.5, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(101.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ], end=106.0)
        self.assertEqual(result.returncode, 0)

    def test_rejects_missing_first_frame(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
        ])
        self.assert_reason(result, root, "missing_first_frame")

    def test_rejects_missing_native_ready(self) -> None:
        result, root = self.run_case([
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ])
        self.assert_reason(result, root, "missing_native_ready")

    def test_rejects_different_marker_pids(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 5678, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
        ])
        self.assert_reason(result, root, "ambiguous_pid")

    def test_rejects_ambiguous_renderer(self) -> None:
        result, root = self.run_case([
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
            compact_marker(104.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_SOFTWARE"),
        ])
        self.assert_reason(result, root, "ambiguous_renderer")

    def test_ignores_out_of_window_markers_when_fresh_pair_is_present(self) -> None:
        result, root = self.run_case([
            compact_marker(99.9, 999, "MEETERM_SMOKE_FIRST_FRAME_SOFTWARE"),
            compact_marker(102.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
            compact_marker(103.0, 1234, "MEETERM_SMOKE_FIRST_FRAME_METAL"),
            compact_marker(110.0, 1234, "MEETERM_SMOKE_NATIVE_READY"),
        ])
        self.assertEqual(result.returncode, 0)
        self.assertIn("renderer_backend=metal\n", (root / "metadata.txt").read_text())

    def test_rejects_malformed_marker_line(self) -> None:
        result, root = self.run_case([
            "not-a-timestamp Df meeterm[1234:42af0] (Foundation) "
            "MEETERM_SMOKE_NATIVE_READY",
        ])
        self.assert_reason(result, root, "malformed_marker")

    def test_rejects_malformed_observation(self) -> None:
        result, root = self.run_case([], observation='{"launch_epoch": 1')
        self.assert_reason(result, root, "malformed_observation")

    def test_rejects_nonfinite_observation(self) -> None:
        result, root = self.run_case([], observation=(
            '{"launch_epoch": 1e309, "survival_start_epoch": 2, '
            '"survival_end_epoch": 8}'
        ))
        self.assert_reason(result, root, "invalid_timestamps")


if __name__ == "__main__":
    unittest.main()
