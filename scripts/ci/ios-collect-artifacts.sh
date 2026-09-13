#!/usr/bin/env bash
set -u

readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
readonly suite="${MEETERM_IOS_SUITE:-standard}"
readonly log_collection_report="${artifact_dir}/simulator-log-collection.txt"
mkdir -p "${artifact_dir}"
find "${artifact_dir}" -maxdepth 1 -type f \
  \( -iname 'meeterm*.crash' -o -iname 'meeterm*.ips' \) \
  -delete 2>/dev/null || true
rm -f "${log_collection_report}"

log_start_metadata="missing"
if [[ -f "${artifact_dir}/launch.txt" ]]; then
  log_start_metadata="$(python3 - "${artifact_dir}/launch.txt" <<'PY' 2>/dev/null || true
from datetime import datetime
from pathlib import Path
import re
import sys

try:
    lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
except (OSError, UnicodeError):
    print("missing")
    raise SystemExit(0)

values = [line.split("=", 1)[1] for line in lines
          if line.startswith("smoke_started_at_utc=")]
if len(values) != 1:
    print("missing" if not values else "invalid")
    raise SystemExit(0)

value = values[0]
if not re.fullmatch(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}Z", value):
    print("invalid")
    raise SystemExit(0)
try:
    datetime.strptime(value, "%Y-%m-%d %H:%M:%SZ")
except ValueError:
    print("invalid")
    raise SystemExit(0)
print(f"valid:{value[:-1]}")
PY
)"
fi

log_collection_source="unavailable"
log_collection_reason="simulator_or_launch_metadata_missing"
log_command_status="not_run"
log_marker_status="absent"
log_collection_start=""
if [[ -s "${artifact_dir}/simulator.log" ]]; then
  log_collection_source="existing_simulator_log"
  log_collection_reason="none"
  log_command_status="existing"
elif [[ -n "${IOS_SIMULATOR_UDID:-}" && "${log_start_metadata}" == valid:* ]]; then
  # Collect only explicit native smoke markers even when the real UI test
  # fails before the post-test foundation launch. The launch metadata is UTC;
  # keep the timezone explicit in the log query as well.
  log_collection_start="${log_start_metadata#valid:}"
  if command -v xcrun >/dev/null 2>&1; then
    if xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
        --style compact \
        --timezone UTC \
        --start "${log_collection_start}" \
        --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
        > "${artifact_dir}/simulator.log" 2>/dev/null; then
      log_command_status="passed"
    else
      # Do not upload a command error or partial output as if it were the
      # sanitized simulator marker log.
      rm -f "${artifact_dir}/simulator.log"
      log_command_status="failed"
    fi
  else
    log_command_status="tool_missing"
  fi
  log_collection_source="launch_metadata"
  log_collection_reason="none"
elif [[ -n "${IOS_SIMULATOR_UDID:-}" ]]; then
  # Older runs have no start metadata. Retain a bounded compatibility window,
  # and expose whether metadata was absent or malformed rather than claiming
  # that the complete startup history was collected.
  if [[ "${log_start_metadata}" == invalid ]]; then
    log_collection_reason="invalid_start_metadata"
  else
    log_collection_reason="start_metadata_missing"
  fi
  if command -v xcrun >/dev/null 2>&1; then
    if xcrun simctl spawn "${IOS_SIMULATOR_UDID}" log show \
        --style compact \
        --timezone UTC \
        --last 10m \
        --predicate 'process == "meeterm" AND eventMessage CONTAINS "MEETERM_SMOKE_"' \
        > "${artifact_dir}/simulator.log" 2>/dev/null; then
      log_command_status="passed"
    else
      rm -f "${artifact_dir}/simulator.log"
      log_command_status="failed"
    fi
  else
    log_command_status="tool_missing"
  fi
  log_collection_source="last10m_fallback"
fi

