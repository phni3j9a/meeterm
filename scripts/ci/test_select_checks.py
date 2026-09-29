"""Guard the lightweight path and the native/security checks that must remain."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("select_checks", Path(__file__).with_name("select-checks.py"))
checks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checks)


class SelectionTests(unittest.TestCase):
    def test_documentation_and_evidence_need_no_build(self):
        self.assertEqual(checks.select_checks(["AGENTS.md", "docs/TESTING.md", "docs/mock/index.html", "artifacts/ios/screen.png"]), set())

    def test_ui_change_does_not_build_native_or_probe_ssh(self):
        self.assertEqual(checks.select_checks(["App.tsx", "app/theme.ts", "scripts/ci/app-selection.test.cjs"]), {"javascript"})

    def test_shared_native_change_keeps_both_adapters_and_rust(self):
        self.assertEqual(checks.select_checks(["native/meeterm-core/src/ssh.rs"]), {"rust", "ios", "android", "scripts"})

    def test_platform_specific_changes_stay_local(self):
        for platform in ("ios", "android"):
            with self.subTest(platform=platform):
                self.assertEqual(checks.select_checks([f"modules/meeterm-terminal/{platform}/source"]), {platform, "scripts"})

    def test_dependencies_include_native_build_checks(self):
        for path in ("package-lock.json", "app.json", "app.config.ts", "modules/meeterm-terminal/MeetermTerminal.podspec"):
            with self.subTest(path=path):
                self.assertEqual(checks.select_checks([path]), {"javascript", "ios", "android", "scripts", "dependencies"})

    def test_native_assets_and_cng_plugins_are_build_inputs(self):
        for path in ("plugins/with-native-accent.js", "app/assets/app-icon.png"):
            with self.subTest(path=path):
                self.assertEqual(checks.select_checks([path]), {"javascript", "ios", "android", "scripts"})

    def test_fixture_and_swift_driver_select_their_runtime(self):
        self.assertEqual(checks.select_checks(["scripts/ssh/fixture.py"]), {"scripts", "rust"})
        self.assertEqual(checks.select_checks(["scripts/ci/MeetermSmokeUITests.swift"]), {"scripts", "ios"})
        self.assertEqual(checks.select_checks(["scripts/ci/devin-cloud.py"]), {"scripts"})

    def test_unknown_and_ci_inputs_fail_safe(self):
        for path in ("new-build.config", ".github/workflows/ci.yml", "scripts/ci/select-checks.py"):
            with self.subTest(path=path):
                self.assertEqual(checks.select_checks([path]), set(checks.CHECKS))

    def test_git_diff_handles_renames_deletions_and_pr_base_advancement(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {"GIT_CONFIG_NOSYSTEM": "1"}):
            def git(*args):
                return subprocess.check_output(["git", "-C", directory, *args], stderr=subprocess.DEVNULL, text=True).strip()

            git("init", "-b", "base")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.invalid")
            root = Path(directory)
            (root / "App.tsx").write_text("ui\n")
            (root / "old.txt").write_text("old\n")
            git("add", ".")
            git("commit", "-m", "base")
            git("checkout", "-b", "pr")
            git("mv", "App.tsx", "guide.md")
            git("rm", "old.txt")
            git("commit", "-m", "rename and delete")
            git("checkout", "base")
            (root / "unrelated.swift").write_text("base advanced\n")
            git("add", ".")
            git("commit", "-m", "unrelated base update")
            previous = os.getcwd()
            try:
                os.chdir(directory)
                self.assertEqual(set(checks.changed_paths("base", "pr", merge_base=True)), {"App.tsx", "guide.md", "old.txt"})
                with self.assertRaises(subprocess.CalledProcessError):
                    checks.changed_paths("missing-ref", "pr")
            finally:
                os.chdir(previous)


if __name__ == "__main__":
    unittest.main()
