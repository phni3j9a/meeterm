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


if __name__ == '__main__':
    unittest.main()
