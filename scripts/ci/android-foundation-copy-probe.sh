#!/usr/bin/env bash
set -Eeuo pipefail

readonly probe_artifact_dir="artifacts/android-foundation-copy-probe"
mkdir -p "${probe_artifact_dir}"

smoke_status=0
scripts/ci/android-smoke.sh || smoke_status=$?

probe_status=0
python3 scripts/ssh/android_foundation_copy_probe.py \
  --artifact-dir "${probe_artifact_dir}" || probe_status=$?

{
  echo "smoke_status=${smoke_status}"
  echo "probe_status=${probe_status}"
} > "${probe_artifact_dir}/status.txt"

if (( smoke_status != 0 || probe_status != 0 )); then
  exit 1
fi
