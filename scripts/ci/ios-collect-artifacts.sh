#!/usr/bin/env bash
set -u

readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
mkdir -p "${artifact_dir}"

if [[ -n "${IOS_SIMULATOR_UDID:-}" && -f "${artifact_dir}/launch.txt" ]]; then
  xcrun simctl io "${IOS_SIMULATOR_UDID}" screenshot \
    "${artifact_dir}/terminal.png" 2>&1 \
    | tee "${artifact_dir}/screenshot.txt" || true
  scripts/ci/validate-png.sh \
    "${artifact_dir}/terminal.png" \
    "${artifact_dir}/screenshot-unavailable.txt"

  # Preserve the fresh PID/time-filtered log produced by ios-smoke.sh. If the
  # smoke did not reach that point, collect marker events only; broad process
  # logs could include connection details and are not useful evidence here.
  if [[ ! -s "${artifact_dir}/simulator.log" ]]; then
    xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
      --style compact \
      --last 10m \
      --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
      > "${artifact_dir}/simulator.log" 2>&1 || true
  fi
fi

# XCUITest writes only safe checkpoints: the connection form is captured
# before any credential is entered, and all later captures happen after the
# form has closed. Validate each checkpoint for artifact readability without
# turning screenshot availability into a machine acceptance gate.
required_screenshots=(
  connection-form-keyboard
  host-trust
  workspaces
  workspace-switched
  pane-switched
  terminal-keyboard
  disconnected
  reconnected
)
missing_screenshots=()
for screenshot_name in "${required_screenshots[@]}"; do
  screenshot_path="${artifact_dir}/${screenshot_name}.png"
  diagnostic_path="${artifact_dir}/${screenshot_name}-unavailable.txt"
  if [[ -f "${screenshot_path}" ]]; then
    scripts/ci/validate-png.sh "${screenshot_path}" "${diagnostic_path}"
  else
    missing_screenshots+=("${screenshot_name}")
  fi
done
if (( ${#missing_screenshots[@]} > 0 )); then
  printf 'checkpoint screenshot unavailable: %s\n' "${missing_screenshots[*]}" \
    > "${artifact_dir}/ui-screenshots-unavailable.txt"
else
  rm -f "${artifact_dir}/ui-screenshots-unavailable.txt"
fi

{
  echo "xcode_developer_dir=$(xcode-select -p 2>/dev/null || true)"
  echo "simulator_udid=${IOS_SIMULATOR_UDID:-unavailable}"
  echo "simulator_name=${IOS_SIMULATOR_NAME:-unavailable}"
} >> "${artifact_dir}/metadata.txt"

diagnostic_root="${HOME}/Library/Logs/DiagnosticReports"
if [[ -d "${diagnostic_root}" ]]; then
  find "${diagnostic_root}" -maxdepth 1 -type f \
    \( -iname 'meeterm*.crash' -o -iname 'meeterm*.ips' \) \
    -exec cp {} "${artifact_dir}/" \; 2>/dev/null || true
fi
