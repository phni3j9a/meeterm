#!/usr/bin/env bash
set -u

readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
mkdir -p "${artifact_dir}"
find "${artifact_dir}" -maxdepth 1 -type f \
  \( -iname 'meeterm*.crash' -o -iname 'meeterm*.ips' \) \
  -delete 2>/dev/null || true

if [[ -n "${IOS_SIMULATOR_UDID:-}" && -f "${artifact_dir}/launch.txt" && ! -s "${artifact_dir}/simulator.log" ]]; then
  # Collect only explicit native smoke markers even when the real UI test
  # fails before the post-test foundation launch. This keeps failure evidence
  # useful without exposing process logs or entered credentials.
  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
    --style compact \
    --last 10m \
    --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
    > "${artifact_dir}/simulator.log" 2>&1 || true
fi

# XCTest captures this only after the fresh foundation preview is visible and
# its foreground survival observation completes. Never capture an arbitrary
# screen after XCTest: it may be a credentials form or a system URL dialog.
if [[ -f "${artifact_dir}/terminal.png" ]]; then
  scripts/ci/validate-png.sh \
    "${artifact_dir}/terminal.png" \
    "${artifact_dir}/screenshot-unavailable.txt"
elif [[ -f "${artifact_dir}/launch.txt" ]]; then
  echo "XCTest did not capture the fresh native foundation; terminal screenshot unavailable" \
    > "${artifact_dir}/screenshot-unavailable.txt"
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
  terminal-input
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

echo "raw_crash_reports=omitted" >> "${artifact_dir}/metadata.txt"
