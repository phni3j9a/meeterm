"""Presentation readiness contracts; these do not simulate native rendering."""

import importlib.util
from pathlib import Path
import sys
import unittest


spec = importlib.util.spec_from_file_location(
    "android_screen_fixtures", Path(__file__).with_name("android-screen-fixtures.py")
)
fixtures = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = fixtures
spec.loader.exec_module(fixtures)


class PresentationReadinessTests(unittest.TestCase):
    def test_all_twenty_one_routes_require_visible_content(self):
        self.assertEqual(len(fixtures.SCREEN_NAMES), 21)
        self.assertEqual(len(set(fixtures.SCREEN_NAMES)), 21)
        for screen in fixtures.SCREEN_NAMES:
            with self.subTest(screen=screen):
                self.assertTrue(fixtures.screen_checks(screen, set()))

    def test_workspace_rows_do_not_depend_on_replaced_count_subtitles(self):
        values = {"All  2", "Workspace Main workspace", "Workspace Tools workspace"}
        self.assertEqual(fixtures.screen_checks("herdr-workspaces", values), [])
        values.remove("Workspace Tools workspace")
        self.assertIn("tools_workspace_row", fixtures.screen_checks("herdr-workspaces", values))

    def test_agent_route_still_requires_both_identity_and_status(self):
        values = {"Switch terminal group", "Terminal", "Claude Code", "Working"}
        self.assertEqual(fixtures.screen_checks("herdr-terminal", values), [])
        values.remove("Working")
        self.assertIn("agent_working", fixtures.screen_checks("herdr-terminal", values))

    def test_empty_state_is_not_confused_with_disconnected_state(self):
        empty = {"A fresh workspace starts here.", "Create workspace"}
        self.assertEqual(fixtures.screen_checks("empty", empty), [])
        self.assertTrue(fixtures.screen_checks("disconnected", empty))


if __name__ == "__main__":
    unittest.main()
