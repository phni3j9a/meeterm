#!/usr/bin/env bash
set -Eeuo pipefail

readonly bundle_id="dev.meeterm.app"
readonly app_path="${RUNNER_TEMP}/meeterm-derived-data/Build/Products/Release-iphonesimulator/meeterm.app"
readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"

: "${IOS_SIMULATOR_UDID:?IOS_SIMULATOR_UDID was not exported}"
mkdir -p "${artifact_dir}"
test -d "${app_path}"

xcrun simctl install "${IOS_SIMULATOR_UDID}" "${app_path}"
launch_output="$(xcrun simctl launch "${IOS_SIMULATOR_UDID}" "${bundle_id}")"
printf '%s\n' "${launch_output}" | tee "${artifact_dir}/launch.txt"
app_pid="$(sed -E 's/.*: ([0-9]+)$/\1/' <<<"${launch_output}")"
if ! [[ "${app_pid}" =~ ^[0-9]+$ ]]; then
  echo "Unable to determine the launched iOS app PID." >&2
  exit 1
fi

deadline=$((SECONDS + 120))
native_ready=0
first_frame=0
renderer_backend="unavailable"
while (( SECONDS < deadline )); do
  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
    --style compact \
    --last 3m \
    --predicate 'process == "meeterm" OR eventMessage CONTAINS "MEETERM_SMOKE_"' \
    > "${artifact_dir}/simulator.log" 2>&1 || true

  if grep -Fq 'MEETERM_SMOKE_NATIVE_READY' "${artifact_dir}/simulator.log"; then
    native_ready=1
  fi
  if grep -Fq 'MEETERM_SMOKE_FIRST_FRAME_METAL' "${artifact_dir}/simulator.log"; then
    first_frame=1
    renderer_backend="metal"
  elif grep -Fq 'MEETERM_SMOKE_FIRST_FRAME_SOFTWARE' "${artifact_dir}/simulator.log"; then
    first_frame=1
    renderer_backend="software-simulator-fallback"
  fi
  if (( native_ready == 1 && first_frame == 1 )); then
    break
  fi

  if ! kill -0 "${app_pid}" 2>/dev/null; then
    echo "The iOS app exited before the native smoke markers appeared." >&2
    exit 1
  fi
  sleep 2
done

if (( native_ready != 1 || first_frame != 1 )); then
  echo "Timed out waiting for both iOS native smoke markers." >&2
  exit 1
fi

echo "renderer_backend=${renderer_backend}" >> "${artifact_dir}/metadata.txt"

sleep 5
if ! kill -0 "${app_pid}" 2>/dev/null; then
  echo "The iOS app exited after its first native frame." >&2
  exit 1
fi

echo "iOS native terminal smoke passed."
