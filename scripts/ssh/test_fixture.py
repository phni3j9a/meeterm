#!/usr/bin/env python3
"""Unit checks for the disposable fixture's sshd-only transport control."""

from __future__ import annotations

from pathlib import Path
import os
import tempfile
import threading
import time
import unittest
from unittest import mock

import fixture


class FixtureControlTests(unittest.TestCase):
    def make_fixture(self) -> tuple[tempfile.TemporaryDirectory[str], fixture.Fixture]:
        directory = tempfile.TemporaryDirectory(prefix="meeterm-ssh-fixture-")
        root = Path(directory.name)
        root.chmod(0o700)
        return directory, fixture.Fixture(root)

    def control_environment(self, instance: fixture.Fixture) -> dict[str, str]:
        return {
            fixture.CONTROL_REQUEST_ENV: str(instance.control_request_path),
            fixture.CONTROL_STATUS_ENV: str(instance.control_status_path),
        }

    def test_stop_and_start_use_one_fixture_owned_control_channel(self) -> None:
        directory, instance = self.make_fixture()
        self.addCleanup(directory.cleanup)
        stopped = threading.Event()
        started = threading.Event()

        def stop() -> None:
            stopped.set()

        def start() -> None:
            started.set()

        with (
            mock.patch.object(instance, "stop_sshd", side_effect=stop),
            mock.patch.object(instance, "start_sshd", side_effect=start),
            mock.patch.dict(os.environ, self.control_environment(instance), clear=False),
        ):
            instance.start_control()
            self.addCleanup(instance.stop_control)
            fixture.send_control("stop", timeout=2)
            fixture.send_control("start", timeout=2)

        self.assertTrue(stopped.is_set())
        self.assertTrue(started.is_set())
        self.assertFalse(instance.control_request_path.exists())
        self.assertTrue(instance.control_status_path.exists())

    def test_stop_waits_for_accepted_session_children_before_acknowledging(self) -> None:
        directory, instance = self.make_fixture()
        self.addCleanup(directory.cleanup)
        process = mock.Mock(pid=4001)
        process.poll.return_value = None
        process.communicate.return_value = ("", "")
        instance.process = process

        with (
            mock.patch.object(instance, "_descendant_process_ids", return_value=[4002]),
            mock.patch.object(instance, "_process_group_member_ids", return_value=[4003]),
            mock.patch.object(
                instance,
                "_capture_process_identities",
                return_value={4002: "process-4002", 4003: "process-4003"},
            ) as capture,
            mock.patch.object(instance, "_signal_process_group") as signal_group,
            mock.patch.object(instance, "_signal_process_identities") as signal_ids,
            mock.patch.object(
                instance,
                "_wait_for_process_ids_exit",
                side_effect=[{4002: "process-4002"}, {}],
            ) as wait_for_exit,
        ):
            instance.stop_sshd()

        self.assertIsNone(instance.process)
        capture.assert_called_once_with([4002, 4003])
        signal_group.assert_called_once_with(process, fixture.signal.SIGTERM)
        signal_ids.assert_has_calls(
            [
                mock.call(
                    {4002: "process-4002", 4003: "process-4003"},
                    fixture.signal.SIGTERM,
                ),
                mock.call({4002: "process-4002"}, fixture.signal.SIGKILL),
            ]
        )
        self.assertEqual(wait_for_exit.call_count, 2)

    def test_stop_fails_closed_when_accepted_session_child_survives(self) -> None:
        directory, instance = self.make_fixture()
        self.addCleanup(directory.cleanup)
        process = mock.Mock(pid=4101)
        process.poll.return_value = None
        process.communicate.return_value = ("", "")
        instance.process = process

        with (
            mock.patch.object(instance, "_descendant_process_ids", return_value=[4102]),
            mock.patch.object(instance, "_process_group_member_ids", return_value=[]),
            mock.patch.object(
                instance,
                "_capture_process_identities",
                return_value={4102: "process-4102"},
            ),
            mock.patch.object(instance, "_signal_process_group"),
            mock.patch.object(instance, "_signal_process_identities"),
            mock.patch.object(
                instance,
                "_wait_for_process_ids_exit",
                side_effect=[
                    {4102: "process-4102"},
                    {4102: "process-4102"},
                ],
            ),
        ):
            with self.assertRaisesRegex(
                fixture.FixtureError,
                "OpenSSH fixture process tree did not stop",
            ):
                instance.stop_sshd()
        self.assertIs(instance.process, process)
        self.assertEqual(instance.sshd_descendants, {4102: "process-4102"})

    def test_process_identity_change_is_never_signaled(self) -> None:
        with (
            mock.patch.object(
                fixture.Fixture,
                "_process_identity",
                return_value="replacement-process",
            ),
            mock.patch.object(fixture.os, "kill") as kill,
        ):
            with self.assertRaisesRegex(
                fixture.FixtureError,
                "OpenSSH fixture process identity changed",
            ):
                fixture.Fixture._signal_process_identities(
                    {4202: "owned-process"},
                    fixture.signal.SIGKILL,
                )
        kill.assert_not_called()

    def test_partial_request_is_not_processed_until_newline_arrives(self) -> None:
        directory, instance = self.make_fixture()
        self.addCleanup(directory.cleanup)
        token = "transport-loss-partial"
        with mock.patch.object(instance, "stop_sshd") as stop, mock.patch.dict(
            os.environ, self.control_environment(instance), clear=False
        ):
            instance.start_control()
            self.addCleanup(instance.stop_control)
            instance.control_request_path.write_text(token + "\tstop", encoding="utf-8")
            instance.control_request_path.chmod(0o600)
            time.sleep(0.15)
            stop.assert_not_called()
            with instance.control_request_path.open("a", encoding="utf-8") as stream:
                stream.write("\n")
            deadline = time.monotonic() + 2
            while not stop.called and time.monotonic() < deadline:
                time.sleep(0.02)
            stop.assert_called_once_with()

    def test_control_error_is_returned_without_retrying_or_touching_tmux(self) -> None:
        directory, instance = self.make_fixture()
        self.addCleanup(directory.cleanup)
        with (
            mock.patch.object(
                instance,
                "start_sshd",
                side_effect=fixture.FixtureError("start failed"),
            ) as start,
            mock.patch.object(instance, "stop_sshd") as stop,
            mock.patch.dict(os.environ, self.control_environment(instance), clear=False),
        ):
            instance.start_control()
            self.addCleanup(instance.stop_control)
            with self.assertRaises(fixture.FixtureError) as error:
                fixture.send_control("start", timeout=2)
            self.assertEqual(str(error.exception), "fixture control action failed")
            start.assert_called_once_with()
            stop.assert_not_called()
            self.assertFalse(instance.control_request_path.exists())

    def test_control_cleanup_removes_request_status_and_atomic_temps(self) -> None:
        directory, instance = self.make_fixture()
        root = Path(directory.name)
        instance.control_request_path.write_text(
            "transport-loss-cleanup\tstop\n", encoding="utf-8"
        )
        instance.control_status_path.write_text("old\n", encoding="utf-8")
        temporary = root / ".sshd-control-status.transport-loss-cleanup.deadbeef"
        temporary.write_text("partial\n", encoding="utf-8")
        instance.stop_control()
        self.assertFalse(instance.control_request_path.exists())
        self.assertFalse(instance.control_status_path.exists())
        self.assertFalse(temporary.exists())
        directory.cleanup()

    def test_control_path_validation_rejects_non_private_fixture_root(self) -> None:
        directory, instance = self.make_fixture()
        self.addCleanup(directory.cleanup)
        instance.root.chmod(0o755)
        with mock.patch.dict(os.environ, self.control_environment(instance), clear=False):
            with self.assertRaises(fixture.FixtureError) as error:
                fixture.send_control("stop", timeout=1)
        self.assertEqual(str(error.exception), "fixture control path is invalid")

    def test_cli_control_requires_no_new_fixture_arguments(self) -> None:
        args = fixture._parse_args(["--control", "stop"])
        self.assertEqual(args.control, "stop")
        with self.assertRaises(SystemExit):
            fixture._parse_args(["--control", "stop", "--check"])
        with self.assertRaises(SystemExit):
            fixture._parse_args(["--control", "start", "--env-file", "/tmp/x"])


if __name__ == "__main__":
    unittest.main()
