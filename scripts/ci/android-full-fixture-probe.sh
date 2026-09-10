#!/usr/bin/env bash
set -Eeuo pipefail

readonly fixture_artifact_dir="artifacts/android-full-fixture-probe"
mkdir -p "${fixture_artifact_dir}"

smoke_status=0
scripts/ci/android-smoke.sh || smoke_status=$?

fixture_status=0
python3 scripts/ssh/fixture.py -- \
  python3 scripts/ssh/android-smoke.py \
  --artifact-dir "${fixture_artifact_dir}" || fixture_status=$?

{
  echo "smoke_status=${smoke_status}"
  echo "fixture_status=${fixture_status}"
} > "${fixture_artifact_dir}/status.txt"

if (( smoke_status != 0 || fixture_status != 0 )); then
  exit 1
fi
