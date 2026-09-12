#!/usr/bin/env python3
"""Remove the app-owned Rust library from disposable iOS storage test flags."""

from __future__ import annotations

import re
import sys
from pathlib import Path


STORAGE_CONFIG_DIRECTORY = Path(
    "ios/Pods/Target Support Files/Pods-meetermStorageTests"
)
STORAGE_CONFIG_NAMES = (
    "Pods-meetermStorageTests.debug.xcconfig",
    "Pods-meetermStorageTests.release.xcconfig",
)
OTHER_LDFLAGS = re.compile(r"^[ \t]*OTHER_LDFLAGS[ \t]*=", re.MULTILINE)
CORE_LIBRARY_TOKEN = re.compile(r'(?<!\S)-l"meeterm_core"(?!\S)')


def patched_storage_config(contents: str, path: Path) -> str:
    definitions = list(OTHER_LDFLAGS.finditer(contents))
    if len(definitions) != 1:
        raise SystemExit(
            f"iOS storage linker isolation expected one OTHER_LDFLAGS in {path}"
        )

    line_start = definitions[0].start()
    line_end = contents.find("\n", line_start)
    if line_end < 0:
        line_end = len(contents)
    line = contents[line_start:line_end]
    tokens = list(CORE_LIBRARY_TOKEN.finditer(line))
    if contents.count("meeterm_core") != 1 or len(tokens) != 1:
        raise SystemExit(
            f"iOS storage linker isolation expected one exact meeterm_core token in {path}"
        )

    token = tokens[0]
    absolute_start = line_start + token.start()
    absolute_end = line_start + token.end()
    return contents[:absolute_start] + contents[absolute_end:]


def main(argv: list[str]) -> int:
    if len(argv) > 2:
        raise SystemExit("usage: ios-strip-storage-rust-link.py [repository-root]")
    root = Path(argv[1]) if len(argv) == 2 else Path.cwd()
    paths = [root / STORAGE_CONFIG_DIRECTORY / name for name in STORAGE_CONFIG_NAMES]

    originals: dict[Path, str] = {}
    for path in paths:
        try:
            originals[path] = path.read_bytes().decode("utf-8")
        except (OSError, UnicodeDecodeError) as error:
            raise SystemExit(
                f"iOS storage linker isolation could not read {path}: {error}"
            ) from error

    # Validate both generated configurations before changing either one.
    patched = {
        path: patched_storage_config(contents, path)
        for path, contents in originals.items()
    }
    for path in paths:
        try:
            path.write_bytes(patched[path].encode("utf-8"))
        except OSError as error:
            raise SystemExit(
                f"iOS storage linker isolation could not write {path}: {error}"
            ) from error

    print("Removed the app-owned Rust link from iOS storage test configurations.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
