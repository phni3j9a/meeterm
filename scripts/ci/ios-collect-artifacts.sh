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

  xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
    --style compact \
    --last 10m \
    --predicate 'process == "meeterm" OR eventMessage CONTAINS "MEETERM_SMOKE_"' \
    > "${artifact_dir}/simulator.log" 2>&1 || true
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
