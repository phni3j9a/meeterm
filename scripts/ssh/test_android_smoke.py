#!/usr/bin/env python3
"""Deterministic checks for the Android SSH smoke driver.

These tests exercise the parser and command-building boundary without
requiring an emulator or an OpenSSH fixture.  The hosted job remains the
authoritative check of the complete UI/native path.
"""

from __future__ import annotations

from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest import mock

import android_smoke_impl as smoke


class UiDriverTests(unittest.TestCase):
    def test_missing_xml_classifies_only_known_diagnostics(self) -> None:
        for output, reason in (
            (b"ERROR: could not get idle state.", "accessibility_not_idle"),
            (b"ERROR: null root node returned by UiTestAutomationBridge.", "root_unavailable"),
            (b"unrecognized private diagnostic", "xml_unavailable"),
        ):
            with self.subTest(reason=reason), self.assertRaises(smoke.SmokeFailure) as error:
                smoke.parse_ui_dump(output)
            self.assertEqual(error.exception.reason, reason)

    def test_node_wait_preserves_acquisition_failure_without_any_hierarchy(self) -> None:
        device = mock.Mock()
        device.dump_ui.side_effect = smoke.SmokeFailure("uiautomator", "accessibility_not_idle")
        with mock.patch.object(smoke.time, "monotonic", side_effect=[0, 0, 31]), mock.patch.object(smoke.time, "sleep"):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.wait_for_node(device, "launch", text="Connect")
        self.assertEqual((error.exception.stage, error.exception.reason), ("launch", "accessibility_not_idle"))

    def test_ui_dump_retries_transient_missing_hierarchy(self) -> None:
        device = smoke.AndroidDevice("test", "adb")
        with mock.patch.object(device, "run", side_effect=[
            b"ERROR: could not get idle state.",
            b'<hierarchy><node bounds="[0,0][100,100]" text="public-probe" /></hierarchy>',
        ]) as run, mock.patch.object(smoke.time, "sleep"):
            self.assertEqual(device.dump_ui()[0].text, "public-probe")
        self.assertEqual(run.call_count, 2)

    def test_ui_dump_persistent_failure_remains_a_failure(self) -> None:
        device = smoke.AndroidDevice("test", "adb")
        with mock.patch.object(device, "run", return_value=b"no hierarchy") as run, mock.patch.object(smoke.time, "sleep"):
            with self.assertRaises(smoke.SmokeFailure) as error:
                device.dump_ui()
        self.assertEqual(run.call_count, 3)
        self.assertEqual(error.exception.reason, "xml_unavailable")

    def test_key_input_checks_focus_after_the_last_retry(self) -> None:
        device = mock.Mock()
        unfocused = smoke.Node("", "Private OpenSSH key, Empty", "android.widget.EditText", (0, 0, 100, 100))
        focused = smoke.Node("", "Private OpenSSH key, Empty", "android.widget.EditText", (0, 0, 100, 100), focused=True)
        entered = smoke.Node("public-probe", "Private OpenSSH key, Private key entered", "android.widget.EditText", (0, 0, 100, 100), focused=True)
        device.dump_ui.side_effect = [[unfocused]] * 4 + [[focused], [entered]]
        with mock.patch.object(smoke.time, "sleep"):
            smoke.fill_multiline_key(device, "public-probe")
        self.assertEqual(device.input_tap.call_count, 3)
        device.input_text.assert_called_once_with("public-probe", "private_key_input")

    def test_key_readback_waits_for_the_focused_editor_update(self) -> None:
        device = mock.Mock()
        device.dump_ui.side_effect = [
            [smoke.Node("", "Private OpenSSH key, Empty", "android.widget.EditText", (0, 0, 100, 100), focused=True)],
            [smoke.Node("public-probe\nline", "Private OpenSSH key, Private key entered", "android.widget.EditText", (0, 0, 100, 100), focused=True)],
        ]
        smoke.verify_key_readback(device, "public-probe\nline")
        self.assertEqual(device.dump_ui.call_count, 2)

    def test_matching_text_without_keyboard_focus_is_rejected(self) -> None:
        device = mock.Mock()
        device.dump_ui.return_value = [smoke.Node(
            "public-probe", "Private OpenSSH key, Private key entered",
            "android.widget.EditText", (0, 0, 100, 100), focused=False,
        )]
        with mock.patch.object(smoke.time, "monotonic", side_effect=[0, 0, 6]), mock.patch.object(smoke.time, "sleep"):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.verify_key_readback(device, "public-probe")
        self.assertEqual(error.exception.reason, "editor_lost_focus")

    @unittest.skipUnless(shutil.which("tmux"), "tmux is needed for fixture preparation")
    def test_empty_owned_server_is_accepted_but_existing_sessions_are_preserved(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-test-") as root:
            socket_path = Path(root) / "tmux" / f"tmux-{os.getuid()}" / "default"
            socket_path.parent.mkdir(mode=0o700, parents=True)
            process = subprocess.Popen(
                ["tmux", "-D", "-f", "/dev/null", "-S", str(socket_path)],
                env=smoke._tmux_environment(socket_path),
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.monotonic() + 5
                while not socket_path.exists() and time.monotonic() < deadline:
                    self.assertIsNone(process.poll())
                    time.sleep(0.01)
                self.assertTrue(socket_path.exists())
                panes = smoke.prepare_tmux_fixture(socket_path)
                self.assertEqual(len(panes), 2)
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.prepare_tmux_fixture(socket_path)
                self.assertEqual(error.exception.reason, "session_already_exists")
                self.assertEqual(smoke.list_tmux_panes(socket_path, "test"), panes)
            finally:
                subprocess.run(["tmux", "-S", str(socket_path), "kill-server"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()

    class _ScrollingDevice:
        def __init__(self, pages: list[list[smoke.Node]]) -> None:
            self.pages = pages
            self.page = 0
            self.swipes: list[tuple[int, int, int, int]] = []

        def dump_ui(self) -> list[smoke.Node]:
            return self.pages[min(self.page, len(self.pages) - 1)]

        def input_swipe(self, bounds: tuple[int, int, int, int], _stage: str) -> None:
            self.swipes.append(bounds)
            self.page += 1

    def test_parse_ui_dump_preserves_scroll_and_accessibility_metadata(self) -> None:
        output = b"""uiautomator dump
<?xml version='1.0' encoding='UTF-8' ?><hierarchy>
<node class='android.widget.ScrollView' content-desc='' bounds='[0,100][1080,1900]'
 scrollable='true' enabled='true' visible-to-user='true'>
<node class='android.widget.EditText' content-desc='Private OpenSSH key, Empty'
   bounds='[20,1200][1060,1700]' scrollable='false' enabled='true'
   visible-to-user='true' selected='true'/>
</node></hierarchy>
UI dumped to: /dev/tty"""

        nodes = smoke.parse_ui_dump(output)

        self.assertEqual(len(nodes), 2)
        self.assertTrue(nodes[0].scrollable)
        self.assertEqual(nodes[1].content_description, "Private OpenSSH key, Empty")
        self.assertTrue(nodes[1].selected)
        self.assertEqual(
            smoke.scroll_container_bounds(nodes),
            (0, 100, 1080, 1900),
        )

    def test_scroll_container_ignores_tiny_ime_viewport(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "",
                "android.widget.ScrollView",
                (0, 100, 1080, 170),
                scrollable=True,
            ),
            smoke.Node(
                "",
                "",
                "android.view.View",
                (0, 0, 1080, 2400),
            ),
        ]

        self.assertIsNone(smoke.scroll_container_bounds(nodes))

    def test_wait_for_node_swipes_only_the_scroll_view(self) -> None:
        scroll = smoke.Node(
            "",
            "",
            "android.widget.ScrollView",
            (0, 100, 1080, 1900),
            scrollable=True,
        )
        target = smoke.Node(
            "",
            "Private OpenSSH key, Empty",
            "android.widget.EditText",
            (20, 1200, 1060, 1700),
        )
        device = self._ScrollingDevice([[scroll], [scroll, target]])

        found = smoke.wait_for_node(
            device,
            "private_key_input",
            content_descriptions=smoke.PRIVATE_KEY_ACCESSIBILITY_LABELS,
            scroll=True,
            timeout=2.0,
        )

        self.assertIs(found, target)
        self.assertEqual(device.swipes, [(0, 100, 1080, 1900)])

    def test_pane_labels_are_stable_runtime_identities(self) -> None:
        nodes = [
            smoke.Node(
                "Workspace 0",
                "",
                "android.widget.TextView",
                (10, 20, 300, 90),
            ),
            smoke.Node(
                "",
                "Terminal %17",
                "android.view.View",
                (10, 100, 300, 190),
            ),
            smoke.Node(
                "Terminal %23",
                "",
                "android.view.View",
                (310, 100, 600, 190),
                selected=True,
            ),
        ]

        first = smoke.find_pane_node(nodes)
        self.assertIsNotNone(first)
        assert first is not None
        self.assertEqual(smoke.pane_id_from_node(first), "%17")
        self.assertIsNotNone(smoke.find_pane_node(nodes, "%23"))
        self.assertIs(smoke.find_pane_node(nodes, "%23", selected=True), nodes[2])
        self.assertIsNone(smoke.find_pane_node(nodes, "%17", selected=True))
        self.assertEqual(
            {smoke.pane_id_from_node(node) for node in smoke.find_pane_nodes(nodes)},
            {"%17", "%23"},
        )
        self.assertIsNone(smoke.find_pane_node(nodes, "%99"))
        workspace = smoke.find_workspace_node(nodes)
        self.assertIsNotNone(workspace)
        assert workspace is not None
        self.assertEqual(smoke.accessible_label(workspace), "Workspace 0")

    def test_private_key_label_allows_only_known_accessibility_value_suffixes(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Private OpenSSH key, Empty",
                "android.widget.EditText",
                (10, 100, 1000, 600),
            ),
        ]

        self.assertIsNotNone(
            smoke.find_node_with_content_descriptions(
                nodes,
                smoke.PRIVATE_KEY_ACCESSIBILITY_LABELS,
            )
        )
        self.assertIsNone(
            smoke.find_node_with_content_descriptions(
                [
                    smoke.Node(
                        "",
                        "Private OpenSSH key, unexpected",
                        "android.widget.EditText",
                        (10, 100, 1000, 600),
                    )
                ],
                smoke.PRIVATE_KEY_ACCESSIBILITY_LABELS,
            )
        )

    def test_terminal_surface_prefers_native_view(self) -> None:
        nodes = [
            smoke.Node("", "", "android.view.View", (0, 50, 1080, 2400)),
            smoke.Node(
                "",
                "",
                "dev.meeterm.terminal.MeetermTerminalView",
                (0, 100, 1080, 2200),
            ),
        ]

        self.assertIs(smoke.find_terminal_node(nodes), nodes[1])

    def test_tmux_parser_keeps_pane_pid_and_real_selection_state(self) -> None:
        output = (
            b"@4\t%12\t1201\t0\t1\t1\n"
            b"@4\t%13\t1202\t1\t1\t1\n"
        )

        records = smoke.parse_tmux_panes(output)

        self.assertEqual(records[1].pane_id, "%13")
        self.assertEqual(records[1].pane_pid, 1202)
        self.assertTrue(
            smoke._selection_matches(records, "%13", 1202),
        )
        self.assertFalse(smoke._selection_matches(records, "%12", 1201))

    def test_tmux_socket_must_be_fixture_scoped(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root_text:
            root = Path(root_text)
            key_path = root / "client_ed25519"
            key_path.write_text("placeholder", encoding="utf-8")
            socket = root / "tmux" / f"tmux-{os.getuid()}" / "default"
            with mock.patch.dict(
                os.environ,
                {"MEETERM_TMUX_SOCKET": str(socket)},
                clear=False,
            ):
                self.assertEqual(smoke.tmux_socket_from_fixture(key_path), socket)

            with mock.patch.dict(
                os.environ,
                {"MEETERM_TMUX_SOCKET": "/tmp/default"},
                clear=False,
            ):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.tmux_socket_from_fixture(key_path)
            self.assertEqual(error.exception.reason, "socket_path_outside_fixture")

    def test_tmux_commands_use_only_the_explicit_fixture_socket(self) -> None:
        socket = (
            Path.home()
            / "meeterm-ssh-fixture-test"
            / "tmux"
            / f"tmux-{os.getuid()}"
            / "default"
        )
        completed = subprocess.CompletedProcess([], 0, b"")
        with mock.patch.dict(
            os.environ,
            {
                "TMUX": "/run/user/1000/tmux/default,123,0",
                "TMUX_PANE": "%99",
                "MEETERM_SSH_PASSPHRASE": "secret-must-not-reach-tmux",
            },
            clear=False,
        ):
            with mock.patch.object(smoke.shutil, "which", return_value="/usr/bin/tmux"):
                with mock.patch.object(
                    smoke.subprocess,
                    "run",
                    return_value=completed,
                ) as run:
                    smoke.run_tmux_command(socket, ("list-sessions",), "test_tmux")

        command = run.call_args.args[0]
        self.assertEqual(command[:4], ["/usr/bin/tmux", "-f", "/dev/null", "-S"])
        self.assertEqual(command[4:], [str(socket), "list-sessions"])
        environment = run.call_args.kwargs["env"]
        self.assertNotIn("TMUX", environment)
        self.assertNotIn("TMUX_PANE", environment)
        self.assertNotIn("MEETERM_SSH_PASSPHRASE", environment)


class CommandTests(unittest.TestCase):
    def test_marker_commands_are_ascii_and_do_not_use_input_text_percent_escape(self) -> None:
        marker = "meeterm-android-shell-0123456789abcdef"
        path = Path("/tmp/meeterm-ssh-fixture-test/.marker.txt")

        first = smoke.session_marker_command(marker, path)
        resumed = smoke.resumed_marker_command(marker, path)

        self.assertNotIn("%", first)
        self.assertNotIn("%", resumed)
        self.assertNotIn("\n", first)
        self.assertNotIn("\n", resumed)
        self.assertIn("MEETERM_ANDROID_SESSION_MARKER", first)
        self.assertIn("-reconnected", resumed)
        self.assertTrue(all(ord(character) < 128 for character in first + resumed))

        with_pid = smoke.session_marker_command(marker, path, 1202)
        resumed_with_pid = smoke.resumed_marker_command(marker, path, 1202)
        self.assertIn(":$$", with_pid)
        self.assertIn('[ "$$" = 1202 ]', resumed_with_pid)

    def test_marker_commands_reject_input_text_unsafe_marker(self) -> None:
        with self.assertRaises(smoke.SmokeFailure) as first_error:
            smoke.session_marker_command("marker%", Path("/tmp/marker"))
        self.assertEqual((first_error.exception.stage, first_error.exception.reason),
                         ("remote_marker", "invalid_marker"))

        with self.assertRaises(smoke.SmokeFailure) as resumed_error:
            smoke.resumed_marker_command("marker\n", Path("/tmp/marker"))
        self.assertEqual(
            (resumed_error.exception.stage, resumed_error.exception.reason),
            ("remote_marker_resume", "invalid_marker"),
        )

    def test_wait_for_file_contents_accepts_exact_content_only(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-android-smoke-") as root:
            path = Path(root) / "marker"
            path.write_text("one\ntwo\n", encoding="utf-8")

            smoke.wait_for_file_contents(path, "one\ntwo\n", "test_marker")

            with self.assertRaises(smoke.SmokeFailure) as repeated_error:
                smoke.wait_for_file_contents(path, "one\n", "test_marker")
            self.assertEqual(repeated_error.exception.reason, "marker_repeated")


if __name__ == "__main__":
    unittest.main()
