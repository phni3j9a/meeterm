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
APP_SOURCE = Path(__file__).parents[2] / "App.tsx"


class PresentationReadinessTests(unittest.TestCase):
    def test_all_thirty_two_routes_require_visible_content(self):
        self.assertEqual(len(fixtures.SCREEN_NAMES), 32)
        self.assertEqual(len(set(fixtures.SCREEN_NAMES)), 32)
        for screen in fixtures.SCREEN_NAMES:
            with self.subTest(screen=screen):
                self.assertTrue(fixtures.screen_checks(screen, set()))

    def test_switcher_and_recovery_routes_are_stably_ordered_after_runtime_routes(self):
        self.assertEqual(
            fixtures.SCREEN_NAMES[14:25],
            (
                "session-switcher",
                "session-switcher-sessions",
                "herdr-connection",
                "herdr-groups",
                "herdr-terminal",
                "herdr-workspaces",
                "recovery-progress",
                "recovery-exhausted",
                "recovery-mismatch",
                "layout-restore-unconfirmed",
                "runtime-layout-restore-unconfirmed",
            ),
        )

    def test_switcher_screens_require_the_session_hierarchy_and_no_old_picker(self):
        server = {
            "Switch server or session",
            "Browse sessions on Smoke server",
            "Manage servers",
        }
        self.assertEqual(fixtures.screen_checks("session-switcher", server), [])
        sessions = {
            "Sessions on Smoke server",
            "tmux session meeterm",
            "Herdr session default",
            "Herdr session paused",
            "Last used",
        }
        self.assertEqual(fixtures.screen_checks("session-switcher-sessions", sessions), [])
        sessions.add("Choose a runtime for Smoke server")
        self.assertIn("runtime_picker_hidden", fixtures.screen_checks("session-switcher-sessions", sessions))

    def test_recovery_routes_and_ids_match_the_app_smoke_contract(self):
        source = APP_SOURCE.read_text(encoding="utf-8")
        for route in (
                "recovery-progress",
                "recovery-exhausted",
                "recovery-mismatch",
                "layout-restore-unconfirmed",
                "runtime-layout-restore-unconfirmed",
            ):
            with self.subTest(route=route):
                self.assertIn(f"'{route}'", source)
        for test_id in (
            "recovery-rail",
            "recovery-title",
            "recovery-detail",
            "recovery-meta",
            "recovery-retry",
            "recovery-change",
        ):
            with self.subTest(test_id=test_id):
                self.assertIn(f'testID="{test_id}"', source)

    def test_recovery_routes_require_cached_terminal_rail_copy_and_enabled_actions(self):
        common = {
            "recovery-rail::enabled",
            "recovery-title::enabled",
            "recovery-detail::enabled",
            "recovery-meta::enabled",
            "Terminal, cached output, read only",
            "Last received output · Input paused",
        }
        progress = common | {
            "Verifying this workspace…",
            "Checking the server, runtime, and terminal.",
        }
        self.assertEqual(fixtures.screen_checks("recovery-progress", progress), [])

        exhausted = common | {
            "Still offline",
            "Couldn’t reach Smoke server.",
            "recovery-retry::enabled",
            "recovery-change::enabled",
        }
        self.assertEqual(fixtures.screen_checks("recovery-exhausted", exhausted), [])
        exhausted.remove("recovery-retry::enabled")
        self.assertIn(
            "recovery_action_recovery-retry_enabled",
            fixtures.screen_checks("recovery-exhausted", exhausted),
        )

        mismatch = common | {
            "This runtime can’t be restored",
            'The runtime named “meeterm” is not the same instance as before.',
            "recovery-retry::enabled",
            "recovery-change::enabled",
        }
        self.assertEqual(fixtures.screen_checks("recovery-mismatch", mismatch), [])

    def test_layout_restore_warning_routes_require_real_message_and_dismiss_action(self):
        warning = "The old connection's desktop layout restore could not be confirmed."
        dismiss_enabled = "cleanup-warning-dismiss::enabled"
        self.assertEqual(
            fixtures.screen_checks(
                "layout-restore-unconfirmed",
                {"Disconnected", warning, "Dismiss desktop layout warning", dismiss_enabled},
            ),
            [],
        )
        self.assertEqual(
            fixtures.screen_checks(
                "runtime-layout-restore-unconfirmed",
                {
                    "Choose a runtime for Smoke server",
                    warning,
                    "Dismiss desktop layout warning",
                    dismiss_enabled,
                },
            ),
            [],
        )

    def test_layout_restore_warning_rejects_old_only_or_incomplete_display(self):
        warning = "The old connection's desktop layout restore could not be confirmed."
        dismiss_enabled = "cleanup-warning-dismiss::enabled"
        old_only = {
            "Disconnected",
            "The connection closed, but the desktop layout could not be confirmed as restored.",
            "Dismiss message",
        }
        missing_warning = {"Disconnected", "Dismiss desktop layout warning", dismiss_enabled}
        missing_dismiss = {"Disconnected", warning}
        self.assertIn(
            "layout_restore_warning",
            fixtures.screen_checks("layout-restore-unconfirmed", old_only),
        )
        self.assertIn(
            "warning_dismiss",
            fixtures.screen_checks("layout-restore-unconfirmed", old_only),
        )
        self.assertIn(
            "warning_dismiss_enabled",
            fixtures.screen_checks("layout-restore-unconfirmed", old_only),
        )
        self.assertIn(
            "layout_restore_warning",
            fixtures.screen_checks("layout-restore-unconfirmed", missing_warning),
        )
        self.assertIn(
            "warning_dismiss",
            fixtures.screen_checks("layout-restore-unconfirmed", missing_dismiss),
        )
        self.assertIn(
            "warning_dismiss_enabled",
            fixtures.screen_checks("layout-restore-unconfirmed", missing_dismiss),
        )

    def test_layout_restore_warning_rejects_disabled_or_hidden_dismiss(self):
        warning = "The old connection's desktop layout restore could not be confirmed."
        label = "Dismiss desktop layout warning"
        base = {"Disconnected", warning, label}
        disabled = base | {"cleanup-warning-dismiss::disabled"}
        hidden_label_only = base
        for values in (disabled, hidden_label_only):
            self.assertIn(
                "warning_dismiss_enabled",
                fixtures.screen_checks("layout-restore-unconfirmed", values),
            )
        # A disabled node from a real UIAutomator dump keeps the label but
        # records ::disabled, so the enabled check must reject it.
        root = fixtures.ET.fromstring(
            '<hierarchy><node text="Disconnected" bounds="[0,0][1080,100]" />'
            f'<node text="{warning}" bounds="[0,100][1080,200]" />'
            f'<node content-desc="{label}" resource-id="dev.meeterm.app:id/cleanup-warning-dismiss" '
            'visible-to-user="true" enabled="false" bounds="[0,200][100,260]" />'
            "</hierarchy>"
        )
        self.assertIn(
            "warning_dismiss_enabled",
            fixtures.screen_checks("layout-restore-unconfirmed", fixtures.ui_values(root)),
        )

    def test_connection_error_requires_auth_guidance_and_independent_warning(self):
        auth_guidance = (
            "Authentication failed. Check your username and the password or private key "
            "for your chosen sign-in method."
        )
        warning = "The old connection's desktop layout restore could not be confirmed."
        dismiss = "Dismiss desktop layout warning"
        dismiss_enabled = "cleanup-warning-dismiss::enabled"
        canonical = {"Connection failed", "Reconnect", auth_guidance, warning, dismiss, dismiss_enabled}
        self.assertEqual(fixtures.screen_checks("connection-error", canonical), [])
        self.assertIn(
            "authentication_guidance",
            fixtures.screen_checks("connection-error", canonical - {auth_guidance}),
        )
        self.assertIn(
            "layout_restore_warning",
            fixtures.screen_checks("connection-error", canonical - {warning}),
        )
        self.assertIn(
            "warning_dismiss",
            fixtures.screen_checks("connection-error", canonical - {dismiss}),
        )
        self.assertIn(
            "warning_dismiss_enabled",
            fixtures.screen_checks(
                "connection-error", (canonical - {dismiss_enabled}) | {"cleanup-warning-dismiss::disabled"}
            ),
        )

    def test_ui_values_records_visibility_state_for_test_ids(self):
        root = fixtures.ET.fromstring(
            '<hierarchy><node resource-id="dev.meeterm.app:id/recovery-retry" '
            'visible-to-user="true" enabled="true" bounds="[0,0][20,20]" />'
            '<node resource-id="dev.meeterm.app:id/recovery-change" '
            'visible-to-user="true" enabled="false" bounds="[0,0][20,20]" />'
            '</hierarchy>'
        )
        values = fixtures.ui_values(root)
        self.assertIn("dev.meeterm.app:id/recovery-retry::enabled", values)
        self.assertIn("dev.meeterm.app:id/recovery-change::disabled", values)

        hidden = fixtures.ET.fromstring(
            '<hierarchy><node resource-id="dev.meeterm.app:id/recovery-rail" '
            'visible-to-user="true" enabled="true" bounds="[0,0][0,20]" />'
            '</hierarchy>'
        )
        self.assertNotIn("dev.meeterm.app:id/recovery-rail::enabled", fixtures.ui_values(hidden))

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

    def test_agent_route_uses_selected_visible_status_not_offscreen_tabs(self):
        values = {
            "Switch terminal group, Group Development, Agent status: working",
            "Terminal",
            "Claude Code, Agent status: working",
            "Working",
        }
        self.assertEqual(fixtures.screen_checks("herdr-terminal", values), [])
        values.update({
            "Terminal Tests, Agent status: blocked",
            "Terminal Review, Agent status: finished, not yet viewed",
            "Terminal Monitor, Agent status: idle",
            "Terminal Logs, Agent status: unknown",
        })
        self.assertEqual(fixtures.screen_checks("herdr-terminal", values), [])

        values.remove("Claude Code, Agent status: working")
        self.assertIn("selected_agent_owner", fixtures.screen_checks("herdr-terminal", values))
        values.add("Claude Code, Agent status: working")
        values.remove("Working")
        self.assertIn("selected_agent_status", fixtures.screen_checks("herdr-terminal", values))

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
