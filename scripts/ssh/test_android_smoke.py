#!/usr/bin/env python3
"""Deterministic checks for the Android SSH smoke driver.

These tests exercise the parser, command-building boundary, and deterministic
key-entry state machine without requiring an emulator or an OpenSSH fixture.
The hosted job remains the authoritative check of the complete UI/native path.
"""

from __future__ import annotations

import ast
import contextlib
import io
from pathlib import Path
import os
import shutil
import stat
import subprocess
import tempfile
import time
import unittest
from unittest import mock

import android_smoke_impl as smoke


_EDITOR_UNAVAILABLE = object()


class _FakeClock:
    """Small monotonic clock whose sleeps never wait in real time."""

    def __init__(self) -> None:
        self.now = 0.0
        self.sleep_calls: list[float] = []

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.sleep_calls.append(seconds)
        self.now += seconds

    def advance(self, seconds: float) -> None:
        self.now += seconds


class _RouteDevice(smoke.AndroidDevice):
    def __init__(self, host_output: bytes, reverse_output: bytes) -> None:
        super().__init__("emulator-5554", "adb")
        self.host_output = host_output
        self.reverse_output = reverse_output
        self.commands: list[tuple[str, tuple[str, ...]]] = []

    def run_host_adb(
        self,
        arguments: tuple[str, ...],
        stage: str,
        timeout: float = 15.0,
    ) -> bytes:
        del stage, timeout
        self.commands.append(("host", arguments))
        return self.host_output

    def run(
        self,
        arguments: tuple[str, ...],
        stage: str,
        timeout: float = 15.0,
    ) -> bytes:
        del stage, timeout
        self.commands.append(("device", arguments))
        return self.reverse_output


