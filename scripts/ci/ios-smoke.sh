#!/usr/bin/env bash
set -Eeuo pipefail

readonly bundle_id="${BUNDLE_ID:-dev.meeterm.app}"
readonly app_path="${RUNNER_TEMP}/meeterm-derived-data/Build/Products/Release-iphonesimulator/meeterm.app"
readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
readonly fixture_env="${RUNNER_TEMP}/meeterm-ssh.env"
readonly derived_data="${RUNNER_TEMP}/meeterm-derived-data"
readonly suite="${MEETERM_IOS_SUITE:-standard}"

: "${IOS_SIMULATOR_UDID:?IOS_SIMULATOR_UDID was not exported}"
case "${suite}" in
  standard|ssh|full|forms|native|names) ;;
  *) echo "Unsupported iOS smoke suite: ${suite}" >&2; exit 2 ;;
esac
mkdir -p "${artifact_dir}"
test -d "${app_path}"
rm -f \
  "${artifact_dir}/ios-foundation-observation.json" \
  "${artifact_dir}/ios-foundation-validation.txt" \
  "${artifact_dir}/ios-standard-validation.txt" \
  "${artifact_dir}/ios-ssh-validation.txt" \
  "${artifact_dir}/ios-ui-standard-validation.txt" \
  "${artifact_dir}/ios-ui-ssh-validation.txt" \
  "${artifact_dir}/ios-ui-names-validation.txt" \
  "${artifact_dir}/ios-names-validation.txt" \
  "${artifact_dir}/terminal.png" \
  "${artifact_dir}/standard-home.png" \
  "${artifact_dir}/standard-servers.png" \
  "${artifact_dir}/standard-connection.png" \
  "${artifact_dir}/standard-password.png" \
  "${artifact_dir}/standard-workspaces.png" \
  "${artifact_dir}/standard-terminal.png" \
  "${artifact_dir}/standard-settings.png" \
  "${artifact_dir}/standard-workspace-name.png" \
  "${artifact_dir}/standard-terminal-name.png" \
  "${artifact_dir}/standard-handoff.png" \
  "${artifact_dir}/ssh-terminal-input.png" \
  "${artifact_dir}/ssh-disconnected.png" \
  "${artifact_dir}/simulator.log"

printf '%s\n' \
  "mode=xcuitest-${suite}" \
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

if [[ "${suite}" == "ssh" || "${suite}" == "full" || "${suite}" == "names" ]]; then
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
else
  # Standard/forms/native suites are deliberately independent of the disposable SSH fixture.
  # Remove any inherited fixture contract before xcodebuild can pass it on.
  unset MEETERM_SSH_HOST MEETERM_SSH_PORT MEETERM_SSH_USERNAME \
    MEETERM_SSH_FINGERPRINT MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE \
    MEETERM_SSH_PRIVATE_KEY_FILE MEETERM_SSH_PASSPHRASE \
    MEETERM_SSH_KNOWN_HOSTS_FILE MEETERM_SSH_HOST_KEY_FILE \
    MEETERM_SSH_ALTERNATE_HOST_KEY_FILE
fi
xcrun simctl uninstall "${IOS_SIMULATOR_UDID}" "${bundle_id}" 2>/dev/null || true
xcrun simctl install "${IOS_SIMULATOR_UDID}" "${app_path}"
if [[ "${suite}" == "standard" || "${suite}" == "ssh" || "${suite}" == "full" ]]; then
  smoke_started_at="$(date -u '+%Y-%m-%d %H:%M:%S')"
fi

python3 "${GITHUB_WORKSPACE}/scripts/ssh/ios-smoke.py" \
  --artifact-dir "${artifact_dir}" \
  --derived-data "${derived_data}" \
  --simulator-udid "${IOS_SIMULATOR_UDID}" \
  --suite "${suite}"

if [[ "${suite}" == "forms" || "${suite}" == "native" || "${suite}" == "names" ]]; then
  echo "iOS ${suite} focused smoke passed."
  exit 0
fi

if [[ "${suite}" == "ssh" ]]; then
  # The short SSH suite has no foundation URL relaunch. Its native terminal
  # still must report readiness and a renderer-specific first frame from the
  # post-install process before the Swift foreground assertion ends.
  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
    --style compact --timezone UTC \
    --start "${smoke_started_at}" \
    --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
    > "${artifact_dir}/simulator.log" 2>&1 || true
  if ! grep -Fq 'MEETERM_SMOKE_NATIVE_READY' "${artifact_dir}/simulator.log"; then
    echo "The short SSH XCUITest did not report a fresh native-ready marker." >&2
    exit 1
  fi
  if ! grep -Eq 'MEETERM_SMOKE_FIRST_FRAME_(METAL|SOFTWARE)' "${artifact_dir}/simulator.log"; then
    echo "The short SSH XCUITest did not report a native first-frame marker." >&2
    exit 1
  fi
  if grep -Fq 'MEETERM_SMOKE_FIRST_FRAME_METAL' "${artifact_dir}/simulator.log"; then
    echo "ssh_renderer_backend=metal" >> "${artifact_dir}/metadata.txt"
  else
    echo "ssh_renderer_backend=software-simulator-fallback" >> "${artifact_dir}/metadata.txt"
  fi
  echo "iOS short SSH UI smoke passed."
  exit 0
fi

# Standard and full both finish with the same fresh foundation launch. Standard
# has no real-SSH phase before it, while full splits its pre-foundation marker
# log so foundation frames cannot satisfy the real SSH gate. The timestamp is
# generated by XCTest, never by app input.
foundation_started_at="$(python3 - "${artifact_dir}/ios-foundation-observation.json" <<'PYTIME'
from datetime import datetime, timezone
import json
import sys
with open(sys.argv[1], encoding="utf-8") as stream:
    observation = json.load(stream)
print(datetime.fromtimestamp(observation["launch_epoch"], timezone.utc).strftime("%Y-%m-%d %H:%M:%S"))
PYTIME
)"
if [[ "${suite}" == "full" ]]; then
  xctest_log="${artifact_dir}/xcuitest-simulator.log"
  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
    --style compact --timezone UTC \
    --start "${smoke_started_at}" --end "${foundation_started_at}" \
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
fi

# Require native readiness and a renderer-specific first frame from the same
# fresh process before the complete five-second XCTest survival observation.
# A visible React Native title or a screenshot alone cannot satisfy this gate.
xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
  --style compact --timezone UTC \
  --start "${foundation_started_at}" \
  --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
  > "${artifact_dir}/simulator.log" 2>&1 || true
python3 "${GITHUB_WORKSPACE}/scripts/ci/ios-validate-foundation.py" \
  --artifact-dir "${artifact_dir}"
if [[ "${suite}" == "standard" ]]; then
  echo "iOS standard seeded-screen and fresh native foundation smoke passed."
else
  echo "iOS real SSH UI and fresh native foundation smoke passed."
fi
