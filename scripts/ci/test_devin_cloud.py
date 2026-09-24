#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('devin_cloud', Path(__file__).with_name('devin-cloud.py'))
devin_cloud = importlib.util.module_from_spec(spec)
spec.loader.exec_module(devin_cloud)

CONFIG = [
    {'id': 'repos', 'currentValue': '', 'options': [{'name': 'meeterm', 'value': 'phni3j9a/meeterm'}]},
    {'id': 'devin_version', 'currentValue': 'devin-2-5', 'options': [
        {'name': 'Normal', 'value': 'devin-2-5'},
        {'name': 'SWE-2', 'options': [{'value': 'devin-swe-2-high'}, {'value': 'devin-swe-2-max'}]},
    ]},
    {'id': 'platform', 'currentValue': 'linux', 'options': [{'value': 'linux'}, {'value': 'macos'}]},
]


class TestDevinCloud(unittest.TestCase):
    def test_option_values_flatten_grouped_options(self):
        self.assertEqual(devin_cloud.option_values(CONFIG[1]), ['devin-2-5', 'devin-swe-2-high', 'devin-swe-2-max'])

    def test_require_option_accepts_offered_values(self):
        devin_cloud.require_option(CONFIG, 'devin_version', 'devin-swe-2-max')
        devin_cloud.require_option(CONFIG, 'platform', 'macos')

    def test_require_option_fails_closed_for_unoffered_model_or_missing_option(self):
        with self.assertRaisesRegex(devin_cloud.AcpError, 'not offered'):
            devin_cloud.require_option(CONFIG, 'devin_version', 'devin-swe-3-max')
        with self.assertRaisesRegex(devin_cloud.AcpError, 'not offered by this Devin Cloud relay'):
            devin_cloud.require_option(CONFIG[:1], 'platform', 'macos')

    def test_session_row_reads_cloud_metadata_and_defaults_platform(self):
        row = devin_cloud.session_row({'sessionId': 'devin-abc', 'title': 't', '_meta': {
            'cognition.ai/sessionStatus': 'suspended',
            'cognition.ai/platform': None,
            'cognition.ai/devinVersionOverride': 'devin-swe-2-max',
            'cognition.ai/isArchived': False,
            'cognition.ai/url': 'https://app.devin.ai/sessions/abc',
        }})
        self.assertEqual(row['platform'], 'linux')
        self.assertEqual(row['version'], 'devin-swe-2-max')
        self.assertFalse(row['archived'])

    def test_full_session_id_accepts_web_url_id(self):
        self.assertEqual(devin_cloud.full_session_id('abc'), 'devin-abc')
        self.assertEqual(devin_cloud.full_session_id('devin-abc'), 'devin-abc')


class FakeRemote:
    def __init__(self, heads):
        self.heads = list(heads)
        self.now = 0.0
        self.sleeps = []

    def head(self, cwd, branch):
        value = self.heads.pop(0) if len(self.heads) > 1 else self.heads[0]
        if isinstance(value, Exception):
            raise value
        return value

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds

    def clock(self):
        return self.now

    def wait(self, after=None, timeout=300, interval=60):
        return devin_cloud.wait_evidence('.', 'evidence/ios', after, timeout, interval,
                                         head=self.head, sleep=self.sleep, clock=self.clock)


class TestWaitEvidence(unittest.TestCase):
    def test_returns_the_first_head_past_the_starting_head(self):
        remote = FakeRemote(['aaa', 'aaa', 'aaa', 'bbb'])
        self.assertEqual(remote.wait(), 'bbb')
        self.assertEqual(remote.sleeps, [60, 60])

    def test_explicit_after_ignores_the_current_head(self):
        remote = FakeRemote(['aaa'])
        self.assertEqual(remote.wait(after='old'), 'aaa')
        self.assertEqual(remote.sleeps, [])

    def test_missing_branch_waits_for_its_first_commit(self):
        remote = FakeRemote(['', '', 'ccc'])
        self.assertEqual(remote.wait(), 'ccc')

    def test_timeout_returns_none_without_oversleeping(self):
        remote = FakeRemote(['aaa'])
        self.assertIsNone(remote.wait(timeout=150))
        self.assertEqual(remote.sleeps, [60, 60, 30])

    def test_transient_remote_error_keeps_waiting(self):
        remote = FakeRemote(['aaa', devin_cloud.AcpError('network'), 'ddd'])
        self.assertEqual(remote.wait(), 'ddd')


if __name__ == '__main__':
    unittest.main()
