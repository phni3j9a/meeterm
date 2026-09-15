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
