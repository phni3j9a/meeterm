#!/usr/bin/env python3
"""Prepare and verify the standalone Android development release in CI."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zipfile

APP = "meeterm"
PACKAGE = "dev.meeterm.app"
LIBRARY = "libmeeterm_core.so"
# Expo's existing evaluation builds use this shared development certificate.
# Fail instead of silently making an APK that cannot update existing installs.
CERTIFICATE = "fac61745dc0903786fb9ede62a962b399f7348f0bb6f899b8332667591033b9c"
OUT = Path("dist/android")


def output(*args):
    return subprocess.check_output(args, text=True).strip()


def prepare():
    sha = output("git", "rev-parse", "HEAD")
    # First-parent history counts main commits, never CI reruns/completion order.
    # A forward-only main keeps this monotonic; keep the offset when changing CI.
    version = 100000 + int(output("git", "rev-list", "--first-parent", "--count", "HEAD"))
    if not 1 <= version <= 2100000000:
        raise ValueError("Android versionCode is out of range")
    config_path = Path("app.json")
    config = json.loads(config_path.read_text())
    config["expo"]["android"]["versionCode"] = version
    config_path.write_text(json.dumps(config, indent=2) + "\n")
    OUT.mkdir(parents=True, exist_ok=True)
    tag = f"android-{version}-{sha[:7]}"
    metadata = {"app": APP, "commit": sha, "versionCode": version, "tag": tag,
                "abis": ["arm64-v8a", "x86_64"], "signingCertificateSha256": CERTIFICATE}
    (OUT / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")
    if "GITHUB_OUTPUT" in os.environ:
        with open(os.environ["GITHUB_OUTPUT"], "a") as f:
            f.write(f"tag={tag}\nversion_code={version}\n")
    print(f"Prepared {APP} {tag}")


def verify():
    metadata = json.loads((OUT / "build.json").read_text())
    apk = Path("android/app/build/outputs/apk/release/app-release.apk")
    tools = Path(os.environ["ANDROID_HOME"]) / "build-tools/36.0.0"
    certificate = output(str(tools / "apksigner"), "verify", "--print-certs", str(apk))
    digests = re.findall(r"Signer #\d+ certificate SHA-256 digest: ([0-9a-f]+)", certificate)
    if digests != [CERTIFICATE]:
        raise ValueError("APK signing certificate differs from existing evaluation builds")
    badging = output(str(tools / "aapt"), "dump", "badging", str(apk))
    identity = re.search(r"package: name='([^']+)' versionCode='(\d+)'", badging)
    if not identity or identity.groups() != (PACKAGE, str(metadata["versionCode"])):
        raise ValueError("APK package or versionCode mismatch")
    if "application-debuggable" in badging:
        raise ValueError("Distribution APK must use the non-debuggable Release variant")
    with zipfile.ZipFile(apk) as archive:
        required = ["assets/index.android.bundle"]
        required += [f"lib/{abi}/{LIBRARY}" for abi in metadata["abis"]]
        for name in required:
            if archive.getinfo(name).file_size == 0:
                raise ValueError(f"Empty APK entry: {name}")
    target = OUT / f"{APP}.apk"
    shutil.copyfile(apk, target)
    with target.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    (OUT / "SHA256SUMS").write_text(f"{digest}  {target.name}\n")
    print(f"Verified {target.name}: signature, version, Release, bundle and native libraries")


if __name__ == "__main__":
    if sys.argv[1:] == ["prepare"]:
        prepare()
    elif sys.argv[1:] == ["verify"]:
        verify()
    else:
        raise SystemExit("usage: android-release.py prepare|verify")
