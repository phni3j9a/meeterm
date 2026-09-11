#!/usr/bin/env python3
"""Package pristine iOS test products and restore only an exact-source build."""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
import plistlib
import re
import tarfile
from pathlib import Path


def digest(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def portable_plist(value: object, products: Path) -> object:
    if isinstance(value, dict):
        if any(str(key).startswith(('MEETERM_SSH_', 'MEETERM_IOS_')) for key in value):
            raise ValueError('test products already contain runtime fixture variables')
        return {key: portable_plist(item, products) for key, item in value.items()}
    if isinstance(value, list):
        return [portable_plist(item, products) for item in value]
    if isinstance(value, str):
        # macOS temporary directories can be spelled /var/... or /private/var/....
        # Xcode may preserve either spelling; relocate both, longest first.
        for root in sorted({str(products.absolute()), str(products.resolve())}, key=len, reverse=True):
            value = value.replace(root, '__TESTROOT__')
        return value
    return value


def pack(products: Path, output: Path, source_sha: str, xcode: str) -> None:
    bundles = list(products.glob('*.xctestrun'))
    if len(bundles) != 1 or not (products / 'Release-iphonesimulator/meeterm.app/meeterm').is_file():
        raise ValueError('expected one xctestrun and a built Simulator app')
    with bundles[0].open('rb') as stream:
        document = portable_plist(plistlib.load(stream), products)
    # Only the build's test configuration is made relocatable. No runtime
    # fixture has started yet; raw XCTest logs/results are never packaged.
    with bundles[0].open('wb') as stream:
        plistlib.dump(document, stream, sort_keys=False)
    output.mkdir(parents=True, exist_ok=True)
    archive = output / 'products.tar.gz'
    with tarfile.open(archive, 'w:gz') as bundle:
        bundle.add(products, arcname='Products')
    (output / 'manifest.json').write_text(json.dumps({
        'version': 1, 'source_sha': source_sha, 'xcode': xcode,
        'architecture': platform.machine(), 'sha256': digest(archive),
        'scope': 'pristine-build-products', 'configuration': 'Release-iphonesimulator',
    }, indent=2) + '\n')


def restore(package: Path, destination: Path, source_sha: str, xcode: str) -> None:
    manifest = json.loads((package / 'manifest.json').read_text())
    expected = {'version': 1, 'source_sha': source_sha, 'xcode': xcode,
                'architecture': platform.machine(), 'scope': 'pristine-build-products',
                'configuration': 'Release-iphonesimulator'}
    if any(manifest.get(key) != value for key, value in expected.items()):
        raise ValueError('test product source, Xcode, architecture or scope mismatch')
    archive = package / 'products.tar.gz'
    if manifest.get('sha256') != digest(archive):
        raise ValueError('test product checksum mismatch')
    if (destination / 'Products').exists():
        raise ValueError('restore requires an empty Products destination')
    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive, 'r:gz') as bundle:
        for entry in bundle.getmembers():
            if not (entry.name == 'Products' or entry.name.startswith('Products/')):
                raise ValueError('unexpected test product archive root')
        bundle.extractall(destination, filter='data')
    bundles = list((destination / 'Products').glob('*.xctestrun'))
    if len(bundles) != 1:
        raise ValueError('restored test configuration is unavailable')
    with bundles[0].open('rb') as stream:
        portable_plist(plistlib.load(stream), destination / 'Products')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=['pack', 'restore'])
    parser.add_argument('--products', type=Path)
    parser.add_argument('--package', type=Path, required=True)
    parser.add_argument('--destination', type=Path)
    parser.add_argument('--source-sha', required=True)
    parser.add_argument('--xcode-version-file', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', args.source_sha):
        parser.error('source SHA must be a full commit ID')
    xcode = args.xcode_version_file.read_text().strip()
    if args.operation == 'pack':
        if args.products is None:
            parser.error('pack requires --products')
        pack(args.products, args.package, args.source_sha, xcode)
    else:
        if args.destination is None:
            parser.error('restore requires --destination')
        restore(args.package, args.destination, args.source_sha, xcode)
    print(f'iOS test products {args.operation} passed (exact source/toolchain).')


if __name__ == '__main__':
    main()
