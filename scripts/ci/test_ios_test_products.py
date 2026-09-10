#!/usr/bin/env python3
import importlib.util
import io
import json
import plistlib
import tarfile
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('products', Path(__file__).with_name('ios-test-products.py'))
products = importlib.util.module_from_spec(spec)
spec.loader.exec_module(products)


class TestProducts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.built = self.root / 'Build/Products'
        app = self.built / 'Release-iphonesimulator/meeterm.app'
        app.mkdir(parents=True)
        (app / 'meeterm').write_bytes(b'native-app')
        (app / 'meeterm').chmod(0o755)
        (app / 'binary-link').symlink_to('meeterm')
        self.config = self.built / 'meeterm.xctestrun'
        self.config.write_bytes(plistlib.dumps({'Tests': {'TestBundlePath': str(self.built / 'test.xctest')}}))
        self.package = self.root / 'package'
        self.sha = 'a' * 40
        self.xcode = 'Xcode fixture\nBuild version fixture'

    def pack(self):
        products.pack(self.built, self.package, self.sha, self.xcode)

    def restore(self, **changes):
        products.restore(self.package, self.root / 'restored', changes.get('sha', self.sha), changes.get('xcode', self.xcode))

    def test_round_trip_preserves_app_and_relocates_test_paths(self):
        self.pack()
        self.restore()
        output = self.root / 'restored/Products'
        app = output / 'Release-iphonesimulator/meeterm.app'
        self.assertEqual((app / 'meeterm').read_bytes(), b'native-app')
        self.assertEqual((app / 'meeterm').stat().st_mode & 0o777, 0o755)
        self.assertTrue((app / 'binary-link').is_symlink())
        self.assertEqual((app / 'binary-link').read_bytes(), b'native-app')
        config = plistlib.loads((output / 'meeterm.xctestrun').read_bytes())
        self.assertEqual(config['Tests']['TestBundlePath'], '__TESTROOT__/test.xctest')

    def test_rejects_other_commit_or_xcode_before_extracting(self):
        self.pack()
        for change in ({'sha': 'b' * 40}, {'xcode': 'different'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.restore(**change)
            self.assertFalse((self.root / 'restored').exists())

    def test_rejects_other_architecture(self):
        self.pack()
        path = self.package / 'manifest.json'
        data = json.loads(path.read_text())
        data['architecture'] = 'other'
        path.write_text(json.dumps(data))
        with self.assertRaises(ValueError):
            self.restore()

    def test_rejects_modified_archive(self):
        self.pack()
        with (self.package / 'products.tar.gz').open('ab') as stream:
            stream.write(b'changed')
        with self.assertRaises(ValueError):
            self.restore()

    def test_rejects_packaging_runtime_fixture_environment(self):
        for name in ('MEETERM_SSH_HOST', 'MEETERM_IOS_MARKER_PATH'):
            with self.subTest(name=name):
                self.config.write_bytes(plistlib.dumps({'Tests': {'EnvironmentVariables': {name: 'value'}}}))
                with self.assertRaises(ValueError):
                    self.pack()
                self.assertFalse(self.package.exists())

    def test_rejects_missing_app_or_ambiguous_test_config(self):
        (self.built / 'second.xctestrun').write_bytes(b'not a plist')
        with self.assertRaises(ValueError):
            self.pack()
        (self.built / 'second.xctestrun').unlink()
        (self.built / 'Release-iphonesimulator/meeterm.app/meeterm').unlink()
        with self.assertRaises(ValueError):
            self.pack()

    def test_rejects_traversal_even_with_matching_digest(self):
        self.pack()
        archive = self.package / 'products.tar.gz'
        with tarfile.open(archive, 'w:gz') as bundle:
            member = tarfile.TarInfo('Products/../../escaped')
            member.size = 4
            bundle.addfile(member, io.BytesIO(b'nope'))
        manifest = self.package / 'manifest.json'
        data = json.loads(manifest.read_text())
        data['sha256'] = products.digest(archive)
        manifest.write_text(json.dumps(data))
        with self.assertRaises(tarfile.FilterError):
            self.restore()
        self.assertFalse((self.root / 'escaped').exists())

    def test_restore_does_not_overwrite_existing_products(self):
        self.pack()
        (self.root / 'restored/Products').mkdir(parents=True)
        with self.assertRaises(ValueError):
            self.restore()


if __name__ == '__main__':
    unittest.main()
