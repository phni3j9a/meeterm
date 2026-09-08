#!/usr/bin/env bash
set -Eeuo pipefail

readonly bundle_id="${BUNDLE_ID:-dev.meeterm.app}"
readonly app_path="${RUNNER_TEMP}/meeterm-derived-data/Build/Products/Release-iphonesimulator/meeterm.app"
readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
readonly fixture_env="${RUNNER_TEMP}/meeterm-ssh.env"
readonly derived_data="${RUNNER_TEMP}/meeterm-derived-data"

: "${IOS_SIMULATOR_UDID:?IOS_SIMULATOR_UDID was not exported}"
mkdir -p "${artifact_dir}"
test -d "${app_path}"

printf '%s\n' \
  "mode=xcuitest-real-ssh" \
  "bundle_id=${bundle_id}" \
  "test_result_bundle=RUNNER_TEMP" \
  > "${artifact_dir}/launch.txt"

fixture_pid=""
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "${fixture_pid}" ]]; then
    kill "${fixture_pid}" 2>/dev/null || true
    wait "${fixture_pid}" 2>/dev/null || true
  fi
  exit "${status}"
}
trap cleanup EXIT INT TERM

rm -f "${fixture_env}"
python3 "${GITHUB_WORKSPACE}/scripts/ssh/fixture.py" --env-file "${fixture_env}" &
fixture_pid=$!

fixture_deadline=$((SECONDS + 30))
while [[ ! -s "${fixture_env}" ]]; do
  if ! kill -0 "${fixture_pid}" 2>/dev/null; then
    echo "The disposable OpenSSH fixture exited before becoming ready." >&2
    exit 1
  fi
  if (( SECONDS >= fixture_deadline )); then
    echo "The disposable OpenSSH fixture did not become ready." >&2
    exit 1
  fi
  sleep 0.2
done

# fixture.py writes shell-quoted values and never prints the key/passphrase.
# Avoid xtrace around this source operation and around the XCUITest invocation.
# shellcheck disable=SC1090
source "${fixture_env}"
xcrun simctl uninstall "${IOS_SIMULATOR_UDID}" "${bundle_id}" 2>/dev/null || true
xcrun simctl install "${IOS_SIMULATOR_UDID}" "${app_path}"
smoke_started_at="$(date '+%Y-%m-%d %H:%M:%S')"

python3 "${GITHUB_WORKSPACE}/scripts/ssh/ios-smoke.py" \
  --artifact-dir "${artifact_dir}" \
  --derived-data "${derived_data}" \
  --simulator-udid "${IOS_SIMULATOR_UDID}"

# This is the real UI gate. The iOS terminal view is reached by the SSH UI
# test above, so its native-ready and first-frame markers must be newer than
# this smoke run. Keep only marker events in the artifact; raw XCTest output
# and the xcresult remain under RUNNER_TEMP.
xctest_log="${artifact_dir}/xcuitest-simulator.log"
xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
  --style compact \
  --start "${smoke_started_at}" \
  --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
  > "${xctest_log}" 2>&1 || true
if ! grep -Fq 'MEETERM_SMOKE_NATIVE_READY' "${xctest_log}"; then
  echo "The real XCUITest did not report a fresh native-ready marker." >&2
  exit 1
fi
if ! grep -Eq 'MEETERM_SMOKE_FIRST_FRAME_(METAL|SOFTWARE)' "${xctest_log}"; then
  echo "The real XCUITest did not report a fresh native first-frame marker." >&2
  exit 1
fi

if grep -Fq 'MEETERM_SMOKE_FIRST_FRAME_METAL' "${xctest_log}"; then
  echo "xcuitest_renderer_backend=metal" >> "${artifact_dir}/metadata.txt"
else
  echo "xcuitest_renderer_backend=software-simulator-fallback" >> "${artifact_dir}/metadata.txt"
fi

# The XCUITest terminates its app instance after the handoff assertion. Launch
# a fresh self-contained app once more. The explicit foundation URL is allowed
# only in this smoke build; normal app launches still start at the real
# workspace hub without local demo data. This gives the post-test no-crash
# gate a native surface whose markers are filtered to the new app PID.
post_test_started_at="$(date '+%Y-%m-%d %H:%M:%S')"
launch_output="$(xcrun simctl launch "${IOS_SIMULATOR_UDID}" "${bundle_id}")"
printf '%s\n' "${launch_output}" > "${artifact_dir}/post-test-launch.txt"
app_pid="$(sed -E 's/.*: ([0-9]+)$/\1/' <<<"${launch_output}")"
if ! [[ "${app_pid}" =~ ^[0-9]+$ ]]; then
  echo "Unable to determine the post-test iOS app PID." >&2
  exit 1
fi
xcrun simctl openurl "${IOS_SIMULATOR_UDID}" 'meeterm://foundation?foundation=1' \
  > "${artifact_dir}/post-test-openurl.txt" 2>&1
marker_predicate="process == \"meeterm\" AND processIdentifier == ${app_pid} AND eventMessage CONTAINS \"MEETERM_SMOKE_\""
simulator_app_is_running() {
  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" launchctl print system 2>/dev/null \
    | grep -Fq "${bundle_id}"
}

deadline=$((SECONDS + 120))
native_ready=0
first_frame=0
renderer_backend="unavailable"
while (( SECONDS < deadline )); do
  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
    --style compact \
    --start "${post_test_started_at}" \
    --predicate "${marker_predicate}" \
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

  if ! simulator_app_is_running; then
    echo "The post-test iOS app exited before the native smoke markers appeared." >&2
    exit 1
  fi
  sleep 2
done

if (( native_ready != 1 || first_frame != 1 )); then
  echo "Timed out waiting for both iOS native smoke markers." >&2
  exit 1
fi

sleep 5
if ! simulator_app_is_running; then
  echo "The post-test iOS app exited after its first native frame." >&2
  exit 1
fi

echo "renderer_backend=${renderer_backend}" >> "${artifact_dir}/metadata.txt"
echo "iOS real SSH UI smoke passed."
