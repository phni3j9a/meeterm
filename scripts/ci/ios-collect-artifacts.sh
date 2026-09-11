#!/usr/bin/env bash
set -u

readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
readonly suite="${MEETERM_IOS_SUITE:-full}"
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

# Only full acceptance requests a fresh foundation frame. Focused suites
# preserve their own safe checkpoints without reporting missing full-flow UI.
required_screenshots=()
case "${suite}" in
  full)
    if [[ -f "${artifact_dir}/terminal.png" ]]; then
      scripts/ci/validate-png.sh \
        "${artifact_dir}/terminal.png" \
        "${artifact_dir}/screenshot-unavailable.txt"
    elif [[ -f "${artifact_dir}/launch.txt" ]]; then
      echo "XCTest did not capture the fresh native foundation; terminal screenshot unavailable" \
        > "${artifact_dir}/screenshot-unavailable.txt"
    fi
    required_screenshots=(
      connection-form-keyboard host-trust workspaces workspace-switched
      pane-switched terminal-keyboard terminal-input disconnected reconnected
    )
    ;;
  forms)
    echo "suite=forms; native foundation screenshot not requested; see focused form checkpoints" \
      > "${artifact_dir}/screenshot-unavailable.txt"
    required_screenshots=(forms-keyboard password-form-keyboard forms-controls)
    ;;
  native)
    echo "suite=native; UI screenshots not requested for storage/input unit cases" \
      > "${artifact_dir}/screenshot-unavailable.txt"
    ;;
  names)
    echo "suite=names; native foundation screenshot not requested; see SSH name-operation checkpoints" \
      > "${artifact_dir}/screenshot-unavailable.txt"
    required_screenshots=(connection-form-keyboard host-trust workspaces daily-workspace-create-form daily-created-pane)
    ;;
  *)
    echo "unsupported iOS evidence suite" > "${artifact_dir}/screenshot-unavailable.txt"
    exit 2
    ;;
esac
printf 'suite=%s\n' "${suite}" > "${artifact_dir}/screenshot-scope.txt"

# Check readability for human review, never as an image-existence acceptance
# gate. No collector branch captures arbitrary UI after a test.
missing_screenshots=()
for screenshot_name in "${required_screenshots[@]+"${required_screenshots[@]}"}"; do
  screenshot_path="${artifact_dir}/${screenshot_name}.png"
  diagnostic_path="${artifact_dir}/${screenshot_name}-unavailable.txt"
  if [[ -f "${screenshot_path}" ]]; then
    scripts/ci/validate-png.sh "${screenshot_path}" "${diagnostic_path}"
  fi
  if [[ ! -f "${screenshot_path}" ]]; then
    missing_screenshots+=("${screenshot_name}")
  fi
done
if [[ -n "${missing_screenshots[*]-}" ]]; then
  printf 'suite=%s; checkpoint screenshot unavailable: %s\n' "${suite}" "${missing_screenshots[*]}" \
    > "${artifact_dir}/ui-screenshots-unavailable.txt"
else
  rm -f "${artifact_dir}/ui-screenshots-unavailable.txt"
fi

{
  echo "xcode_developer_dir=${DEVELOPER_DIR:-$(xcode-select -p 2>/dev/null || true)}"
  echo "simulator_udid=${IOS_SIMULATOR_UDID:-unavailable}"
  echo "simulator_name=${IOS_SIMULATOR_NAME:-unavailable}"
} >> "${artifact_dir}/metadata.txt"

echo "raw_crash_reports=omitted" >> "${artifact_dir}/metadata.txt"