if [[ "${log_command_status}" == "existing" || "${log_command_status}" == "passed" ]] \
    && [[ -s "${artifact_dir}/simulator.log" ]] \
    && grep -Eq 'meeterm\[[0-9]+:[^]]+\].*MEETERM_SMOKE_' "${artifact_dir}/simulator.log"; then
  log_marker_status="present"
fi

case "${log_command_status}" in
  tool_missing)
    log_collection_result="unavailable"
    log_collection_reason="log_tool_missing"
    ;;
  failed)
    log_collection_result="unavailable"
    log_collection_reason="log_command_failed"
    ;;
  existing|passed)
    if [[ "${log_marker_status}" == "present" ]]; then
      log_collection_result="available"
      log_collection_reason="none"
    else
      log_collection_result="unavailable"
      # Keep a missing/invalid start reason for compatibility runs; the
      # separate marker_status field still records that no marker was found.
      if [[ "${log_collection_reason}" == "none" ]]; then
        log_collection_reason="no_smoke_markers"
      fi
    fi
    ;;
  *)
    log_collection_result="unavailable"
    ;;
esac
printf 'source=%s\nresult=%s\nreason=%s\n' \
  "${log_collection_source}" "${log_collection_result}" "${log_collection_reason}" \
  > "${log_collection_report}"
printf 'command_status=%s\nmarker_status=%s\n' \
  "${log_command_status}" "${log_marker_status}" \
  >> "${log_collection_report}"

# Standard and full request a fresh foundation frame. The standard suite also
# preserves the direct seeded-screen checkpoints emitted by XCTest, including
# the four Herdr presentation fixtures. These
# files are human-review evidence; their presence never decides pass/fail.
required_screenshots=()
diagnostic_screenshots=()
case "${suite}" in
  standard)
    if [[ -f "${artifact_dir}/terminal.png" ]]; then
      scripts/ci/validate-png.sh \
        "${artifact_dir}/terminal.png" \
        "${artifact_dir}/screenshot-unavailable.txt"
    elif [[ -f "${artifact_dir}/launch.txt" ]]; then
      echo "XCTest did not capture the fresh native foundation; terminal screenshot unavailable" \
        > "${artifact_dir}/screenshot-unavailable.txt"
    fi
    required_screenshots=(
      standard-home standard-servers standard-connection standard-password
      standard-workspaces standard-terminal standard-settings
      standard-workspace-name standard-terminal-name standard-handoff
      standard-herdr-connection standard-herdr-groups standard-herdr-terminal
      standard-herdr-workspaces
    )
    ;;
  polish)
    if [[ -f "${artifact_dir}/terminal.png" ]]; then
      scripts/ci/validate-png.sh "${artifact_dir}/terminal.png" "${artifact_dir}/screenshot-unavailable.txt"
    else
      echo "Polish XCTest did not capture its fresh native foundation" > "${artifact_dir}/screenshot-unavailable.txt"
    fi
    required_screenshots=(
      polish-welcome polish-empty polish-search-empty polish-disconnected polish-reconnecting polish-connection-error polish-long-workspaces
      polish-terminal-keyboard polish-edge-back
    )
    ;;
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
  ssh)
    echo "suite=ssh; safe post-auth checkpoint screenshots are optional for the short SSH smoke" \
      > "${artifact_dir}/screenshot-unavailable.txt"
    required_screenshots=(ssh-terminal-input ssh-disconnected)
    # These are emitted only while the test-local pre-credential permission is
    # armed. They are useful failure evidence, never a success requirement.
    diagnostic_screenshots=(ssh-entry-initial ssh-entry-failure)
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

for screenshot_name in "${diagnostic_screenshots[@]+${diagnostic_screenshots[@]}}"; do
  screenshot_path="${artifact_dir}/${screenshot_name}.png"
  diagnostic_path="${artifact_dir}/${screenshot_name}-unavailable.txt"
  if [[ -f "${screenshot_path}" ]]; then
    scripts/ci/validate-png.sh "${screenshot_path}" "${diagnostic_path}"
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
