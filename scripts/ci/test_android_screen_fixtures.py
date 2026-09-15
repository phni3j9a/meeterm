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
    def test_all_twenty_five_routes_require_visible_content(self):
        self.assertEqual(len(fixtures.SCREEN_NAMES), 25)
        self.assertEqual(len(set(fixtures.SCREEN_NAMES)), 25)
        for screen in fixtures.SCREEN_NAMES:
            with self.subTest(screen=screen):
                self.assertTrue(fixtures.screen_checks(screen, set()))

    def test_workspace_rows_do_not_depend_on_replaced_count_subtitles(self):
        values = {
            "All  2",
            "Workspace Main workspace, Agent status: blocked, 2 terminals",
            "Workspace Tools workspace, Agent status: idle, 1 terminal",
        }
        self.assertEqual(fixtures.screen_checks("herdr-workspaces", values), [])
        values.remove("Workspace Tools workspace, Agent status: idle, 1 terminal")
        self.assertIn("tools_workspace_row", fixtures.screen_checks("herdr-workspaces", values))
        self.assertIn("tools_workspace_status", fixtures.screen_checks("herdr-workspaces", values))

    def test_agent_route_still_requires_both_identity_and_status(self):
        values = {
            "Switch terminal group, Group Development, Agent status: working",
            "Terminal",
            "Claude Code",
            "Working",
            "Terminal Tests, Agent status: blocked",
            "Terminal Review, Agent status: finished, not yet viewed",
            "Terminal Monitor, Agent status: idle",
            "Terminal Logs, Agent status: unknown",
        }
        self.assertEqual(fixtures.screen_checks("herdr-terminal", values), [])
        values.remove("Working")
        values.remove("Switch terminal group, Group Development, Agent status: working")
        self.assertIn("agent_working", fixtures.screen_checks("herdr-terminal", values))

    def test_empty_state_is_not_confused_with_disconnected_state(self):
        empty = {"A fresh workspace starts here.", "Create workspace"}
        self.assertEqual(fixtures.screen_checks("empty", empty), [])
        self.assertTrue(fixtures.screen_checks("disconnected", empty))

    def test_runtime_picker_requires_both_backends_and_stopped_herdr(self):
        values = {
            "Choose a runtime for Smoke server",
            "tmux runtime meeterm",
            "Herdr runtime default",
            "Herdr runtime paused",
        }
        self.assertEqual(fixtures.screen_checks("runtime-picker", values), [])
        values.remove("Herdr runtime paused")
        self.assertIn("herdr_paused", fixtures.screen_checks("runtime-picker", values))

    def test_runtime_create_requires_the_native_form_contract(self):
        values = {
            "Create tmux session",
            "tmux session name",
            "dev.meeterm.app:id/runtime-tmux-create-submit",
        }
        self.assertEqual(fixtures.screen_checks("runtime-create", values), [])

    def test_herdr_connection_route_is_the_picker_with_last_used_hint(self):
        values = {
            "Choose a runtime for Smoke server",
            "Herdr runtime default",
            "Last used",
        }
        self.assertEqual(fixtures.screen_checks("herdr-connection", values), [])


if __name__ == "__main__":
    unittest.main()