class AndroidFixtureTransportTests(unittest.TestCase):
    READY_INVENTORY = (
        b"List of devices attached\n"
        b"emulator-5554 device product:sdk model:test transport_id:1\n"
    )

    def test_emulator_serial_is_required_for_host_alias(self) -> None:
        smoke.require_emulator_serial("emulator-5554")
        for serial in ("physical-1", "127.0.0.1:5555", "", "emulator-bad"):
            with self.subTest(serial=serial):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.require_emulator_serial(serial)
                self.assertEqual(
                    (error.exception.stage, error.exception.reason),
                    ("device_select", "emulator_required"),
                )

    def test_fixture_loopback_is_mapped_to_official_emulator_alias(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-test-") as root:
            key_path = Path(root) / "client_key"
            key_path.write_text(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n"
                "fixture\n"
                "-----END OPENSSH PRIVATE KEY-----\n",
                encoding="utf-8",
            )
            environment = {
                "MEETERM_SSH_HOST": "127.0.0.1",
                "MEETERM_SSH_PORT": "2222",
                "MEETERM_SSH_USERNAME": "fixture",
                "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE": str(key_path),
                "MEETERM_SSH_PRIVATE_KEY_FILE": str(key_path),
            }
            with mock.patch.dict(smoke.os.environ, environment, clear=True):
                host, port, username, _key, fixture_key = smoke.load_fixture()

        self.assertEqual(host, "10.0.2.2")
        self.assertEqual(host, smoke.ANDROID_EMULATOR_HOST_ALIAS)
        self.assertEqual((port, username), (2222, "fixture"))
        self.assertEqual(fixture_key, key_path)

    def test_fixture_rejects_non_loopback_published_host(self) -> None:
        with mock.patch.dict(
            smoke.os.environ,
            {"MEETERM_SSH_HOST": "10.0.2.2"},
            clear=True,
        ):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.load_fixture()
        self.assertEqual(
            (error.exception.stage, error.exception.reason),
            ("fixture_environment", "loopback_required"),
        )

    def test_route_preflight_requires_one_ready_emulator_and_empty_reverse(self) -> None:
        device = _RouteDevice(self.READY_INVENTORY, b"")
        device.verify_emulator_fixture_route("route")
        self.assertEqual(
            device.commands,
            [
                ("host", ("devices", "-l")),
                ("device", ("reverse", "--list")),
            ],
        )

    def test_route_preflight_rejects_other_transport_states_without_mutation(self) -> None:
        unsafe = (
            b"unexpected\n",
            b"List of devices attached\nemulator-5554 offline\n",
            b"List of devices attached\nemulator-5554 unauthorized\n",
            b"List of devices attached\nemulator-5554 device\nemulator-5556 device\n",
        )
        for inventory in unsafe:
            with self.subTest(inventory=inventory):
                device = _RouteDevice(inventory, b"")
                with self.assertRaises(smoke.SmokeFailure):
                    device.verify_emulator_fixture_route("route")
                self.assertNotIn(("device", ("reverse", "--remove-all")), device.commands)

    def test_route_preflight_rejects_existing_reverse_without_removing_it(self) -> None:
        device = _RouteDevice(
            self.READY_INVENTORY,
            b"emulator-5554 tcp:2222 tcp:2222\n",
        )
        with self.assertRaises(smoke.SmokeFailure) as error:
            device.verify_emulator_fixture_route("route")
        self.assertEqual(error.exception.reason, "adb_reverse_present")
        self.assertEqual(device.commands[-1], ("device", ("reverse", "--list")))

    def test_host_alias_reachability_uses_bounded_zero_io_probe(self) -> None:
        device = smoke.AndroidDevice("emulator-5554", "adb")
        with mock.patch.object(device, "run", return_value=b"") as run:
            device.verify_emulator_host_alias(2222, "alias")
        run.assert_called_once_with(
            (
                "shell",
                "toybox",
                "nc",
                "-z",
                "-w",
                "5",
                "10.0.2.2",
                "2222",
            ),
            "alias",
            timeout=8.0,
        )

    def test_host_alias_unreachable_has_explicit_sanitized_reason(self) -> None:
        device = smoke.AndroidDevice("emulator-5554", "adb")
        with mock.patch.object(
            device,
            "run",
            side_effect=smoke.SmokeFailure("alias", "adb_failed"),
        ):
            with self.assertRaises(smoke.SmokeFailure) as error:
                device.verify_emulator_host_alias(2222, "alias")
        self.assertEqual(
            (error.exception.stage, error.exception.reason),
            ("alias", "emulator_host_alias_unreachable"),
        )

    def test_transport_loss_path_has_no_adb_reverse_or_server_restart(self) -> None:
        source = Path(smoke.__file__).read_text(encoding="utf-8")
        self.assertNotIn('("reverse", "--no-rebind"', source)
        self.assertNotIn('("reverse", "--remove"', source)
        self.assertNotIn('("reverse", "--remove-all"', source)
        self.assertNotIn('"kill-server"', source)
        self.assertNotIn('("root",)', source)
        self.assertNotIn('("unroot",)', source)
        self.assertIn("ANDROID_EMULATOR_HOST_ALIAS = \"10.0.2.2\"", source)


class ArtifactBoundaryTests(unittest.TestCase):
    def test_marker_timeout_distinguishes_absent_and_mismatched_content(self) -> None:
        clock = _FakeClock()
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            marker = Path(root) / "foreground.txt"
            with _patched_clock(clock):
                with self.assertRaises(smoke.SmokeFailure) as absent:
                    smoke.wait_for_file_contents(marker, "expected\n", "foreground")
            self.assertEqual(absent.exception.reason, "marker_timeout")

            marker.write_text("mismatch\n", encoding="utf-8")
            with _patched_clock(clock):
                with self.assertRaises(smoke.SmokeFailure) as mismatched:
                    smoke.wait_for_file_contents(marker, "expected\n", "foreground")
            self.assertEqual(mismatched.exception.reason, "marker_content_mismatch")

    def test_selection_fixture_missing_ack_stops_before_native_drag(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            fixture = Path(root)
            setup_marker = fixture / "selection-ready.txt"
            with (
                _patched_clock(clock),
                mock.patch.object(smoke, "terminal_line") as terminal_line,
                mock.patch.object(smoke, "run_tmux_command") as capture_row,
                mock.patch.object(smoke, "wait_for_labeled_terminal_surface") as wait_surface,
                mock.patch.object(smoke, "list_tmux_panes") as list_panes,
            ):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.prepare_and_select_daily_marker(
                        device,
                        fixture / "tmux.sock",
                        "%7",
                        setup_marker,
                        "fixture-ready",
                        fixture,
                        [],
                    )

            self.assertEqual(
                (error.exception.stage, error.exception.reason),
                ("daily_selection_fixture", "marker_timeout"),
            )
            terminal_line.assert_called_once()
            capture_row.assert_not_called()
            wait_surface.assert_not_called()
            list_panes.assert_not_called()
            device.input_long_press_drag.assert_not_called()

    def test_selection_fixture_exact_ack_proceeds_to_native_drag(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        terminal = smoke.Node(
            "",
            "Terminal",
            "android.view.SurfaceView",
            (0, 448, 1080, 1391),
        )
        panes = smoke.parse_tmux_panes(
            b"@3\tdaily-ci-renamed\t%7\t1201\t1\t1\t45\t14\t0\t0\t44\t13\t1\t0\n"
        )
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            fixture = Path(root)
            setup_marker = fixture / "selection-ready.txt"
            completed: list[str] = []

            def send_fixture_line(_device: object, command: str) -> None:
                self.assertIn("clear; printf 'COPY29F7\\n' && ", command)
                self.assertIn("&& echo fixture-ready > ", command)
                self.assertIn(str(setup_marker), command)
                self.assertTrue(command.endswith("; stty echo"))
                setup_marker.write_text("fixture-ready\n", encoding="utf-8")

            with (
                _patched_clock(clock),
                mock.patch.object(
                    smoke,
                    "terminal_line",
                    side_effect=send_fixture_line,
                ),
                mock.patch.object(
                    smoke,
                    "wait_for_labeled_terminal_surface",
                    return_value=terminal,
                ),
                mock.patch.object(
                    smoke,
                    "run_tmux_command",
                    return_value=subprocess.CompletedProcess(
                        [],
                        0,
                        b"COPY29F7\n",
                    ),
                ) as capture_row,
                mock.patch.object(smoke, "list_tmux_panes", return_value=panes),
            ):
                smoke.prepare_and_select_daily_marker(
                    device,
                    fixture / "tmux.sock",
                    "%7",
                    setup_marker,
                    "fixture-ready",
                    fixture,
                    completed,
                )

            self.assertEqual(completed, ["daily_selection_fixture_ready"])
            self.assertEqual(
                capture_row.call_args.args[1],
                ("capture-pane", "-p", "-t", "%7", "-S", "0", "-E", "0"),
            )
            device.input_long_press_drag.assert_called_once_with(
                12,
                472,
                180,
                472,
                "daily_terminal_selection",
            )
            self.assertTrue((fixture / "daily-selection-geometry.txt").exists())

    def test_selection_fixture_rejects_wrong_visible_row_before_drag(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            fixture = Path(root)
            setup_marker = fixture / "selection-ready.txt"

            def acknowledge_fixture(_device: object, _command: str) -> None:
                setup_marker.write_text("fixture-ready\n", encoding="utf-8")

            with (
                _patched_clock(clock),
                mock.patch.object(
                    smoke,
                    "terminal_line",
                    side_effect=acknowledge_fixture,
                ),
                mock.patch.object(
                    smoke,
                    "run_tmux_command",
                    return_value=subprocess.CompletedProcess([], 0, b"COPY29F8\n"),
                ),
                mock.patch.object(smoke, "wait_for_labeled_terminal_surface") as wait_surface,
            ):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.prepare_and_select_daily_marker(
                        device,
                        fixture / "tmux.sock",
                        "%7",
                        setup_marker,
                        "fixture-ready",
                        fixture,
                        [],
                    )

            self.assertEqual(
                (error.exception.stage, error.exception.reason),
                ("daily_selection_fixture", "display_marker_mismatch"),
            )
            wait_surface.assert_not_called()
            device.input_long_press_drag.assert_not_called()

    def test_glyph_stress_sequence_checks_real_marker_before_capture(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.run.side_effect = [
            b"",
            b"I/MeetermRenderer: MEETERM_GLYPH_ATLAS_RESET count=1\n",
        ]
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            fixture = Path(root)
            stress = smoke.make_glyph_stress_file(fixture / "key")
            marker = fixture / "done.txt"
            completed: list[str] = []

            def send_line(_device: object, command: str) -> None:
                if str(stress) in command:
                    marker.write_text("fixture-done\n", encoding="utf-8")

            with (
                _patched_clock(clock),
                mock.patch.object(smoke, "wait_for_terminal"),
                mock.patch.object(smoke, "focus_terminal"),
                mock.patch.object(smoke, "terminal_line", side_effect=send_line),
                mock.patch.object(smoke, "capture_optional_screenshot", return_value="ok") as capture,
            ):
                smoke.exercise_glyph_atlas_stress(
                    device, stress, marker, "fixture-done", fixture, completed
                )

            self.assertEqual(marker.read_text(encoding="utf-8"), "fixture-done\n")
            self.assertEqual(completed, ["daily_glyph_atlas_reset"])
            self.assertEqual(device.run.call_count, 2)
            capture.assert_called_once()
            self.assertEqual(capture.call_args.args[1], fixture / "daily-glyph-atlas.png")

    def test_glyph_stress_file_has_distinct_public_cjk_and_final_marker(self) -> None:
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            stress_path = smoke.make_glyph_stress_file(Path(root) / "fixture-key")
            try:
                lines = stress_path.read_text(encoding="utf-8").splitlines()
                cjk = "".join(lines[:-1])
                self.assertEqual(len(cjk), smoke.DAILY_GLYPH_STRESS_COUNT)
                self.assertEqual(
                    [ord(character) for character in cjk],
                    list(
                        range(
                            0x4E00,
                            0x4E00 + smoke.DAILY_GLYPH_STRESS_COUNT,
                        )
                    ),
                )
                self.assertTrue(
                    all(
                        len(line) == smoke.DAILY_GLYPH_STRESS_COLUMNS
                        for line in lines[:-1]
                    )
                )
                self.assertEqual(lines[-1], "END 日本語")
                self.assertEqual(stat.S_IMODE(stress_path.stat().st_mode), 0o600)
            finally:
                stress_path.unlink()

    def test_wait_for_atlas_reset_requires_a_new_sanitized_marker(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.run.side_effect = [
            b"I/MeetermRenderer: MEETERM_GLYPH_ATLAS_RESET count=1\n",
            (
                b"I/MeetermRenderer: MEETERM_GLYPH_ATLAS_RESET count=1\n"
                b"I/MeetermRenderer: unrelated count=2\n"
            ),
            (
                b"I/MeetermRenderer: MEETERM_GLYPH_ATLAS_RESET count=1\n"
                b"I/MeetermRenderer: MEETERM_GLYPH_ATLAS_RESET count=2\n"
            ),
        ]

        baseline = smoke.renderer_atlas_reset_events(device, "daily_glyph_atlas")
        with _patched_clock(clock):
            smoke.wait_for_new_atlas_reset(
                device,
                baseline,
                "daily_glyph_atlas",
                timeout=2.0,
            )

        self.assertEqual(baseline, 1)
        self.assertEqual(device.run.call_count, 3)
        self.assertEqual(device.assert_foreground.call_count, 2)

    def test_screenshot_skips_capture_when_another_app_is_focused(self) -> None:
        device = smoke.AndroidDevice("emulator-5554", "adb")
        device.run = mock.Mock(return_value=b"mCurrentFocus=Window{other.app/.Main}")
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "screen.png"
            completed = []
            reason = smoke.capture_optional_screenshot(device, output, completed, "daily")
            self.assertEqual(reason, "app_not_foreground")
            self.assertEqual(completed, ["daily_screenshot_unavailable"])
            self.assertFalse(output.exists())
            self.assertTrue(device.foreground_evidence_lost)
        self.assertEqual(device.run.call_count, 1)

    def test_screenshot_drops_pixels_if_focus_changes_during_capture(self) -> None:
        device = smoke.AndroidDevice("emulator-5554", "adb")
        device.run = mock.Mock(side_effect=[
            b"mCurrentFocus=Window{dev.meeterm.app/.MainActivity}",
            smoke.PNG_SIGNATURE,
            b"mCurrentFocus=Window{other.app/.Main}",
        ])
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "screen.png"
            completed = []
            reason = smoke.capture_optional_screenshot(device, output, completed, "daily")
            self.assertEqual(reason, "app_not_foreground")
            self.assertFalse(output.exists())
            self.assertTrue(device.foreground_evidence_lost)

    def test_failure_screenshot_waits_until_both_credential_forms_are_closed(self) -> None:
        self.assertFalse(smoke.failure_screenshot_is_secret_safe([]))
        self.assertFalse(
            smoke.failure_screenshot_is_secret_safe(["form_submitted"])
        )
        self.assertFalse(
            smoke.failure_screenshot_is_secret_safe(["daily_profile_switched"])
        )
        self.assertTrue(
            smoke.failure_screenshot_is_secret_safe(
                ["daily_second_profile_saved"]
            )
        )
        self.assertTrue(
            smoke.failure_screenshot_is_secret_safe(["terminal_focused"])
        )

    def test_recording_is_removed_without_pull_after_detected_focus_loss(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.foreground_evidence_lost = True
        device.run.side_effect = [b"", smoke.SmokeFailure("wait", "adb_failed"), b""]
        with tempfile.TemporaryDirectory() as root:
            output = Path(root) / "daily.mp4"
            self.assertEqual(smoke.start_optional_screenrecord(device, output), (None, "foreground_lost"))
            device.run.assert_not_called()
            recording = smoke.ScreenRecording(4312, smoke.SCREENRECORD_REMOTE_PATH, output)
            self.assertEqual(smoke.finish_optional_screenrecord(device, recording), "foreground_lost")
            self.assertFalse(output.exists())
        arguments = [call.args[0] for call in device.run.call_args_list]
        self.assertFalse(any(args[0] == "pull" for args in arguments))
        self.assertIn(("shell", "kill", "-2", "4312"), arguments)
        self.assertTrue(any(args[:3] == ("shell", "rm", "-f") for args in arguments))


class DailyAcceptanceFlowTests(unittest.TestCase):
    @staticmethod
    def profile_node(name: str, *, selected: bool = False) -> smoke.Node:
        return smoke.Node(
            "",
            f"Connect saved server {name}",
            "android.view.View",
            (0, 100, 800, 220),
            selected=selected,
        )

    def test_saved_profile_management_runs_before_secret_safe_return(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        events: list[tuple[str, ...]] = []

        def wait_profile(
            _device: object,
            stage: str,
            name: str,
            *,
            selected: bool | None = None,
            timeout: float = smoke.RECONNECT_TIMEOUT,
        ) -> smoke.Node:
            del timeout
            events.append(("profile", stage, name, str(selected)))
            return self.profile_node(name, selected=bool(selected))

        def action(
            _device: object,
            stage: str,
            labels: tuple[str, ...],
            *,
            timeout: float = smoke.DEFAULT_UI_TIMEOUT,
        ) -> smoke.Node:
            del timeout
            events.append(("action", stage, labels[0]))
            return smoke.Node("", labels[0], "android.widget.Button", (0, 0, 100, 100))

        def fill(
            _device: object,
            label: str,
            _value: str,
            stage: str,
            **_kwargs: object,
        ) -> None:
            events.append(("fill", stage, label))

        def key_entry(
            _device: object,
            _key: str,
            *,
            return_from_form_end: bool = False,
        ) -> None:
            self.assertTrue(return_from_form_end)
            events.append(("credential", "entered"))

        boundary_results = [
            smoke.PROFILE_SWITCH_SESSION_SWITCHER_BRANCH,
            smoke.PROFILE_SWITCH_SESSION_SWITCHER_BRANCH,
        ]

        def wait_boundary(
            _device: object,
            stage: str,
            _name: str,
            *,
            timeout: float = smoke.RECONNECT_TIMEOUT,
        ) -> str:
            del timeout
            events.append(("boundary", stage, boundary_results[0]))
            return boundary_results.pop(0)

        def wait_node(
            _device: object,
            stage: str,
            **selectors: object,
        ) -> smoke.Node:
            label = str(
                selectors.get("content_description")
                or selectors.get("text")
                or "public-control"
            )
            events.append(("wait_node", stage, label))
            return smoke.Node(
                str(selectors.get("text") or ""),
                str(selectors.get("content_description") or ""),
                "android.view.View",
                (0, 0, 100, 100),
            )

        completed: list[str] = []
        with (
            mock.patch.object(smoke, "wait_for_saved_profile", side_effect=wait_profile),
            mock.patch.object(smoke, "tap_action", side_effect=action),
            mock.patch.object(
                smoke,
                "wait_for_node",
                side_effect=wait_node,
            ) as wait_node_mock,
            mock.patch.object(smoke, "tap_node") as tap_node,
            mock.patch.object(smoke, "fill_field", side_effect=fill),
            mock.patch.object(smoke, "set_toggle") as set_toggle,
            mock.patch.object(smoke, "fill_multiline_key", side_effect=key_entry),
            mock.patch.object(smoke, "wait_for_saved_profile_absent") as wait_absent,
            mock.patch.object(smoke, "capture_optional_screenshot") as capture,
            mock.patch.object(smoke, "wait_for_workspace") as wait_workspace,
            mock.patch.object(
                smoke,
                "wait_for_profile_switch_boundary",
                side_effect=wait_boundary,
            ) as wait_boundary_mock,
        ):
            smoke.exercise_saved_profile_management(
                device,
                "127.0.0.1",
                2222,
                "fixture",
                "fixture-key",
                completed,
            )

        self.assertEqual(
            completed,
            [
                "daily_profile_edited",
                "daily_second_profile_saved",
                "daily_profile_switch_second_boundary_session_switcher",
                "daily_profile_switched",
                "daily_profile_switch_primary_boundary_session_switcher",
                "daily_profile_switch_restored",
                "daily_profile_delete_cancelled",
                "daily_second_profile_deleted",
            ],
        )
        self.assertIn(("fill", "daily_profile_edit", "Server name"), events)
        self.assertIn(("fill", "daily_profile_edit_restore", "Server name"), events)
        self.assertIn(("fill", "daily_profile_add_host", "Host"), events)
        self.assertIn(("fill", "daily_profile_add_port", "Port"), events)
        self.assertIn(("fill", "daily_profile_add_username", "Username"), events)
        self.assertIn(("fill", "daily_profile_add_name", "Server name"), events)
        credential_index = events.index(("credential", "entered"))
        second_save_index = events.index(
            ("action", "daily_profile_add", "Save server")
        )
        first_switch_index = events.index(
            (
                "boundary",
                "daily_profile_switch_second",
                smoke.PROFILE_SWITCH_SESSION_SWITCHER_BRANCH,
            )
        )
        self.assertLess(credential_index, second_save_index)
        self.assertLess(second_save_index, first_switch_index)
        self.assertEqual(
            [
                event
                for event in events
                if event[0] == "action"
                and event[2] in {"Server connection", "Saved servers"}
            ],
            [
                ("action", "daily_profile_management_open", "Server connection"),
                ("action", "daily_profile_management_open", "Saved servers"),
                ("action", "daily_profile_switch_second", "Server connection"),
                ("action", "daily_profile_switch_second", "Saved servers"),
                ("action", "daily_profile_switch_primary", "Server connection"),
                ("action", "daily_profile_switch_primary", "Saved servers"),
            ],
        )
        self.assertEqual(wait_boundary_mock.call_count, 2)
        self.assertFalse(
            any(
                "Choose a session" in marker
                or marker in {
                    smoke.DAILY_SECOND_PROFILE_NAME,
                    smoke.DAILY_PROFILE_NAME,
                }
                for marker in completed
            )
        )
        self.assertIn(("action", "daily_profile_delete_cancel", "Cancel"), events)
        self.assertEqual(
            [event for event in events if event[0] == "action" and event[2] == "Remove"],
            [
                ("action", "daily_profile_delete_cancel", "Remove"),
                ("action", "daily_profile_delete_confirm", "Remove"),
                ("action", "daily_profile_delete_confirm", "Remove"),
            ],
        )
        set_toggle.assert_called_once_with(
            device,
            "Save credentials securely",
            True,
            "daily_profile_add_credential_toggle",
            scroll_gutter=True,
        )
        wait_absent.assert_called_once_with(
            device,
            "daily_profile_delete_confirm",
            smoke.DAILY_SECOND_PROFILE_NAME,
            smoke.DAILY_PROFILE_NAME,
        )
        capture.assert_not_called()
        self.assertEqual(
            [
                call
                for call in wait_node_mock.call_args_list
                if call.kwargs.get("content_description", "").startswith(
                    "tmux session "
                )
            ],
            [
                mock.call(
                    device,
                    "daily_profile_switch_second_runtime_selection",
                    content_description=(
                        "tmux session meeterm on Android daily second "
                        "(fixture@127.0.0.1:2222)"
                    ),
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
                mock.call(
                    device,
                    "daily_profile_switch_primary_runtime_selection",
                    content_description=(
                        "tmux session meeterm on Android daily fixture "
                        "(fixture@127.0.0.1:2222)"
                    ),
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
            ],
        )
        selected_rows = [
            call
            for call in tap_node.call_args_list
            if call.args[2]
            in {
                "daily_profile_switch_second_runtime_selection",
                "daily_profile_switch_primary_runtime_selection",
            }
        ]
        self.assertEqual(
            [call.args[1].content_description for call in selected_rows],
            [
                "tmux session meeterm on Android daily second "
                "(fixture@127.0.0.1:2222)",
                "tmux session meeterm on Android daily fixture "
                "(fixture@127.0.0.1:2222)",
            ],
        )
        self.assertEqual(
            [
                call
                for call in wait_node_mock.call_args_list
                if call.kwargs.get("text") == "Connected"
                and call.args[1].endswith("_runtime_connected")
            ],
            [
                mock.call(
                    device,
                    "daily_profile_switch_second_runtime_connected",
                    text="Connected",
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
                mock.call(
                    device,
                    "daily_profile_switch_primary_runtime_connected",
                    text="Connected",
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
            ],
        )
        self.assertEqual(
            wait_workspace.call_args_list,
            [
                mock.call(
                    device,
                    "daily_profile_switch_second_workspace_ready",
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
                mock.call(
                    device,
                    "daily_profile_switch_primary_workspace_ready",
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
            ],
        )

    def test_handoff_action_scrolls_the_server_sheet_before_tapping(self) -> None:
        clock = _FakeClock()
        scroll = smoke.Node(
            "",
            "",
            "android.widget.ScrollView",
            (0, 100, 1080, 1900),
            scrollable=True,
        )
        handoff = smoke.Node(
            "",
            smoke.HANDOFF_LABELS[0],
            "android.widget.Button",
            (24, 1600, 1056, 1720),
        )
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.side_effect = [[scroll]] * 5 + [[scroll, handoff]]

        with (
            _patched_clock(clock),
            mock.patch.object(smoke, "tap_action") as tap_action,
            mock.patch.object(smoke, "wait_for_text_fragment"),
            mock.patch.object(smoke, "capture_optional_screenshot"),
            mock.patch.object(smoke, "dismiss_handoff"),
        ):
            smoke.open_handoff_and_capture(
                device,
                Path("/tmp/public-artifacts"),
                [],
            )

        tap_action.assert_called_once_with(
            device,
            "terminal_menu",
            smoke.TERMINAL_MENU_LABELS,
        )
        device.input_swipe.assert_called_once_with(
            scroll.bounds,
            "handoff_action",
        )
        device.input_tap.assert_called_once_with(
            *handoff.center,
            "handoff_action",
        )

    def test_removed_profile_must_be_absent_in_settled_retained_list(self) -> None:
        clock = _FakeClock()
        primary = self.profile_node(smoke.DAILY_PROFILE_NAME, selected=True)
        second = self.profile_node(smoke.DAILY_SECOND_PROFILE_NAME)
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.side_effect = [
            [primary, second],
            [primary],
            [primary],
        ]

        with _patched_clock(clock):
            smoke.wait_for_saved_profile_absent(
                device,
                "daily_profile_delete_confirm",
                smoke.DAILY_SECOND_PROFILE_NAME,
                smoke.DAILY_PROFILE_NAME,
            )

        self.assertEqual(device.dump_ui.call_count, 3)

    def test_cold_restart_rechecks_that_only_the_fixture_primary_remains(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.process_id.return_value = "9000"
        saved_servers = smoke.Node(
            "",
            "Saved servers",
            "android.widget.Button",
            (0, 0, 100, 100),
        )
        primary = self.profile_node(smoke.DAILY_PROFILE_NAME)
        completed: list[str] = []

        with (
            mock.patch.object(
                smoke,
                "wait_for_node",
                side_effect=[saved_servers, primary],
            ),
            mock.patch.object(smoke, "tap_node"),
            mock.patch.object(smoke, "wait_for_saved_profile_absent") as wait_absent,
            mock.patch.object(smoke, "wait_for_text_fragment"),
            mock.patch.object(smoke, "capture_optional_screenshot"),
            mock.patch.object(
                smoke,
                "select_fixture_tmux_runtime_and_wait_for_connected",
            ) as select_runtime,
        ):
            restarted = smoke.reconnect_saved_profile_after_restart(
                device,
                Path("/tmp/public-artifacts"),
                completed,
                "4312",
            )

        self.assertEqual(restarted, "9000")
        wait_absent.assert_called_once_with(
            device,
            "daily_saved_profile",
            smoke.DAILY_SECOND_PROFILE_NAME,
            smoke.DAILY_PROFILE_NAME,
        )
        self.assertEqual(
            completed,
            [
                "daily_process_restarted",
                "daily_profile_and_credential_restored",
                "daily_saved_profile_connected",
            ],
        )
        select_runtime.assert_called_once()
        self.assertEqual(
            select_runtime.call_args.args[:2],
            (device, "daily_saved_profile_connect_runtime_selection"),
        )
        self.assertIs(select_runtime.call_args.args[2], completed)

    def test_home_foreground_requires_the_resolved_launcher(self) -> None:
        output = b"com.google.android.apps.nexuslauncher/.NexusLauncherActivity\n"
        expected = smoke.resolved_home_component(output)
        self.assertEqual(
            expected,
            "com.google.android.apps.nexuslauncher/"
            "com.google.android.apps.nexuslauncher.NexusLauncherActivity",
        )
        focused = (
            b"mCurrentFocus=Window{123 u0 com.google.android.apps.nexuslauncher/"
            b"com.google.android.apps.nexuslauncher.NexusLauncherActivity}\n"
        )
        self.assertEqual(smoke.focused_window_component(focused), expected)

        device = mock.Mock(spec=smoke.AndroidDevice)
        device.foreground_evidence_lost = False
        device.run.return_value = b"mCurrentFocus=Window{123 u0 other.app/.Main}\n"
        with self.assertRaises(smoke.SmokeFailure) as error:
            smoke.wait_for_home_foreground(device, expected, "daily_foreground_home")
        self.assertEqual(error.exception.reason, "unexpected_foreground")
        self.assertTrue(device.foreground_evidence_lost)

    def test_foreground_return_keeps_pid_and_requires_native_terminal_ack(self) -> None:
        fixture = smoke.parse_tmux_panes(
            b"@4\tsmoke\t%12\t1201\t0\t1\t80\t24\t0\t0\t79\t23\t1\t0\n"
        )
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.foreground_evidence_lost = False
        device.process_id.return_value = "4312"
        device.run.return_value = b""
        workspace = smoke.Node("", "Workspace smoke", "android.view.View", (0, 0, 100, 100))
        terminal = smoke.Node("", "Terminal", "android.view.SurfaceView", (0, 100, 100, 300))
        connected = smoke.Node("Connected", "", "android.widget.TextView", (0, 0, 100, 40))
        pane = smoke.Node(
            "",
            "",
            "android.widget.Button",
            (0, 40, 100, 100),
            resource_id=f"{smoke.PACKAGE}:id/terminal-tab-%12",
            selected=True,
        )
        device.dump_ui.return_value = [connected, pane, terminal]
        completed: list[str] = []
        marker = Path("/tmp/meeterm-ssh-fixture-test/foreground.txt")

        with (
            mock.patch.object(smoke, "wait_for_workspace", return_value=workspace),
            mock.patch.object(smoke, "tap_node"),
            mock.patch.object(smoke, "wait_for_pane"),
            mock.patch.object(smoke, "wait_for_labeled_terminal_surface", return_value=terminal),
            mock.patch.object(smoke, "configured_home_component", return_value="launcher.app/.Home"),
            mock.patch.object(smoke, "wait_for_home_foreground") as wait_home,
            mock.patch.object(smoke, "wait_for_node"),
            mock.patch.object(smoke, "focus_terminal") as focus,
            mock.patch.object(smoke, "terminal_line") as terminal_line,
            mock.patch.object(smoke, "wait_for_file_contents") as wait_marker,
            mock.patch.object(smoke, "tap_action"),
        ):
            smoke.exercise_foreground_return(
                device,
                fixture,
                marker,
                "fresh-ack",
                completed,
                "4312",
            )

        device.input_keyevent.assert_called_once_with(
            smoke.KEYCODE_HOME,
            "daily_foreground_home",
        )
        wait_home.assert_called_once_with(
            device,
            "launcher.app/.Home",
            "daily_foreground_home",
            timeout=smoke.DEFAULT_UI_TIMEOUT,
        )
        self.assertEqual(
            device.run.call_args_list,
            [
                mock.call(
                    (
                        "shell",
                        "am",
                        "start",
                        "-W",
                        "-n",
                        f"{smoke.PACKAGE}/.MainActivity",
                    ),
                    "daily_foreground_return",
                    timeout=15.0,
                )
            ],
        )
        terminal_line.assert_called_once_with(
            device,
            smoke.session_marker_command("fresh-ack", marker, 1201, append=True),
        )
        wait_marker.assert_called_once_with(
            marker,
            "fresh-ack:1201\n",
            "daily_foreground_return",
        )
        focus.assert_called_once_with(device, terminal, "daily_foreground_return")
        self.assertEqual(
            completed,
            [
                "daily_app_backgrounded",
                "daily_foreground_authoritative_ready",
                "daily_foreground_native_binding_verified",
                "daily_foreground_marker_exactly_once",
                "daily_foreground_terminal_resumed",
            ],
        )
        self.assertFalse(device.foreground_evidence_lost)

    def test_foreground_recovery_ready_rejects_picker_and_cached_terminal(self) -> None:
        picker = smoke.Node(
            "Choose a session",
            "",
            "android.widget.TextView",
            (0, 0, 100, 40),
        )
        self.assertTrue(smoke.runtime_picker_is_visible([picker]))
        self.assertFalse(smoke.foreground_recovery_ready([picker], "%12"))
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = [picker]
        with self.assertRaises(smoke.SmokeFailure) as error:
            smoke.wait_for_foreground_recovery_ready(
                device,
                "daily_foreground_return",
                "%12",
                timeout=1.0,
            )
        self.assertEqual(
            (error.exception.stage, error.exception.reason),
            ("daily_foreground_return", "runtime_picker_reappeared"),
        )

        connected = smoke.Node("Connected", "", "android.widget.TextView", (0, 0, 100, 40))
        pane = smoke.Node(
            "",
            "",
            "android.widget.Button",
            (0, 40, 100, 100),
            resource_id=f"{smoke.PACKAGE}:id/terminal-tab-%12",
            selected=True,
        )
        cached_terminal = smoke.Node(
            "",
            "Terminal, cached output, read only",
            "android.view.SurfaceView",
            (0, 100, 100, 300),
        )
        self.assertFalse(
            smoke.foreground_recovery_ready([connected, pane, cached_terminal], "%12")
        )

    def test_foreground_return_rejects_a_restarted_process_before_ack(self) -> None:
        fixture = smoke.parse_tmux_panes(
            b"@4\tsmoke\t%12\t1201\t0\t1\t80\t24\t0\t0\t79\t23\t1\t0\n"
        )
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.foreground_evidence_lost = False
        device.process_id.side_effect = ["4312", "4312", "9000"]
        device.run.return_value = b""
        node = smoke.Node("", "fixture", "android.view.View", (0, 0, 100, 100))
        connected = smoke.Node("Connected", "", "android.widget.TextView", (0, 0, 100, 40))
        pane = smoke.Node(
            "",
            "",
            "android.widget.Button",
            (0, 40, 100, 100),
            resource_id=f"{smoke.PACKAGE}:id/terminal-tab-%12",
            selected=True,
        )
        terminal = smoke.Node("", "Terminal", "android.view.SurfaceView", (0, 100, 100, 300))
        device.dump_ui.return_value = [connected, pane, terminal]
        with (
            mock.patch.object(smoke, "wait_for_workspace", return_value=node),
            mock.patch.object(smoke, "tap_node"),
            mock.patch.object(smoke, "wait_for_pane"),
            mock.patch.object(smoke, "wait_for_labeled_terminal_surface", return_value=node),
            mock.patch.object(smoke, "configured_home_component", return_value="launcher.app/.Home"),
            mock.patch.object(smoke, "wait_for_home_foreground"),
            mock.patch.object(smoke, "wait_for_node"),
            mock.patch.object(smoke, "terminal_line") as terminal_line,
        ):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.exercise_foreground_return(
                    device,
                    fixture,
                    Path("/tmp/meeterm-ssh-fixture-test/foreground.txt"),
                    "fresh-ack",
                    [],
                    "4312",
                )
        self.assertEqual(
            (error.exception.stage, error.exception.reason),
            ("daily_foreground_return", "app_process_changed"),
        )
        terminal_line.assert_not_called()


class ProfileSwitchBoundaryTests(unittest.TestCase):
    PROFILE_NAME = "Android daily second"
    HOST = "127.0.0.1"
    PORT = 2222
    USERNAME = "fixture"

    @staticmethod
    def confirmation_node() -> smoke.Node:
        return smoke.Node(
            "Switch servers?",
            "",
            "android.widget.TextView",
            (0, 0, 400, 80),
        )

    @staticmethod
    def picker_heading(name: str) -> smoke.Node:
        return smoke.Node(
            f"{smoke.RUNTIME_PICKER_HEADING_PREFIX}{name}",
            "",
            "android.widget.TextView",
            (0, 0, 800, 120),
        )

    @staticmethod
    def picker_row() -> smoke.Node:
        return smoke.Node(
            "",
            smoke.TMUX_RUNTIME_LABELS[0],
            "android.widget.Button",
            (0, 120, 800, 240),
        )

    @classmethod
    def session_row(
        cls,
        *,
        name: str | None = None,
        backend: str = "tmux",
        enabled: bool = True,
        visible: bool = True,
    ) -> smoke.Node:
        profile_name = name or cls.PROFILE_NAME
        return smoke.Node(
            "",
            f"{backend} session meeterm on {profile_name} "
            f"({cls.USERNAME}@{cls.HOST}:{cls.PORT})",
            "android.widget.Button",
            (0, 120, 800, 240),
            enabled=enabled,
            visible_to_user=visible,
        )

    @staticmethod
    def profile_node() -> smoke.Node:
        return smoke.Node(
            "",
            f"Connect saved server {ProfileSwitchBoundaryTests.PROFILE_NAME}",
            "android.view.View",
            (0, 100, 800, 220),
        )

    def test_session_switcher_branch_selects_exact_session_before_ready(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = [self.session_row()]
        target_label = self.session_row().content_description

        def wait_node(
            _device: object,
            _stage: str,
            *,
            text: str | None = None,
            content_description: str | None = None,
            timeout: float = smoke.DEFAULT_UI_TIMEOUT,
        ) -> smoke.Node:
            del timeout
            return smoke.Node(
                text or "",
                content_description or "",
                "android.widget.Button",
                (0, 120, 800, 240),
            )

        with (
            mock.patch.object(
                smoke,
                "wait_for_saved_profile",
                side_effect=[self.profile_node(), self.profile_node()],
            ),
            mock.patch.object(smoke, "tap_node") as tap_node,
            mock.patch.object(smoke, "tap_action") as tap_action,
            mock.patch.object(smoke, "wait_for_node", side_effect=wait_node) as wait_node_mock,
            mock.patch.object(smoke, "wait_for_workspace"),
        ):
            branch = smoke.switch_saved_profile(
                device,
                self.PROFILE_NAME,
                self.HOST,
                self.PORT,
                self.USERNAME,
                "daily_profile_switch_second",
            )

        self.assertEqual(branch, smoke.PROFILE_SWITCH_SESSION_SWITCHER_BRANCH)
        self.assertEqual(tap_node.call_count, 2)
        self.assertEqual(
            [call.args[1].content_description for call in tap_node.call_args_list],
            [
                f"Connect saved server {self.PROFILE_NAME}",
                target_label,
            ],
        )
        self.assertEqual(
            [call.args[2] for call in tap_node.call_args_list],
            [
                "daily_profile_switch_second",
                "daily_profile_switch_second_runtime_selection",
            ],
        )
        self.assertEqual(device.dump_ui.call_count, 1)
        self.assertEqual(
            wait_node_mock.call_args_list,
            [
                mock.call(
                    device,
                    "daily_profile_switch_second_runtime_selection",
                    content_description=target_label,
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
                mock.call(
                    device,
                    "daily_profile_switch_second_runtime_connected",
                    text="Connected",
                    timeout=smoke.RECONNECT_TIMEOUT,
                ),
            ],
        )
        self.assertEqual(
            tap_action.call_args_list,
            [
                mock.call(device, "daily_profile_switch_second", smoke.SERVER_CONNECTION_LABELS),
                mock.call(device, "daily_profile_switch_second", ("Saved servers",)),
            ],
        )

    def test_herdr_session_row_is_also_a_session_switcher_boundary(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = [self.session_row(backend="Herdr")]
        self.assertEqual(
            smoke.wait_for_profile_switch_boundary(
                device,
                "daily_profile_switch_second",
                self.PROFILE_NAME,
            ),
            smoke.PROFILE_SWITCH_SESSION_SWITCHER_BRANCH,
        )

    def test_unexpected_picker_and_confirmation_fail_closed(self) -> None:
        surfaces = (
            (
                [self.picker_heading(self.PROFILE_NAME), self.picker_row()],
                smoke.PROFILE_SWITCH_BOUNDARY_UNEXPECTED_PICKER,
            ),
            (
                [self.confirmation_node()],
                smoke.PROFILE_SWITCH_BOUNDARY_UNEXPECTED_CONFIRMATION,
            ),
        )
        for nodes, expected_reason in surfaces:
            with self.subTest(reason=expected_reason):
                device = mock.Mock(spec=smoke.AndroidDevice)
                device.dump_ui.return_value = nodes
                with (
                    mock.patch.object(
                        smoke,
                        "wait_for_saved_profile",
                        return_value=self.profile_node(),
                    ),
                    mock.patch.object(smoke, "tap_node"),
                    mock.patch.object(smoke, "wait_for_node") as wait_node,
                ):
                    with self.assertRaises(smoke.SmokeFailure) as error:
                        smoke.switch_saved_profile(
                            device,
                            self.PROFILE_NAME,
                            self.HOST,
                            self.PORT,
                            self.USERNAME,
                            "daily_profile_switch_second",
                        )
                self.assertEqual(error.exception.reason, expected_reason)
                wait_node.assert_not_called()

    def test_simultaneous_session_switcher_and_removed_surface_is_ambiguous(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = [
            self.confirmation_node(),
            self.session_row(),
        ]
        with self.assertRaises(smoke.SmokeFailure) as error:
            smoke.wait_for_profile_switch_boundary(
                device,
                "daily_profile_switch_second",
                self.PROFILE_NAME,
            )
        self.assertEqual(error.exception.reason, smoke.PROFILE_SWITCH_BOUNDARY_AMBIGUOUS)

    def test_boundary_timeout_uses_fixed_reason_without_dynamic_profile_data(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = []
        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.wait_for_profile_switch_boundary(
                    device,
                    "daily_profile_switch_second",
                    self.PROFILE_NAME,
                )
        self.assertEqual(error.exception.reason, smoke.PROFILE_SWITCH_BOUNDARY_TIMEOUT)
        self.assertNotIn(self.PROFILE_NAME, error.exception.stage)
        self.assertNotIn(self.PROFILE_NAME, error.exception.reason)
        self.assertNotIn(self.PROFILE_NAME, str(error.exception))


class TerminalSurfaceBindingTests(unittest.TestCase):
    @staticmethod
    def surface(
        bounds: tuple[int, int, int, int],
        *,
        class_name: str = "dev.meeterm.terminal.MeetermTerminalView",
        resource_id: str = "",
    ) -> smoke.Node:
        return smoke.Node(
            "raw surface text",
            "raw surface label",
            class_name,
            bounds,
            resource_id=resource_id,
        )

    def test_same_class_and_empty_resource_ids_allow_vertical_resize(self) -> None:
        before = self.surface((0, 100, 1080, 900))
        after = self.surface((0, 520, 1080, 1900))

        self.assertTrue(smoke.same_terminal_surface_binding(before, after))

    def test_same_resource_id_allows_changed_bounds(self) -> None:
        before = self.surface(
            (0, 100, 1080, 900),
            resource_id="dev.meeterm.app:id/terminal-surface",
        )
        after = self.surface(
            (0, 520, 1080, 1900),
            resource_id="dev.meeterm.app:id/terminal-surface",
        )

        self.assertTrue(smoke.same_terminal_surface_binding(before, after))

    def test_different_nonempty_resource_ids_are_different_bindings(self) -> None:
        before = self.surface(
            (0, 100, 1080, 900),
            resource_id="dev.meeterm.app:id/terminal-surface-before",
        )
        after = self.surface(
            (0, 520, 1080, 1900),
            resource_id="dev.meeterm.app:id/terminal-surface-after",
        )

        self.assertFalse(smoke.same_terminal_surface_binding(before, after))

    def test_one_sided_resource_id_uses_class_fallback(self) -> None:
        matching_cases = (
            (
                self.surface(
                    (0, 100, 1080, 900),
                    resource_id="dev.meeterm.app:id/terminal-surface",
                ),
                self.surface((0, 520, 1080, 1900)),
            ),
            (
                self.surface((0, 100, 1080, 900)),
                self.surface(
                    (0, 520, 1080, 1900),
                    resource_id="dev.meeterm.app:id/terminal-surface",
                ),
            ),
        )
        for before, after in matching_cases:
            with self.subTest(before=bool(before.resource_id), after=bool(after.resource_id)):
                self.assertTrue(smoke.same_terminal_surface_binding(before, after))

        mismatching_cases = (
            (
                self.surface(
                    (0, 100, 1080, 900),
                    resource_id="dev.meeterm.app:id/terminal-surface",
                ),
                self.surface(
                    (0, 520, 1080, 1900),
                    class_name="android.view.SurfaceView",
                ),
            ),
            (
                self.surface((0, 100, 1080, 900), class_name="android.view.SurfaceView"),
                self.surface(
                    (0, 520, 1080, 1900),
                    resource_id="dev.meeterm.app:id/terminal-surface",
                ),
            ),
        )
        for before, after in mismatching_cases:
            with self.subTest(before=bool(before.resource_id), after=bool(after.resource_id)):
                self.assertFalse(smoke.same_terminal_surface_binding(before, after))


class TransportLossTests(unittest.TestCase):
    def fixture_nodes(self, *, picker: bool = False) -> list[smoke.Node]:
        nodes = [
            smoke.Node(
                "",
                "",
                "android.widget.Button",
                (0, 40, 100, 100),
                resource_id=f"{smoke.PACKAGE}:id/terminal-tab-%12",
                enabled=False,
                selected=True,
            ),
            smoke.Node(
                "",
                "Terminal, cached output, read only",
                "android.view.SurfaceView",
                (0, 100, 100, 300),
                enabled=False,
            ),
        ]
        for identifier in ("recovery-rail", "recovery-title", "recovery-detail", "recovery-meta"):
            nodes.append(
                smoke.Node(
                    "",
                    "",
                    "android.view.View",
                    (0, 0, 100, 40),
                    resource_id=f"{smoke.PACKAGE}:id/{identifier}",
                )
            )
        if picker:
            nodes.append(smoke.Node("Choose a session", "", "android.widget.TextView", (0, 0, 100, 20)))
        return nodes

    def test_stale_recovery_requires_cached_surface_rail_and_disabled_same_pane(self) -> None:
        nodes = self.fixture_nodes()
        self.assertTrue(smoke.transport_loss_recovery_ready(nodes, "%12"))
        self.assertIsNotNone(smoke.find_recovery_pane_node(nodes, "%12"))
        self.assertFalse(smoke.transport_loss_recovery_ready(self.fixture_nodes(picker=True), "%12"))

    def test_stale_diagnostic_uses_only_fixed_predicates(self) -> None:
        nodes = self.fixture_nodes()
        cached = smoke.find_cached_terminal_surface(nodes)
        self.assertIsNotNone(cached)
        assert cached is not None
        cached.bounds = (0, 500, 100, 900)
        initial = smoke.Node(
            "raw-terminal-text",
            "raw-terminal-label",
            cached.class_name,
            (10, 20, 110, 220),
            resource_id="raw-terminal-resource-id",
        )

        diagnostic = smoke.stale_read_only_diagnostic(nodes, initial, "%12")
        records = dict(line.split("=", 1) for line in diagnostic.splitlines())

        self.assertEqual(set(records), set(smoke.STALE_READ_ONLY_DIAGNOSTIC_KEYS))
        self.assertTrue(
            set(records.values()).issubset({"yes", "no", "unknown"})
        )
        self.assertEqual(records["runtime_picker_hidden"], "yes")
        self.assertEqual(records["cached_surface_visible"], "yes")
        self.assertEqual(records["surface_class_match"], "yes")
        self.assertEqual(records["surface_resource_id_match"], "unknown")
        for identifier in smoke.RECOVERY_TEST_IDS:
            self.assertEqual(records[f"{identifier.replace('-', '_')}_visible"], "yes")
        self.assertEqual(records["expected_selected_pane_visible"], "yes")
        for sensitive in (
            "raw-terminal-text",
            "raw-terminal-label",
            "raw-terminal-resource-id",
            "10,20,110,220",
            "%12",
        ):
            self.assertNotIn(sensitive, diagnostic)

    def test_stale_timeout_writes_fixed_diagnostic_artifact(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        nodes = [
            node
            for node in self.fixture_nodes()
            if node.resource_id != f"{smoke.PACKAGE}:id/recovery-meta"
        ]
        device.dump_ui.return_value = nodes
        initial = smoke.Node(
            "",
            "Terminal",
            "android.view.SurfaceView",
            (0, 100, 100, 300),
        )

        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            artifact_dir = Path(root)
            with _patched_clock(clock):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.wait_for_transport_loss_stale(
                        device,
                        "daily_transport_loss_stale",
                        "%12",
                        initial,
                        timeout=1.0,
                        artifact_dir=artifact_dir,
                    )

            self.assertEqual(
                (error.exception.stage, error.exception.reason),
                ("daily_transport_loss_stale", "stale_read_only_timeout"),
            )
            diagnostic_path = artifact_dir / smoke.STALE_READ_ONLY_DIAGNOSTIC_NAME
            self.assertTrue(diagnostic_path.is_file())
            records = dict(
                line.split("=", 1)
                for line in diagnostic_path.read_text(encoding="utf-8").splitlines()
            )
            self.assertEqual(records["surface_class_match"], "yes")
            self.assertEqual(records["surface_resource_id_match"], "unknown")
            self.assertEqual(records["recovery_meta_visible"], "no")

    def test_transport_marker_command_is_ascii_and_avoids_android_percent_escape(self) -> None:
        command = smoke.transport_loss_marker_command(
            "android-ssh-loss-pre-0123456789abcdef",
            Path("/tmp/meeterm-ssh-fixture-test/loss-marker"),
            "%12",
        )
        self.assertNotIn("%", command)
        self.assertNotIn("\n", command)
        self.assertIn("android-ssh-loss-pre-0123456789abcdef:12:", command)
        self.assertIn('"$$"', command)
        self.assertIn(" > ", command)
        appended = smoke.transport_loss_marker_command(
            "android-ssh-loss-post-0123456789abcdef",
            Path("/tmp/meeterm-ssh-fixture-test/loss-marker"),
            "%12",
            append=True,
        )
        self.assertIn(" >> ", appended)

    def test_transport_marker_command_rejects_wrong_pane_or_marker(self) -> None:
        with self.assertRaises(smoke.SmokeFailure) as marker_error:
            smoke.transport_loss_marker_command("not-a-loss-marker", Path("/tmp/x"), "%12")
        self.assertEqual(marker_error.exception.reason, "invalid_marker")
        with self.assertRaises(smoke.SmokeFailure) as pane_error:
            smoke.transport_loss_marker_command(
                "android-ssh-loss-pre-0123456789abcdef", Path("/tmp/x"), "%bad"
            )
        self.assertEqual(pane_error.exception.reason, "invalid_pane_id")

    def test_transport_marker_scope_rejects_duplicate_and_other_pane_output(self) -> None:
        records = smoke.parse_tmux_panes(
            b"@4\tsmoke\t%12\t1201\t0\t1\t40\t24\t0\t0\t39\t23\t1\t0\n"
            b"@4\tsmoke\t%13\t1202\t1\t0\t40\t24\t40\t0\t79\t23\t1\t0\n"
            b"@5\thandoff\t%14\t1203\t0\t1\t80\t24\t0\t0\t79\t23\t0\t0\n"
            b"@5\thandoff\t%15\t1204\t1\t0\t80\t24\t0\t0\t79\t23\t0\t0\n"
        )
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            marker = Path(root) / "marker"
            marker.write_text(
                "android-ssh-loss-pre-0123456789abcdef:12:1201\n"
                "android-ssh-loss-post-0123456789abcdef:12:1201\n",
                encoding="utf-8",
            )
            with (
                mock.patch.object(smoke, "list_tmux_panes", return_value=records),
                mock.patch.object(
                    smoke,
                    "run_tmux_command",
                    return_value=subprocess.CompletedProcess([], 0, b"clean\n"),
                ),
                mock.patch.object(smoke.time, "sleep"),
            ):
                smoke.validate_transport_loss_marker_scope(
                    Path(root) / "tmux.sock",
                    "%12",
                    marker,
                    "android-ssh-loss-pre-0123456789abcdef",
                    "android-ssh-loss-post-0123456789abcdef",
                    "transport_loss_scope",
                )

            marker.write_text(marker.read_text(encoding="utf-8") + "duplicate\n", encoding="utf-8")
            with self.assertRaises(smoke.SmokeFailure) as duplicate_error:
                smoke.validate_transport_loss_marker_scope(
                    Path(root) / "tmux.sock",
                    "%12",
                    marker,
                    "android-ssh-loss-pre-0123456789abcdef",
                    "android-ssh-loss-post-0123456789abcdef",
                    "transport_loss_scope",
                )
            self.assertEqual(duplicate_error.exception.reason, "marker_repeated")

    def test_transport_marker_scope_rejects_marker_in_another_pane(self) -> None:
        records = smoke.parse_tmux_panes(
            b"@4\tsmoke\t%12\t1201\t0\t1\t40\t24\t0\t0\t39\t23\t1\t0\n"
            b"@4\tsmoke\t%13\t1202\t1\t0\t40\t24\t40\t0\t79\t23\t1\t0\n"
            b"@5\thandoff\t%14\t1203\t0\t1\t80\t24\t0\t0\t79\t23\t0\t0\n"
            b"@5\thandoff\t%15\t1204\t1\t0\t80\t24\t0\t0\t79\t23\t0\t0\n"
        )
        with tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-") as root:
            marker = Path(root) / "marker"
            marker.write_text(
                "android-ssh-loss-pre-0123456789abcdef:12:1201\n"
                "android-ssh-loss-post-0123456789abcdef:12:1201\n",
                encoding="utf-8",
            )
            captures = iter((b"android-ssh-loss-pre-0123456789abcdef\n", b"clean\n", b"clean\n"))
            with (
                mock.patch.object(smoke, "list_tmux_panes", return_value=records),
                mock.patch.object(
                    smoke,
                    "run_tmux_command",
                    side_effect=lambda *args, **kwargs: subprocess.CompletedProcess(
                        [], 0, next(captures)
                    ),
                ),
                mock.patch.object(smoke.time, "sleep"),
            ):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    smoke.validate_transport_loss_marker_scope(
                        Path(root) / "tmux.sock",
                        "%12",
                        marker,
                        "android-ssh-loss-pre-0123456789abcdef",
                        "android-ssh-loss-post-0123456789abcdef",
                        "transport_loss_scope",
                    )
            self.assertEqual(error.exception.reason, "marker_in_other_pane")

    def test_fixture_transport_driver_passes_only_control_paths(self) -> None:
        with mock.patch.dict(
            smoke.os.environ,
            {
                smoke.FIXTURE_CONTROL_REQUEST_ENV: "/tmp/meeterm-ssh-fixture-test/sshd-control-request",
                smoke.FIXTURE_CONTROL_STATUS_ENV: "/tmp/meeterm-ssh-fixture-test/sshd-control-status",
                "MEETERM_SSH_PASSPHRASE": "must-not-be-forwarded",
            },
            clear=False,
        ), mock.patch.object(
            smoke.subprocess,
            "run",
            return_value=subprocess.CompletedProcess([], 0),
        ) as run:
            smoke.request_fixture_transport("stop", "transport_loss_inject")
        self.assertEqual(
            run.call_args.args[0][-2:],
            ["--control", "stop"],
        )
        self.assertEqual(
            run.call_args.kwargs["env"],
            {
                smoke.FIXTURE_CONTROL_REQUEST_ENV: "/tmp/meeterm-ssh-fixture-test/sshd-control-request",
                smoke.FIXTURE_CONTROL_STATUS_ENV: "/tmp/meeterm-ssh-fixture-test/sshd-control-status",
            },
        )
        self.assertNotIn("MEETERM_SSH_PASSPHRASE", run.call_args.kwargs["env"])

    def test_transport_loss_driver_source_has_no_input_between_stop_and_start(self) -> None:
        source = Path(smoke.__file__).read_text(encoding="utf-8")
        start = source.index("def exercise_transport_loss_recovery")
        stop = source.index('request_fixture_transport("stop"', start)
        restore = source.index('request_fixture_transport("start"', stop)
        self.assertNotIn("terminal_line", source[stop:restore])
        self.assertNotIn("input_", source[stop:restore])
        self.assertNotIn('("reverse", "--remove"', source[stop:restore])
        self.assertNotIn("reconnect_transport", source[stop:restore])
        self.assertIn("10.0.2.2", source)
        self.assertIn("wait_for_transport_loss_stale", source[stop:restore])
        completion = source.index(
            'completed.append("daily_transport_loss_complete")',
            restore,
        )
        function_end = source.index("\ndef reconnect_saved_profile_after_restart", restore)
        self.assertLess(completion, function_end)

    def test_foreground_and_transport_loss_calls_match_required_positional_arity(self) -> None:
        tree = ast.parse(Path(smoke.__file__).read_text(encoding="utf-8"))
        call_arities: dict[str, list[int]] = {
            "exercise_foreground_return": [],
            "exercise_transport_loss_recovery": [],
        }
        for node in ast.walk(tree):
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Name)
                and node.func.id in call_arities
            ):
                self.assertEqual(node.keywords, [])
                call_arities[node.func.id].append(len(node.args))

        self.assertEqual(call_arities["exercise_foreground_return"], [6])
        self.assertEqual(call_arities["exercise_transport_loss_recovery"], [9])

    def test_transport_loss_completion_is_exactly_once_and_ordered(self) -> None:
        completed = list(smoke.TRANSPORT_LOSS_COMPLETION_STAGES)
        smoke.require_transport_loss_completion(completed, "transport_loss_complete")
        completed.insert(3, smoke.TRANSPORT_LOSS_COMPLETION_STAGES[0])
        with self.assertRaises(smoke.SmokeFailure) as error:
            smoke.require_transport_loss_completion(completed, "transport_loss_complete")
        self.assertEqual(error.exception.reason, "completion_sequence_invalid")


class RuntimeSelectionTests(unittest.TestCase):
    def test_runtime_selection_taps_exact_fixture_row_even_with_one_candidate(self) -> None:
        self.assertEqual(smoke.TMUX_RUNTIME_LABELS, ("tmux runtime meeterm",))
        device = mock.Mock(spec=smoke.AndroidDevice)
        runtime = smoke.Node(
            "",
            smoke.TMUX_RUNTIME_LABELS[0],
            "android.widget.Button",
            (0, 100, 1080, 260),
        )
        completed: list[str] = []

        with (
            mock.patch.object(
                smoke,
                "wait_for_node_with_labels",
                return_value=runtime,
            ) as wait_picker,
            mock.patch.object(smoke, "tap_node") as tap,
            mock.patch.object(smoke, "wait_for_node") as wait_connected,
        ):
            smoke.select_fixture_tmux_runtime_and_wait_for_connected(
                device,
                "initial_runtime_selection",
                completed,
            )

        wait_picker.assert_called_once_with(
            device,
            "initial_runtime_selection_runtime_picker",
            smoke.TMUX_RUNTIME_LABELS,
            timeout=smoke.RECONNECT_TIMEOUT,
        )
        tap.assert_called_once_with(
            device,
            runtime,
            "initial_runtime_selection_runtime_select_tmux",
        )
        wait_connected.assert_called_once_with(
            device,
            "initial_runtime_selection_runtime_connected",
            text="Connected",
            timeout=smoke.RECONNECT_TIMEOUT,
        )
        self.assertEqual(
            completed,
            [
                "initial_runtime_selection_runtime_picker_ready",
                "initial_runtime_selection_runtime_picker_selected",
                "initial_runtime_selection_runtime_connected",
            ],
        )

    def test_runtime_picker_failure_keeps_a_specific_failure_stage(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        picker_failure = smoke.SmokeFailure("ignored", "ui_timeout")

        with mock.patch.object(
            smoke,
            "wait_for_node_with_labels",
            side_effect=picker_failure,
        ):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.select_fixture_tmux_runtime_and_wait_for_connected(
                    device,
                    "manual_reconnect",
                )

        self.assertEqual(
            (error.exception.stage, error.exception.reason),
            ("manual_reconnect_runtime_picker", "runtime_picker_ui_timeout"),
        )


class _ProbeEditorDevice:
    """Deterministic accessibility/editor fake for credential-entry tests.

    Each item in ``observations`` is either probe text (focused), a
    ``(text, focused)`` pair, or ``_EDITOR_UNAVAILABLE``.  The fake stores
    input calls as structural records; it never prints or dumps the probe.
    """

    def __init__(
        self,
        clock: _FakeClock,
        observations: list[object],
        *,
        dump_advance: float = 0.0,
        input_advance: float = 0.0,
    ) -> None:
        self.clock = clock
        self.observations = observations
        self.dump_advance = dump_advance
        self.input_advance = input_advance
        self._observation_index = 0
        self.dump_calls = 0
        self.input_text_calls: list[tuple[str, str]] = []
        self.input_keyevent_calls: list[tuple[int, str]] = []
        self.input_tap_calls: list[tuple[int, int, str]] = []
        self.foreground_checks: list[str] = []

    def assert_foreground(self, stage: str) -> None:
        self.foreground_checks.append(stage)

    def dump_ui(self) -> list[smoke.Node]:
        self.dump_calls += 1
        self.clock.advance(self.dump_advance)
        if self.observations:
            observation = self.observations[
                min(self._observation_index, len(self.observations) - 1)
            ]
            self._observation_index += 1
        else:
            observation = _EDITOR_UNAVAILABLE
        if observation is _EDITOR_UNAVAILABLE:
            return []
        if isinstance(observation, tuple):
            text, focused = observation
        else:
            text, focused = observation, True
        description = (
            "Private OpenSSH key, Empty"
            if not text
            else "Private OpenSSH key, Private key entered"
        )
        return [
            smoke.Node(
                text,
                description,
                "android.widget.EditText",
                (0, 0, 100, 100),
                focused=focused,
            )
        ]

    def input_text(self, value: str, stage: str) -> None:
        self.input_text_calls.append((value, stage))
        self.clock.advance(self.input_advance)

    def input_keyevent(self, keycode: int, stage: str) -> None:
        self.input_keyevent_calls.append((keycode, stage))
        self.clock.advance(self.input_advance)

    def input_tap(self, x: int, y: int, stage: str) -> None:
        self.input_tap_calls.append((x, y, stage))


class _MutableProbeEditorDevice:
    """Editor fake that mutates its value when the driver sends input."""

    def __init__(
        self,
        clock: _FakeClock,
        *,
        initial_text: str = "",
        focused: bool = False,
    ) -> None:
        self.clock = clock
        self.text = initial_text
        self.focused = focused
        self.dump_calls = 0
        self.input_text_calls: list[tuple[str, str]] = []
        self.input_keyevent_calls: list[tuple[int, str]] = []
        self.input_tap_calls: list[tuple[int, int, str]] = []

    def dump_ui(self) -> list[smoke.Node]:
        self.dump_calls += 1
        description = (
            "Private OpenSSH key, Empty"
            if not self.text
            else "Private OpenSSH key, Private key entered"
        )
        return [
            smoke.Node(
                self.text,
                description,
                "android.widget.EditText",
                (0, 0, 100, 100),
                focused=self.focused,
            )
        ]

    def input_tap(self, x: int, y: int, stage: str) -> None:
        self.input_tap_calls.append((x, y, stage))
        self.focused = True

    def input_text(self, value: str, stage: str) -> None:
        self.input_text_calls.append((value, stage))
        self.text += value

    def input_keyevent(self, keycode: int, stage: str) -> None:
        self.input_keyevent_calls.append((keycode, stage))
        if keycode == smoke.KEYCODE_ENTER:
            self.text += "\n"


class _FieldProbeDevice:
    """A controlled TextInput whose accessibility value settles in stages."""

    def __init__(self, label: str, observations: list[str]) -> None:
        self.label = label
        self.observations = observations
        self.index = 0
        self.input_tap_calls: list[tuple[int, int, str]] = []
        self.input_text_calls: list[tuple[str, str]] = []

    def dump_ui(self) -> list[smoke.Node]:
        value = self.observations[min(self.index, len(self.observations) - 1)]
        self.index += 1
        description = self.label if not value else f"{self.label}, {value}"
        return [
            smoke.Node(
                value,
                description,
                "android.widget.EditText",
                (0, 0, 100, 100),
            )
        ]

    def input_tap(self, x: int, y: int, stage: str) -> None:
        self.input_tap_calls.append((x, y, stage))

    def input_text(self, value: str, stage: str) -> None:
        self.input_text_calls.append((value, stage))


@contextlib.contextmanager
def _patched_clock(clock: _FakeClock):
    with mock.patch.object(smoke.time, "monotonic", side_effect=clock.monotonic), mock.patch.object(
        smoke.time, "sleep", side_effect=clock.sleep
    ):
        yield


class UiDriverTests(unittest.TestCase):
    def test_quick_keys_reject_crowded_phone_columns(self):
        labels = ("Esc", "Tab", "Ctrl-C", "Paste", "Copy selection")
        good = [smoke.Node(label, label, "android.widget.TextView", (0, 0, 132, 132)) for label in labels]
        smoke.validate_quick_key_targets(good)
        crowded = [smoke.Node(label, label, "android.widget.TextView", (0, 0, 99, 132)) for label in labels]
        with self.assertRaises(smoke.SmokeFailure) as failure:
            smoke.validate_quick_key_targets(crowded)
        self.assertEqual(failure.exception.reason, "key_target_too_narrow")

    def test_native_theme_action_uses_uppercase_button_not_setting_value(self):
        value = smoke.Node("Light", "", "android.widget.TextView", (10, 10, 100, 40))
        action = smoke.Node("LIGHT", "", "android.widget.Button", (10, 50, 100, 90), resource_id="android:id/button1")
        self.assertIs(smoke.find_node([value, action], text="LIGHT", class_fragment="Button"), action)
        self.assertIsNone(smoke.find_node([value], text="LIGHT", class_fragment="Button"))

    def test_focus_terminal_waits_for_native_ime_after_tap(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        node = smoke.Node(
            "",
            "Terminal %1",
            "dev.meeterm.terminal.MeetermTerminalView",
            (0, 0, 100, 100),
        )

        with _patched_clock(clock):
            smoke.focus_terminal(device, node, "terminal_focus")

        device.input_tap.assert_called_once_with(50, 50, "terminal_focus")
        self.assertEqual(
            clock.sleep_calls,
            [smoke.TERMINAL_FOCUS_SETTLE_SECONDS],
        )

    def test_text_input_label_accepts_android_value_suffix_without_printing_value(self) -> None:
        value = "127.0.0.1"
        node = smoke.Node(
            value,
            f"Host, {value}",
            "android.widget.EditText",
            (0, 0, 100, 100),
        )
        self.assertTrue(smoke.content_description_has_label(node.content_description, "Host"))
        self.assertIs(smoke.find_text_input([node], "Host"), node)
        self.assertFalse(smoke.content_description_has_label("Hostname, other", "Host"))

    def test_field_readback_waits_for_settled_value_after_input(self) -> None:
        clock = _FakeClock()
        device = _FieldProbeDevice("Host", ["", "", "127.0.0.1", "127.0.0.1"])
        with _patched_clock(clock):
            node = smoke.wait_for_field_value(
                device,
                "host_input",
                "Host",
                "127.0.0.1",
            )
        self.assertEqual(node.text, "127.0.0.1")
        self.assertEqual(clock.sleep_calls, [smoke.FIELD_SETTLE_SECONDS] * 3)

    def test_field_readback_preserves_foreground_loss_when_editor_disappears(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = []
        device.assert_foreground.side_effect = smoke.SmokeFailure(
            "host_input",
            "app_not_foreground",
        )

        with self.assertRaises(smoke.SmokeFailure) as error:
            smoke.wait_for_field_value(
                device,
                "host_input",
                "Host",
                "127.0.0.1",
            )

        self.assertEqual(error.exception.reason, "app_not_foreground")
        device.assert_foreground.assert_called_once_with("host_input")

    def test_field_timeout_rechecks_foreground_before_entry_mismatch(self) -> None:
        clock = _FakeClock()
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.dump_ui.return_value = [
            smoke.Node(
                "partial",
                "Host, partial",
                "android.widget.EditText",
                (0, 0, 100, 100),
            )
        ]
        device.assert_foreground.side_effect = smoke.SmokeFailure(
            "host_input",
            "app_not_foreground",
        )

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.wait_for_field_value(
                    device,
                    "host_input",
                    "Host",
                    "127.0.0.1",
                    timeout=1.0,
                )

        self.assertEqual(error.exception.reason, "app_not_foreground")
        device.assert_foreground.assert_called_once_with("host_input")

    def test_fill_field_retries_once_after_readback_mismatch_without_appending(self) -> None:
        first = smoke.Node(
            "",
            "Host",
            "android.widget.EditText",
            (0, 0, 100, 100),
        )
        partial = smoke.Node(
            "part",
            "Host, part",
            "android.widget.EditText",
            (0, 0, 100, 100),
        )
        device = mock.Mock()
        readback_error = smoke.SmokeFailure("host_input", "entry_mismatch")
        with mock.patch.object(
            smoke,
            "wait_for_text_input",
            side_effect=[first, partial],
        ) as find, mock.patch.object(
            smoke,
            "wait_for_field_value",
            side_effect=[readback_error, None],
        ) as readback, mock.patch.object(smoke.time, "sleep"):
            smoke.fill_field(device, "Host", "part-value", "host_input", scroll=False)

        self.assertEqual(find.call_count, 2)
        self.assertEqual(readback.call_count, 2)
        self.assertEqual(
            device.input_text.call_args_list,
            [
                mock.call("part-value", "host_input"),
                mock.call("part-value", "host_input"),
            ],
        )
        self.assertEqual(
            device.input_keyevent.call_args_list,
            [
                mock.call(smoke.KEYCODE_F10, "host_input"),
                mock.call(smoke.KEYCODE_F10, "host_input"),
            ],
        )
        device.input_keyevents.assert_called_once_with(
            (smoke.KEYCODE_MOVE_END, smoke.KEYCODE_DEL, smoke.KEYCODE_DEL, smoke.KEYCODE_DEL, smoke.KEYCODE_DEL),
            "host_input",
        )

    def test_set_toggle_taps_once_and_verifies_checked_state(self) -> None:
        switch = smoke.Node(
            "",
            "Save server profile",
            "android.widget.Switch",
            (0, 0, 100, 100),
            checked=False,
        )
        device = mock.Mock()
        device.dump_ui.return_value = [switch]
        device.input_tap.side_effect = lambda _x, _y, _stage: setattr(
            switch,
            "checked",
            True,
        )

        smoke.set_toggle(
            device,
            "Save server profile",
            True,
            "save_profile",
            scroll=False,
        )

        device.input_tap.assert_called_once_with(50, 50, "save_profile")
        self.assertTrue(switch.checked)

    def test_fill_field_returns_after_first_success_without_retry(self) -> None:
        node = smoke.Node(
            "",
            "Host",
            "android.widget.EditText",
            (0, 0, 100, 100),
        )
        device = mock.Mock()
        with mock.patch.object(smoke, "wait_for_text_input", return_value=node) as find, mock.patch.object(
            smoke,
            "wait_for_field_value",
            return_value=node,
        ) as readback:
            smoke.fill_field(device, "Host", "127.0.0.1", "host_input", scroll=False)

        find.assert_called_once()
        readback.assert_called_once()
        device.input_text.assert_called_once_with("127.0.0.1", "host_input")
        device.input_keyevent.assert_called_once_with(
            smoke.KEYCODE_F10,
            "host_input",
        )
        device.input_keyevents.assert_not_called()

    def test_fill_field_stops_after_bounded_mismatch_retries(self) -> None:
        node = smoke.Node(
            "",
            "Host",
            "android.widget.EditText",
            (0, 0, 100, 100),
        )
        device = mock.Mock()
        mismatch = smoke.SmokeFailure("host_input", "entry_mismatch")
        with mock.patch.object(smoke, "wait_for_text_input", return_value=node) as find, mock.patch.object(
            smoke,
            "wait_for_field_value",
            side_effect=[mismatch, mismatch],
        ) as readback, mock.patch.object(smoke.time, "sleep"):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.fill_field(device, "Host", "127.0.0.1", "host_input", scroll=False)

        self.assertEqual(error.exception.reason, "entry_mismatch")
        self.assertEqual(find.call_count, smoke.FIELD_INPUT_MAX_ATTEMPTS)
        self.assertEqual(readback.call_count, smoke.FIELD_INPUT_MAX_ATTEMPTS)
        self.assertEqual(device.input_text.call_count, smoke.FIELD_INPUT_MAX_ATTEMPTS)
        self.assertEqual(
            device.input_keyevent.call_count,
            smoke.FIELD_INPUT_MAX_ATTEMPTS,
        )

    def test_fill_field_does_not_retry_non_mismatch_failure(self) -> None:
        node = smoke.Node(
            "",
            "Host",
            "android.widget.EditText",
            (0, 0, 100, 100),
        )
        device = mock.Mock()
        with mock.patch.object(smoke, "wait_for_text_input", return_value=node) as find, mock.patch.object(
            smoke,
            "wait_for_field_value",
            side_effect=smoke.SmokeFailure("host_input", "field_unavailable"),
        ) as readback:
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.fill_field(device, "Host", "127.0.0.1", "host_input", scroll=False)

        self.assertEqual(error.exception.reason, "field_unavailable")
        find.assert_called_once()
        readback.assert_called_once()
        device.input_text.assert_called_once_with("127.0.0.1", "host_input")
        device.input_keyevent.assert_called_once_with(
            smoke.KEYCODE_F10,
            "host_input",
        )
        device.input_keyevents.assert_not_called()

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

    def test_logcat_aggregates_fixed_native_input_rejection_reasons(self) -> None:
        device = smoke.AndroidDevice("test", "adb")
        device.note_terminal_input(4)
        logcat = b"\n".join(
            (
                b"01-01 00:00:01.000 I/MeetermInput: IME commit accepted; nativeCount=1 byteCount=2",
                b"01-01 00:00:01.001 I/MeetermInput: IME commit rejected; reason=unbound",
                b"01-01 00:00:01.002 I/MeetermInput: IME commit rejected; reason=native_exception",
                b"01-01 00:00:01.003 I/MeetermInput: IME commit rejected; reason=native_rejection",
                b"01-01 00:00:01.004 I/MeetermInput: IME commit rejected; reason=stale_or_native_rejection",
                b"01-01 00:00:01.005 I/MeetermInput: terminal special accepted",
                b"01-01 00:00:01.006 I/MeetermInput: terminal special rejected; reason=stale_or_native_rejection",
                b"01-01 00:00:01.007 I/MeetermInput: IME commit rejected; reason=unexpected",
            )
        )

        with mock.patch.object(device, "run", return_value=logcat):
            output = device.logcat()

        self.assertIn("acceptedCommits=1", output)
        self.assertIn("acceptedBytes=2", output)
        self.assertIn("rejectedCommits=4", output)
        self.assertIn("rejectedUnbound=1", output)
        self.assertIn("rejectedNativeException=1", output)
        self.assertIn("rejectedNativeRejection=1", output)
        self.assertIn("rejectedStaleOrNative=1", output)
        self.assertIn("acceptedSpecials=1", output)
        self.assertIn("rejectedSpecials=1", output)
        self.assertIn("rejectedSpecialStaleOrNative=1", output)
        self.assertNotIn("IME commit accepted", output)
        self.assertNotIn("IME commit rejected", output)
        self.assertNotIn("unexpected", output)

    def test_key_input_checks_focus_before_planning_probe_prefixes(self) -> None:
        device = mock.Mock()
        unfocused = smoke.Node("", "Private OpenSSH key, Empty", "android.widget.EditText", (0, 0, 100, 100))
        focused = smoke.Node("", "Private OpenSSH key, Empty", "android.widget.EditText", (0, 0, 100, 100), focused=True)
        device.dump_ui.side_effect = [[unfocused]] * 4 + [[focused]]
        with mock.patch.object(smoke.time, "sleep"), mock.patch.object(
            smoke.time, "monotonic", return_value=0.0
        ), mock.patch.object(smoke, "enter_key_prefix") as enter:
            smoke.fill_multiline_key(device, "public-probe")
        self.assertEqual(device.input_tap.call_count, 3)
        self.assertEqual(
            [call.args[1] for call in enter.call_args_list],
            ["", "public-probe"],
        )
        self.assertEqual(
            enter.call_args.kwargs["deadline"], smoke.KEY_INPUT_TIMEOUT
        )
        device.input_text.assert_not_called()
        device.input_keyevent.assert_not_called()

    def test_multiline_key_plans_empty_prefix_chunks_and_newline(self) -> None:
        focused = smoke.Node(
            "",
            "Private OpenSSH key, Empty",
            "android.widget.EditText",
            (0, 0, 100, 100),
            focused=True,
        )
        device = mock.Mock()
        device.dump_ui.return_value = [focused]
        key = "probe-0123456789abcdef\nprobe-tail"
        first_line, second_line = key.splitlines()

        with mock.patch.object(smoke.time, "sleep"), mock.patch.object(
            smoke.time, "monotonic", return_value=0.0
        ), mock.patch.object(smoke, "enter_key_prefix") as enter:
            smoke.fill_multiline_key(device, key)

        self.assertEqual(
            [call.args[1] for call in enter.call_args_list],
            [
                "",
                first_line[:16],
                first_line,
                first_line + "\n",
                key,
            ],
        )
        self.assertEqual(len(enter.call_args_list), 5)
        self.assertEqual(
            {call.kwargs["deadline"] for call in enter.call_args_list},
            {smoke.KEY_INPUT_TIMEOUT},
        )

    def test_multiline_key_uses_mutating_editor_for_each_settled_prefix(self) -> None:
        clock = _FakeClock()
        key = "probe-0123456789abcdef\nprobe-tail"
        device = _MutableProbeEditorDevice(clock)

        with _patched_clock(clock):
            smoke.fill_multiline_key(device, key)

        first_line, second_line = key.splitlines()
        self.assertEqual(device.text, key)
        self.assertEqual(
            device.input_text_calls,
            [
                (first_line[:16], "private_key_input"),
                (first_line[16:], "private_key_input"),
                (second_line, "private_key_input"),
            ],
        )
        self.assertEqual(
            device.input_keyevent_calls,
            [
                (smoke.KEYCODE_F10, "private_key_input"),
                (smoke.KEYCODE_F10, "private_key_input"),
                (smoke.KEYCODE_ENTER, "private_key_input"),
                (smoke.KEYCODE_F10, "private_key_input"),
            ],
        )
        self.assertEqual(len(device.input_tap_calls), 1)

    def test_key_readback_waits_for_the_focused_editor_update(self) -> None:
        clock = _FakeClock()
        expected = "public-probe\nline"
        device = _ProbeEditorDevice(clock, [expected, expected])

        with _patched_clock(clock):
            self.assertEqual(smoke.verify_key_readback(device, expected), expected)

        self.assertEqual(device.dump_calls, 2)
        self.assertEqual(clock.sleep_calls, [smoke.KEY_INPUT_SETTLE_SECONDS])

    def test_key_readback_returns_a_settled_partial_prefix(self) -> None:
        clock = _FakeClock()
        expected = "probe-alpha\nprobe-beta"
        prefix = "probe-alpha"
        device = _ProbeEditorDevice(clock, [prefix, prefix])

        with _patched_clock(clock):
            self.assertEqual(smoke.verify_key_readback(device, expected), prefix)

        self.assertEqual(device.dump_calls, 2)
        self.assertEqual(clock.sleep_calls, [smoke.KEY_INPUT_SETTLE_SECONDS])

    def test_enter_key_prefix_sends_only_missing_suffix_after_delayed_readback(self) -> None:
        clock = _FakeClock()
        expected = "probe-0123456789abcdef"
        prefix = "probe-"
        # The first post-input dump is stale.  The driver must let the
        # accessibility value catch up instead of sending the suffix again.
        device = _ProbeEditorDevice(
            clock,
            [prefix, prefix, prefix, expected, expected],
        )

        with _patched_clock(clock), contextlib.redirect_stdout(io.StringIO()):
            smoke.enter_key_prefix(device, expected, deadline=10.0)

        self.assertEqual(
            device.input_text_calls,
            [(expected[len(prefix):], "private_key_input")],
        )
        self.assertEqual(
            device.input_keyevent_calls,
            [(smoke.KEYCODE_F10, "private_key_input")],
        )

    def test_enter_key_prefix_recovers_from_repeated_partial_prefixes(self) -> None:
        clock = _FakeClock()
        expected = "probe-abcdefghijklmnopqr"
        first_prefix = expected[:8]
        device = _ProbeEditorDevice(
            clock,
            ["", "", first_prefix, first_prefix, expected, expected],
        )

        with _patched_clock(clock), contextlib.redirect_stdout(io.StringIO()):
            smoke.enter_key_prefix(device, expected, deadline=10.0)

        self.assertEqual(
            device.input_text_calls,
            [
                (expected[:16], "private_key_input"),
                (expected[8:], "private_key_input"),
            ],
        )
        self.assertEqual(
            device.input_keyevent_calls,
            [
                (smoke.KEYCODE_F10, "private_key_input"),
                (smoke.KEYCODE_F10, "private_key_input"),
            ],
        )

    def test_enter_key_prefix_retries_a_dropped_newline_without_replaying_text(self) -> None:
        clock = _FakeClock()
        expected = "probe-line\n"
        before_newline = "probe-line"
        device = _ProbeEditorDevice(
            clock,
            [
                before_newline,
                before_newline,
                before_newline,
                before_newline,
                expected,
                expected,
            ],
        )

        with _patched_clock(clock), contextlib.redirect_stdout(io.StringIO()):
            smoke.enter_key_prefix(device, expected, deadline=10.0)

        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(
            device.input_keyevent_calls,
            [
                (smoke.KEYCODE_ENTER, "private_key_input"),
                (smoke.KEYCODE_ENTER, "private_key_input"),
            ],
        )

    def test_enter_key_prefix_has_a_bounded_retry_limit_when_text_does_not_progress(self) -> None:
        clock = _FakeClock()
        expected = "probe-0123456789abcdef"
        device = _ProbeEditorDevice(clock, ["", ""] * 5)

        with _patched_clock(clock), contextlib.redirect_stdout(io.StringIO()) as output:
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, expected, deadline=20.0)

        self.assertEqual(error.exception.reason, "entry_retry_limit")
        self.assertEqual(
            device.input_text_calls,
            [(expected[:16], "private_key_input")] * smoke.KEY_INPUT_MAX_ATTEMPTS,
        )
        self.assertEqual(
            device.input_keyevent_calls,
            [(smoke.KEYCODE_F10, "private_key_input")]
            * smoke.KEY_INPUT_MAX_ATTEMPTS,
        )
        self.assertNotIn(expected, output.getvalue())

    def test_verify_key_readback_reports_unsettled_when_a_dump_crosses_deadline(self) -> None:
        clock = _FakeClock()
        device = _ProbeEditorDevice(clock, [""], dump_advance=2.0)

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.verify_key_readback(device, "probe-value", deadline=1.0)

        self.assertEqual(error.exception.reason, "entry_not_settled")
        self.assertEqual(device.dump_calls, 1)
        self.assertEqual(clock.sleep_calls, [])
        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(device.input_keyevent_calls, [])

    def test_verify_key_readback_default_timeout_is_finite(self) -> None:
        clock = _FakeClock()
        device = _ProbeEditorDevice(
            clock,
            [""],
            dump_advance=smoke.KEY_READBACK_TIMEOUT + 1.0,
        )

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.verify_key_readback(device, "probe-value")

        self.assertEqual(error.exception.reason, "entry_not_settled")
        self.assertEqual(device.dump_calls, 1)
        self.assertEqual(clock.sleep_calls, [])

    def test_enter_key_prefix_reports_timeout_after_an_input_crosses_deadline(self) -> None:
        clock = _FakeClock()
        expected = "probe-0123456789abcdef"
        device = _ProbeEditorDevice(
            clock,
            ["", ""],
            input_advance=2.0,
        )

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, expected, deadline=1.0)

        self.assertEqual(error.exception.reason, "entry_timeout")
        self.assertEqual(
            device.input_text_calls,
            [(expected[:16], "private_key_input")],
        )
        self.assertEqual(device.input_keyevent_calls, [])

    def test_enter_key_prefix_does_not_dump_or_send_after_deadline(self) -> None:
        clock = _FakeClock()
        clock.now = 1.0
        device = _ProbeEditorDevice(clock, ["probe-value"])

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, "probe-value-suffix", deadline=1.0)

        self.assertEqual(error.exception.reason, "entry_timeout")
        self.assertEqual(device.dump_calls, 0)
        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(device.input_keyevent_calls, [])

    def test_matching_text_without_keyboard_focus_is_rejected(self) -> None:
        clock = _FakeClock()
        device = _ProbeEditorDevice(clock, [("probe-value", False)])

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, "probe-value", deadline=5.0)

        self.assertEqual(error.exception.reason, "editor_lost_focus")
        self.assertEqual(device.dump_calls, 1)
        self.assertEqual(clock.sleep_calls, [])
        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(device.input_keyevent_calls, [])

    def test_transient_missing_editor_is_retried_without_input(self) -> None:
        clock = _FakeClock()
        device = _ProbeEditorDevice(
            clock,
            [_EDITOR_UNAVAILABLE, "probe-value", "probe-value"],
        )

        with _patched_clock(clock):
            observed = smoke.verify_key_readback(device, "probe-value", deadline=5.0)

        self.assertEqual(observed, "probe-value")
        self.assertEqual(device.dump_calls, 3)
        self.assertEqual(device.foreground_checks, ["private_key_input"])
        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(device.input_keyevent_calls, [])

    def test_reappearing_editor_must_settle_again_after_missing_pass(self) -> None:
        clock = _FakeClock()
        prefix = "probe-"
        device = _ProbeEditorDevice(
            clock,
            [prefix, _EDITOR_UNAVAILABLE, prefix, prefix],
        )

        with _patched_clock(clock):
            observed = smoke.verify_key_readback(
                device,
                "probe-value",
                deadline=5.0,
            )

        self.assertEqual(observed, prefix)
        self.assertGreaterEqual(device.dump_calls, 4)
        self.assertEqual(device.foreground_checks, ["private_key_input"])
        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(device.input_keyevent_calls, [])

    def test_persistently_missing_editor_is_rejected_without_input(self) -> None:
        clock = _FakeClock()
        device = _ProbeEditorDevice(clock, [_EDITOR_UNAVAILABLE])

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, "probe-value", deadline=5.0)

        self.assertEqual(error.exception.reason, "editor_unavailable")
        self.assertGreater(device.dump_calls, 1)
        self.assertGreaterEqual(clock.now, 5.0)
        self.assertEqual(
            device.foreground_checks,
            ["private_key_input"] * device.dump_calls,
        )
        self.assertEqual(device.input_text_calls, [])
        self.assertEqual(device.input_keyevent_calls, [])

    def test_nonprefix_corruption_fails_immediately_for_same_length_longer_and_newline(self) -> None:
        expected = "probe-line\nnext"
        corruptions = {
            "same_length": "probe-lxne\nnext",
            "longer": expected + "x",
            "wrong_newline": expected.replace("\n", " "),
        }

        for kind, observed in corruptions.items():
            with self.subTest(kind=kind):
                clock = _FakeClock()
                device = _ProbeEditorDevice(clock, [observed])
                with _patched_clock(clock):
                    with self.assertRaises(smoke.SmokeFailure) as error:
                        smoke.verify_key_readback(device, expected)

                self.assertEqual(error.exception.reason, "entry_content_mismatch")
                self.assertEqual(device.dump_calls, 1)
                self.assertEqual(clock.sleep_calls, [])
                self.assertEqual(device.input_text_calls, [])
                self.assertEqual(device.input_keyevent_calls, [])

    def test_failed_recovery_diagnostics_never_print_probe_text(self) -> None:
        probe = "probe-sensitive-value"
        expected = probe + "-suffix"
        device = _ProbeEditorDevice(_FakeClock(), [probe, probe] * 5)
        clock = device.clock

        with _patched_clock(clock), contextlib.redirect_stdout(io.StringIO()) as output:
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, expected, deadline=20.0)

        self.assertEqual(error.exception.reason, "entry_retry_limit")
        self.assertNotIn(probe, output.getvalue())
        self.assertNotIn(expected, str(error.exception))

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
                self.assertEqual(len(panes), smoke.FIXTURE_PANE_COUNT)
                self.assertEqual(
                    len({record.window_id for record in panes}),
                    len(smoke.FIXTURE_WINDOW_NAMES),
                )
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
            self.swipe_options: list[tuple[int | None, bool]] = []

        def dump_ui(self) -> list[smoke.Node]:
            return self.pages[min(self.page, len(self.pages) - 1)]

        def input_swipe(
            self,
            bounds: tuple[int, int, int, int],
            _stage: str,
            *,
            x: int | None = None,
            toward_start: bool = False,
        ) -> None:
            self.swipes.append(bounds)
            self.swipe_options.append((x, toward_start))
            self.page += 1

    def test_parse_ui_dump_preserves_scroll_and_accessibility_metadata(self) -> None:
        output = b"""uiautomator dump
<?xml version='1.0' encoding='UTF-8' ?><hierarchy>
<node class='android.widget.ScrollView' content-desc='' bounds='[0,100][1080,1900]'
 scrollable='true' enabled='true' visible-to-user='true'>
<node class='android.widget.EditText' resource-id='ssh-private-key'
   content-desc='Private OpenSSH key, Empty'
   bounds='[20,1200][1060,1700]' scrollable='false' enabled='true'
   visible-to-user='true' selected='true' checked='true'/>
</node></hierarchy>
UI dumped to: /dev/tty"""

        nodes = smoke.parse_ui_dump(output)

        self.assertEqual(len(nodes), 2)
        self.assertTrue(nodes[0].scrollable)
        self.assertEqual(nodes[1].resource_id, "ssh-private-key")
        self.assertEqual(nodes[1].content_description, "Private OpenSSH key, Empty")
        self.assertTrue(nodes[1].selected)
        self.assertTrue(nodes[1].checked)
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
        self.assertEqual(device.swipe_options, [(None, False)])

    def test_form_gutter_uses_observed_padding_beside_editor(self) -> None:
        scroll_bounds = (0, 100, 1080, 1900)
        nodes = [
            smoke.Node(
                "",
                "",
                "android.widget.ScrollView",
                scroll_bounds,
                scrollable=True,
            ),
            smoke.Node(
                "",
                "Private OpenSSH key, Empty",
                "android.widget.EditText",
                (72, 900, 1008, 1250),
            ),
        ]

        self.assertEqual(smoke.form_scroll_gutter_x(nodes, scroll_bounds), 36)

    def test_private_key_wait_returns_from_form_end_through_gutter(self) -> None:
        scroll = smoke.Node(
            "",
            "",
            "android.widget.ScrollView",
            (0, 100, 1080, 1900),
            scrollable=True,
        )
        name = smoke.Node(
            "",
            "Server name",
            "android.widget.EditText",
            (72, 400, 1008, 560),
        )
        editor = smoke.Node(
            "",
            "Private OpenSSH key, Empty",
            "android.widget.EditText",
            (72, 400, 1008, 750),
        )
        device = self._ScrollingDevice([[scroll, name], [scroll, editor]])

        found = smoke.wait_for_private_key_editor(
            device,
            "private_key_input",
            scroll_gutter=True,
            scroll_toward_start=True,
            timeout=2.0,
        )

        self.assertIs(found, editor)
        self.assertEqual(device.swipe_options, [(36, True)])

    def test_terminal_tab_test_ids_are_stable_runtime_identities_without_spoken_ids(self) -> None:
        nodes = [
            smoke.Node(
                "Workspace 0",
                "",
                "android.widget.TextView",
                (10, 20, 300, 90),
                resource_id="workspace-row-@0",
            ),
            smoke.Node(
                "",
                "Terminal Build, Agent status: working",
                "android.view.View",
                (10, 100, 300, 190),
                resource_id="dev.meeterm.app:id/terminal-tab-%17",
            ),
            smoke.Node(
                "",
                "Terminal Shell",
                "android.view.View",
                (310, 100, 600, 190),
                resource_id="dev.meeterm.app:id/terminal-tab-%23",
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

    def test_workspace_lookup_prefers_tmux_selected_window(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Workspace handoff",
                "android.view.View",
                (0, 0, 400, 100),
                resource_id="workspace-row-@1",
            ),
            smoke.Node(
                "",
                "Workspace smoke",
                "android.view.View",
                (0, 100, 400, 200),
                resource_id="workspace-row-@0",
                selected=True,
            ),
        ]

        workspace = smoke.find_workspace_node(nodes)
        self.assertIsNotNone(workspace)
        assert workspace is not None
        self.assertEqual(smoke.accessible_label(workspace), "Workspace smoke")

    def test_wait_for_workspace_label_finds_nonselected_requested_window(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Workspace handoff",
                "android.view.View",
                (0, 0, 400, 100),
                resource_id="workspace-row-@1",
                selected=True,
            ),
            smoke.Node(
                "",
                "Workspace smoke",
                "android.view.View",
                (0, 100, 400, 200),
                resource_id="workspace-row-@0",
            ),
        ]
        device = mock.Mock()
        device.dump_ui.return_value = nodes

        workspace = smoke.wait_for_workspace(
            device,
            "workspace_return",
            label="Workspace smoke",
            timeout=1.0,
        )

        self.assertIs(workspace, nodes[1])

    def test_workspace_rows_ignore_option_buttons_without_name_reservations(self) -> None:
        output = b"""<?xml version='1.0' encoding='UTF-8' ?><hierarchy>
<node class='android.view.View' resource-id='dev.meeterm.app:id/workspace-row-@1'
 content-desc='Workspace smoke' bounds='[0,0][800,100]' enabled='true' visible-to-user='true'/>
<node class='android.view.View' resource-id='' content-desc='Workspace options smoke'
 bounds='[800,0][1000,100]' enabled='true' visible-to-user='true'/>
<node class='android.view.View' resource-id='workspace-row-@2'
 content-desc='Workspace handoff' bounds='[0,100][800,200]' enabled='true' visible-to-user='true'/>
<node class='android.view.View' resource-id='' content-desc='Workspace options handoff'
 bounds='[800,100][1000,200]' enabled='true' visible-to-user='true'/>
<node class='android.view.View' resource-id='workspace-row-@3'
 content-desc='Workspace options foo' bounds='[0,200][800,300]' enabled='true' visible-to-user='true'/>
<node class='android.view.View' resource-id='' content-desc='Workspace options options foo'
 bounds='[800,200][1000,300]' enabled='true' visible-to-user='true'/>
</hierarchy>"""

        rows = smoke.find_workspace_nodes(smoke.parse_ui_dump(output))

        self.assertEqual(len(rows), 3)
        self.assertEqual(
            {smoke.workspace_id_from_node(node) for node in rows},
            {"@1", "@2", "@3"},
        )
        self.assertEqual(
            {smoke.accessible_label(node) for node in rows},
            {"Workspace smoke", "Workspace handoff", "Workspace options foo"},
        )

    def test_failure_ui_state_contains_only_allowlisted_categories(self) -> None:
        nodes = [
            smoke.Node(
                "Choose a runtime",
                "",
                "android.widget.TextView",
                (0, 0, 400, 100),
            ),
            smoke.Node(
                "",
                "tmux runtime meeterm",
                "android.view.View",
                (0, 100, 400, 200),
            ),
            smoke.Node(
                "",
                "Workspace private-name",
                "android.view.View",
                (0, 200, 400, 300),
                resource_id="workspace-row-@9",
            ),
            smoke.Node(
                "Saved servers",
                "",
                "android.widget.TextView",
                (0, 300, 400, 400),
            ),
            smoke.Node(
                smoke.PROFILE_CONNECT_ERROR,
                "",
                "android.widget.TextView",
                (0, 400, 400, 500),
            ),
        ]

        self.assertEqual(
            smoke.sanitized_failure_ui_state(nodes),
            "connection_state=awaiting_runtime_selection\n"
            "runtime_picker=yes\n"
            "workspace_row=yes\n"
            "saved_servers_sheet=yes\n"
            "switch_confirmation=no\n"
            "profile_connect_error=yes\n",
        )
        self.assertNotIn("private-name", smoke.sanitized_failure_ui_state(nodes))

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

    def test_private_key_editor_prefers_edit_text_and_allows_invisible_focus_pass(self) -> None:
        wrapper = smoke.Node(
            "",
            "Private OpenSSH key, Empty",
            "android.view.View",
            (10, 100, 1000, 600),
        )
        editor = smoke.Node(
            "",
            "Private OpenSSH key, Empty",
            "android.widget.EditText",
            (10, 100, 1000, 600),
            visible_to_user=False,
            focused=True,
        )

        self.assertIs(smoke.find_private_key_editor([wrapper, editor]), wrapper)
        self.assertIs(
            smoke.find_private_key_editor([wrapper, editor], include_invisible=True),
            editor,
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

    def test_labeled_terminal_surface_beats_larger_generic_parent(self) -> None:
        nodes = [
            smoke.Node("", "", "android.view.View", (0, 50, 1080, 2400)),
            smoke.Node(
                "",
                "Terminal",
                "android.opengl.GLSurfaceView",
                (0, 430, 1080, 2200),
            ),
        ]

        self.assertIs(smoke.find_labeled_terminal_surface(nodes), nodes[1])
        self.assertIs(smoke.find_terminal_node(nodes), nodes[1])

    def test_labeled_terminal_surface_prefers_renderer_over_larger_wrapper(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Terminal",
                "android.widget.LinearLayout",
                (0, 430, 1080, 2400),
            ),
            smoke.Node(
                "",
                "Terminal",
                "android.opengl.GLSurfaceView",
                (0, 430, 1080, 1320),
            ),
        ]

        self.assertIs(smoke.find_labeled_terminal_surface(nodes), nodes[1])
        self.assertIs(smoke.find_terminal_node(nodes), nodes[1])

    def test_labeled_terminal_surface_falls_back_without_renderer(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Terminal",
                "android.widget.LinearLayout",
                (0, 430, 1080, 2200),
            ),
            smoke.Node(
                "",
                "Terminal",
                "android.view.View",
                (0, 430, 800, 1800),
            ),
        ]

        self.assertIs(smoke.find_labeled_terminal_surface(nodes), nodes[0])

    def test_labeled_terminal_surface_rejects_disabled_renderer_for_fallback(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Terminal",
                "android.widget.LinearLayout",
                (0, 430, 1080, 2200),
            ),
            smoke.Node(
                "",
                "Terminal",
                "android.opengl.GLSurfaceView",
                (0, 430, 1080, 1320),
                enabled=False,
            ),
        ]

        self.assertIs(smoke.find_labeled_terminal_surface(nodes), nodes[0])

    def test_cached_terminal_surface_keeps_its_distinct_label_contract(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Terminal, cached output, read only",
                "android.widget.LinearLayout",
                (0, 430, 1080, 2200),
                enabled=False,
            ),
            smoke.Node(
                "",
                "Terminal, cached output, read only",
                "android.opengl.GLSurfaceView",
                (0, 430, 1080, 1320),
                enabled=False,
            ),
        ]

        self.assertIs(smoke.find_cached_terminal_surface(nodes), nodes[1])

    def test_cached_terminal_surface_falls_back_without_renderer(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Terminal, cached output, read only",
                "android.widget.LinearLayout",
                (0, 430, 1080, 2200),
                enabled=False,
            ),
            smoke.Node(
                "",
                "Terminal, cached output, read only",
                "android.view.View",
                (0, 430, 800, 1800),
                enabled=False,
            ),
        ]

        self.assertIs(smoke.find_cached_terminal_surface(nodes), nodes[0])

    def test_labeled_terminal_surface_rejects_unlabeled_fallbacks(self) -> None:
        nodes = [
            smoke.Node("", "", "android.view.View", (0, 50, 1080, 2400)),
            smoke.Node("", "", "android.opengl.GLSurfaceView", (0, 430, 1080, 2200)),
            smoke.Node("", "Terminal", "android.view.View", (0, 430, 0, 2200)),
        ]

        self.assertIsNone(smoke.find_labeled_terminal_surface(nodes))

    def test_selection_drag_maps_exact_first_row_character_range(self) -> None:
        terminal = smoke.Node(
            "",
            "",
            "dev.meeterm.terminal.MeetermTerminalView",
            (10, 100, 1010, 1900),
        )

        start, end = smoke.selection_drag_points(
            terminal,
            columns=50,
            character_count=8,
        )

        self.assertEqual(start, (20, 120))
        self.assertEqual(end, (160, 120))
        with self.assertRaises(smoke.SmokeFailure) as error:
            smoke.selection_drag_points(
                terminal,
                columns=7,
                character_count=8,
            )
        self.assertEqual(error.exception.reason, "invalid_selection_geometry")

    def test_selection_geometry_diagnostic_has_only_fixed_structure(self) -> None:
        terminal = smoke.Node(
            "",
            "Terminal",
            "android.opengl.GLSurfaceView",
            (10, 100, 1010, 1900),
        )

        diagnostic = smoke.selection_geometry_diagnostic(
            terminal,
            columns=50,
            character_count=8,
            start=(20, 120),
            end=(160, 120),
        )

        self.assertEqual(
            diagnostic,
            "surface_label=Terminal\n"
            "surface_class=android.opengl.GLSurfaceView\n"
            "surface_candidate=renderer\n"
            "surface_class_kind=gl_surface\n"
            "bounds_left=10\n"
            "bounds_top=100\n"
            "bounds_right=1010\n"
            "bounds_bottom=1900\n"
            "columns=50\n"
            "character_count=8\n"
            "start_x=20\n"
            "start_y=120\n"
            "end_x=160\n"
            "end_y=120\n",
        )

    def test_native_selection_gesture_is_a_bounded_long_press_drag(self) -> None:
        device = smoke.AndroidDevice("emulator-5554", "adb")
        device.assert_foreground = mock.Mock()
        device.run = mock.Mock(return_value=b"")

        device.input_long_press_drag(20, 120, 160, 120, "selection")

        device.assert_foreground.assert_called_once_with("selection")
        device.run.assert_called_once_with(
            (
                "shell",
                "input",
                "draganddrop",
                "20",
                "120",
                "160",
                "120",
                "1200",
            ),
            "selection",
            timeout=10.0,
        )

    def test_native_selection_gesture_rejects_invalid_coordinates(self) -> None:
        device = smoke.AndroidDevice("emulator-5554", "adb")
        device.assert_foreground = mock.Mock()
        device.run = mock.Mock(return_value=b"")

        for coordinates in ((-1, 120, 160, 120), (20, 120, 20, 120)):
            with self.subTest(coordinates=coordinates):
                with self.assertRaises(smoke.SmokeFailure) as error:
                    device.input_long_press_drag(*coordinates, "selection")
                self.assertEqual(error.exception.reason, "invalid_long_press")

        device.assert_foreground.assert_not_called()
        device.run.assert_not_called()

    def test_screenrecord_owns_remote_pid_and_stops_only_that_process(self) -> None:
        device = mock.Mock(spec=smoke.AndroidDevice)
        device.adb_path = "adb"
        device.serial = "emulator-5554"
        device.run.side_effect = [b"", b"4312\n", b"", b""]

        with tempfile.TemporaryDirectory(prefix="meeterm-video-") as root:
            output = Path(root) / "daily-use.mp4"
            recording, reason = smoke.start_optional_screenrecord(device, output)

            self.assertEqual(reason, "ok")
            self.assertEqual(
                recording,
                smoke.ScreenRecording(4312, smoke.SCREENRECORD_REMOTE_PATH, output),
            )
            start_script = device.run.call_args_list[1].args[0]
            self.assertEqual(start_script[:3], ("shell", "sh", "-c"))
            self.assertIn("--time-limit 180", start_script[3])
            self.assertIn("echo $child", start_script[3])
            self.assertNotIn("pkill", start_script[3])

            assert recording is not None

            def finish_run(arguments, _stage, timeout):
                if arguments[:3] == ("shell", "kill", "-0"):
                    raise smoke.SmokeFailure("daily_screenrecord_wait", "adb_failed")
                if arguments[0] == "pull":
                    output.write_bytes(b"\x00\x00\x00\x18ftypisom")
                return b""

            device.run.reset_mock(side_effect=True)
            device.run.side_effect = finish_run
            self.assertEqual(
                smoke.finish_optional_screenrecord(device, recording),
                "ok",
            )
            stop_arguments = device.run.call_args_list[0].args[0]
            self.assertEqual(stop_arguments, ("shell", "kill", "-2", "4312"))

    def test_tmux_parser_keeps_pane_pid_and_real_selection_state(self) -> None:
        output = (
            b"@4\tsmoke\t%12\t1201\t0\t0\t40\t24\t0\t0\t39\t23\t1\t1\n"
            b"@4\tsmoke\t%13\t1202\t1\t1\t40\t24\t40\t0\t79\t23\t1\t1\n"
            b"@5\thandoff\t%14\t1203\t0\t1\t40\t24\t0\t0\t39\t23\t0\t0\n"
            b"@5\thandoff\t%15\t1204\t1\t0\t40\t24\t40\t0\t79\t23\t0\t0\n"
        )

        records = smoke.parse_tmux_panes(output)

        self.assertEqual(records[1].pane_id, "%13")
        self.assertEqual(records[1].pane_pid, 1202)
        self.assertTrue(
            smoke._selection_matches(records, "%13", 1202),
        )
        self.assertFalse(smoke._selection_matches(records, "%12", 1201))

    def test_tmux_layout_check_detects_split_changes_separately_from_identity(self) -> None:
        output = (
            b"@4\tsmoke\t%12\t1201\t0\t0\t40\t24\t0\t0\t39\t23\t1\t0\n"
            b"@4\tsmoke\t%13\t1202\t1\t1\t40\t24\t40\t0\t79\t23\t1\t0\n"
            b"@5\thandoff\t%14\t1203\t0\t1\t40\t24\t0\t0\t39\t23\t0\t0\n"
            b"@5\thandoff\t%15\t1204\t1\t0\t40\t24\t40\t0\t79\t23\t0\t0\n"
        )
        records = smoke.parse_tmux_panes(output)
        smoke.assert_fixture_layout_preserved(records, records, "layout")

        changed_identity = list(records)
        changed_identity[0] = changed_identity[0]._replace(pane_pid=9999)
        with self.assertRaises(smoke.SmokeFailure) as identity_error:
            smoke.assert_fixture_layout_preserved(records, changed_identity, "layout")
        self.assertEqual(identity_error.exception.reason, "pane_layout_changed")

        changed_split = list(records)
        changed_split[1] = changed_split[1]._replace(pane_top=1)
        with self.assertRaises(smoke.SmokeFailure) as split_error:
            smoke.assert_fixture_layout_preserved(records, changed_split, "layout")
        self.assertEqual(split_error.exception.reason, "pane_split_changed")

    def test_tmux_identity_check_allows_mobile_zoom_geometry(self) -> None:
        output = (
            b"@4\tsmoke\t%12\t1201\t0\t0\t40\t24\t0\t0\t39\t23\t1\t0\n"
            b"@4\tsmoke\t%13\t1202\t1\t1\t40\t24\t40\t0\t79\t23\t1\t0\n"
            b"@5\thandoff\t%14\t1203\t0\t1\t40\t24\t0\t0\t39\t23\t0\t0\n"
            b"@5\thandoff\t%15\t1204\t1\t0\t40\t24\t40\t0\t79\t23\t0\t0\n"
        )
        records = smoke.parse_tmux_panes(output)
        zoomed = list(records)
        zoomed[0] = zoomed[0]._replace(
            pane_width=80,
            pane_right=79,
            zoomed=True,
        )

        smoke.assert_fixture_identity_preserved(records, zoomed, "resume")
        with self.assertRaises(smoke.SmokeFailure) as full_error:
            smoke.assert_fixture_layout_preserved(records, zoomed, "resume")
        self.assertEqual(full_error.exception.reason, "pane_split_changed")

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
        appended = smoke.session_marker_command(marker, path, append=True)
        self.assertIn(":$$", with_pid)
        self.assertIn('[ "$$" = 1202 ]', resumed_with_pid)
        self.assertIn(" >> ", appended)

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
