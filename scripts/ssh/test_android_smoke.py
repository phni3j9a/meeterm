#!/usr/bin/env python3
"""Deterministic checks for the Android SSH smoke driver.

These tests exercise the parser, command-building boundary, and deterministic
key-entry state machine without requiring an emulator or an OpenSSH fixture.
The hosted job remains the authoritative check of the complete UI/native path.
"""

from __future__ import annotations

import contextlib
import io
from pathlib import Path
import os
import shutil
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
    def test_focus_terminal_waits_for_native_ime_after_tap(self) -> None:
        clock = _FakeClock()
        device = mock.Mock()
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
                b"01-01 00:00:01.004 I/MeetermInput: IME commit rejected; reason=unexpected",
            )
        )

        with mock.patch.object(device, "run", return_value=logcat):
            output = device.logcat()

        self.assertIn("acceptedCommits=1", output)
        self.assertIn("acceptedBytes=2", output)
        self.assertIn("rejectedCommits=3", output)
        self.assertIn("rejectedUnbound=1", output)
        self.assertIn("rejectedNativeException=1", output)
        self.assertIn("rejectedNativeRejection=1", output)
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
            [(smoke.KEYCODE_ENTER, "private_key_input")],
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
        self.assertEqual(device.input_keyevent_calls, [])

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
        self.assertEqual(device.input_keyevent_calls, [])

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
        self.assertEqual(device.input_keyevent_calls, [])
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

    def test_missing_editor_is_rejected_immediately_without_input(self) -> None:
        clock = _FakeClock()
        device = _ProbeEditorDevice(clock, [_EDITOR_UNAVAILABLE])

        with _patched_clock(clock):
            with self.assertRaises(smoke.SmokeFailure) as error:
                smoke.enter_key_prefix(device, "probe-value", deadline=5.0)

        self.assertEqual(error.exception.reason, "editor_unavailable")
        self.assertEqual(device.dump_calls, 1)
        self.assertEqual(clock.sleep_calls, [])
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

    def test_workspace_lookup_prefers_tmux_selected_window(self) -> None:
        nodes = [
            smoke.Node(
                "",
                "Workspace handoff",
                "android.view.View",
                (0, 0, 400, 100),
            ),
            smoke.Node(
                "",
                "Workspace smoke",
                "android.view.View",
                (0, 100, 400, 200),
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
                selected=True,
            ),
            smoke.Node(
                "",
                "Workspace smoke",
                "android.view.View",
                (0, 100, 400, 200),
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
