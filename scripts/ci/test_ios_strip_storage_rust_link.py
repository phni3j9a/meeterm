"""Checks the disposable iOS storage target linker isolation helper."""

from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HELPER = Path(__file__).with_name("ios-strip-storage-rust-link.py")
STORAGE_DIRECTORY = Path(
    "ios/Pods/Target Support Files/Pods-meetermStorageTests"
)
APP_CONFIG = Path(
    "ios/Pods/Target Support Files/Pods-meeterm/Pods-meeterm.release.xcconfig"
)
STANDARD_CONFIG = (
    "CLANG_WARN_QUOTED_INCLUDE_IN_FRAMEWORK_HEADER = NO\n"
    'OTHER_LDFLAGS = $(inherited) -ObjC -l"c++" -l"meeterm_core" '
    '-framework "Security"\n'
    "PODS_ROOT = ${SRCROOT}/Pods\n"
)


class IOSStorageRustLinkIsolationTests(unittest.TestCase):
    def write_storage_configs(
        self,
        root: Path,
        *,
        debug: str = STANDARD_CONFIG,
        release: str = STANDARD_CONFIG,
    ) -> tuple[Path, Path]:
        directory = root / STORAGE_DIRECTORY
        directory.mkdir(parents=True)
        debug_path = directory / "Pods-meetermStorageTests.debug.xcconfig"
        release_path = directory / "Pods-meetermStorageTests.release.xcconfig"
        debug_path.write_text(debug, encoding="utf-8")
        release_path.write_text(release, encoding="utf-8")
        return debug_path, release_path

    def run_helper(self, root: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(HELPER), str(root)],
            check=False,
            capture_output=True,
            text=True,
            cwd=root,
        )

    def test_removes_only_core_token_from_both_storage_configs(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ios-storage-link-") as directory:
            root = Path(directory)
            debug_path, release_path = self.write_storage_configs(root)
            app_path = root / APP_CONFIG
            app_path.parent.mkdir(parents=True)
            app_contents = (
                'OTHER_LDFLAGS = $(inherited) -l"meeterm_core" '
                '-framework "MeetermTerminal"\n'
            )
            app_path.write_text(app_contents, encoding="utf-8")

            result = self.run_helper(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            expected = STANDARD_CONFIG.replace('-l"meeterm_core"', "")
            self.assertEqual(debug_path.read_text(encoding="utf-8"), expected)
            self.assertEqual(release_path.read_text(encoding="utf-8"), expected)
            self.assertEqual(app_path.read_text(encoding="utf-8"), app_contents)

            first_output = debug_path.read_bytes(), release_path.read_bytes()
            repeated = self.run_helper(root)
            self.assertNotEqual(repeated.returncode, 0)
            self.assertIn("expected one exact meeterm_core token", repeated.stderr)
            self.assertEqual(
                (debug_path.read_bytes(), release_path.read_bytes()),
                first_output,
            )

    def test_invalid_release_token_leaves_both_configs_unchanged(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ios-storage-link-") as directory:
            root = Path(directory)
            invalid = STANDARD_CONFIG.replace(
                '-l"meeterm_core"',
                '-Wl,-force_load,meeterm_core',
            )
            debug_path, release_path = self.write_storage_configs(
                root,
                release=invalid,
            )
            before = debug_path.read_bytes(), release_path.read_bytes()

            result = self.run_helper(root)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("expected one exact meeterm_core token", result.stderr)
            self.assertEqual(
                (debug_path.read_bytes(), release_path.read_bytes()),
                before,
            )

    def test_missing_configuration_fails_before_mutating_existing_one(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ios-storage-link-") as directory:
            root = Path(directory)
            debug_path, release_path = self.write_storage_configs(root)
            release_path.unlink()
            before = debug_path.read_bytes()

            result = self.run_helper(root)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("could not read", result.stderr)
            self.assertEqual(debug_path.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
