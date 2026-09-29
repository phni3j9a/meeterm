#!/usr/bin/env python3
"""Select CI jobs from a complete git diff; unknown inputs select every check."""

import argparse
import subprocess
from pathlib import PurePosixPath


CHECKS = ("javascript", "rust", "ios", "android", "scripts", "dependencies")


def select_checks(paths):
    selected = set()
    for path in paths:
        name = PurePosixPath(path).name
        if path.startswith(("docs/", "artifacts/")) or name.endswith(".md") or name in ("LICENSE", "OFL.txt"):
            continue
        if path in (".github/workflows/ci.yml", "scripts/ci/select-checks.py", "scripts/ci/test_select_checks.py"):
            selected.update(CHECKS)
        elif path in ("package.json", "package-lock.json", ".nvmrc", ".npmrc", "app.json", "app.config.ts") or name in ("expo-module.config.json", "MeetermTerminal.podspec"):
            selected.update(("javascript", "ios", "android", "scripts", "dependencies"))
        elif path.startswith(("plugins/", "app/assets/")):
            selected.update(("javascript", "ios", "android", "scripts"))
        elif path.startswith("native/"):
            selected.update(("rust", "ios", "android", "scripts"))
        elif path.startswith("modules/meeterm-terminal/android/"):
            selected.update(("android", "scripts"))
        elif path.startswith("modules/meeterm-terminal/ios/"):
            selected.update(("ios", "scripts"))
        elif path.startswith("app/") or path.endswith((".ts", ".tsx", ".js", ".cjs")) or path == "tsconfig.json":
            selected.add("javascript")
        elif path.startswith("scripts/"):
            selected.add("scripts")
            if path.endswith(".swift") or path in ("scripts/ci/ios-typecheck.sh", "scripts/ci/ios-inject-ui-test.py", "scripts/ci/ios-inject-ui-test.sh", "scripts/ci/test_ios_selection_text.py"):
                selected.add("ios")
            if path == "scripts/ssh/fixture.py" or path.startswith("scripts/herdr/"):
                selected.add("rust")
        else:
            # New build/config inputs must not silently bypass verification.
            selected.update(CHECKS)
    return selected


def changed_paths(base, head, merge_base=False):
    if merge_base:
        base = subprocess.check_output(["git", "merge-base", base, head], text=True).strip()
    # Disable rename detection to include both the old and new path's checks.
    data = subprocess.check_output(["git", "diff", "--name-only", "--no-renames", "-z", base, head, "--"])
    return data.decode("utf-8", errors="surrogateescape").split("\0")[:-1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?")
    parser.add_argument("head", nargs="?", default="HEAD")
    parser.add_argument("--merge-base", action="store_true")
    parser.add_argument("--all", action="store_true")
    args = parser.parse_args()
    if args.all or args.base == "0" * 40:
        selected = set(CHECKS)
    elif args.base:
        selected = select_checks(changed_paths(args.base, args.head, args.merge_base))
    else:
        parser.error("provide a base commit or --all")
    for check in CHECKS:
        print(f"{check}={str(check in selected).lower()}")


if __name__ == "__main__":
    main()
