#!/usr/bin/env bash
set -Eeuo pipefail

readonly package_name="dev.meeterm.app"
readonly launcher_package_name="com.google.android.apps.nexuslauncher"
readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/android-emulator-observability"
readonly apk_path="${artifact_dir}/app-release.apk"
readonly foundation_url="meeterm://foundation?foundation=1"
readonly anr_trace_max_bytes=262144
readonly anr_trace_command_timeout_seconds=15

launcher_stabilizer_status="not_run"
launcher_anr_recovery_status="not_observed"
app_pid_status="not_captured"

mkdir -p "${artifact_dir}"

collect_artifacts() {
  local exit_status=$?
  trap - EXIT
  set +e

  if [[ -f "${artifact_dir}/launch.txt" ]]; then
    adb exec-out screencap -p > "${artifact_dir}/terminal.png"
    scripts/ci/validate-png.sh \
      "${artifact_dir}/terminal.png" \
      "${artifact_dir}/screenshot-unavailable.txt"
  fi
  adb logcat -d -v threadtime \
    | grep -E 'Meeterm|MEETERM_SMOKE_|AndroidRuntime|ActivityManager|WindowManager|SystemUI|ANR|dev\.meeterm\.app' \
    > "${artifact_dir}/logcat.txt"
  adb shell dumpsys window windows \
    | grep -E 'mCurrentFocus|mFocusedApp|Application Not Responding|dev\.meeterm\.app|com\.android\.systemui' \
    > "${artifact_dir}/window.txt"
  adb shell dumpsys activity activities \
    | grep -E 'mResumedActivity|topResumedActivity|dev\.meeterm\.app' \
    > "${artifact_dir}/activity.txt"
  adb shell dumpsys activity processes \
    | grep -A 16 -B 4 "${package_name}" \
    > "${artifact_dir}/process.txt"
  capture_app_anr_trace
  {
    echo "api=$(adb shell getprop ro.build.version.sdk | tr -d '\r')"
    echo "abi=$(adb shell getprop ro.product.cpu.abi | tr -d '\r')"
    echo "device=$(adb shell getprop ro.product.model | tr -d '\r')"
    echo "launcher_stabilizer=${launcher_stabilizer_status}"
    echo "launcher_anr_recovery=${launcher_anr_recovery_status}"
    echo "app_pid_identity=${app_pid_status}"
    echo "smoke_exit=${exit_status}"
  } >> "${artifact_dir}/metadata.txt"

  exit "${exit_status}"
}
trap collect_artifacts EXIT

is_github_actions() {
  [[ "${GITHUB_ACTIONS:-}" == "true" ]]
}

window_anr_present_for() {
  local package=$1
  local window_output
  window_output="$(adb shell dumpsys window windows 2>/dev/null)" || return 1
  # dumpsys window is live state.  Match the concrete ANR window title so an
  # old logcat ANR cannot trigger recovery or mask a later app crash.
  grep -Fq "Application Not Responding: ${package}" <<<"${window_output}"
}

launcher_anr_window_present() {
  window_anr_present_for "${launcher_package_name}"
}

app_anr_window_present() {
  window_anr_present_for "${package_name}"
}

extract_app_anr_section() {
  awk -v package_name="${package_name}" '
    $0 == ("Cmd line: " package_name) { keep = 1; print; next }
    keep && /^----- pid / { keep = 0 }
    keep { print }
  '
}

capture_app_anr_trace() {
  local trace_path="${artifact_dir}/anr-trace.txt"
  local latest_trace raw_trace filtered_trace dropbox_filtered app_process_id

  # The system has already written an ANR record by the time the concrete
  # dialog is visible. Read only the newest record and retain the section for
  # this package; do not include other applications' stacks in the artifact.
  if ! app_anr_window_present; then
    printf 'capture=not_requested\nreason=app_anr_window_not_present\n' > "${trace_path}"
    return 0
  fi

  app_process_id="$(app_pid || true)"
  latest_trace="$(adb shell 'ls -t /data/anr/anr_* 2>/dev/null | head -n 1' 2>/dev/null | tr -d '\r' || true)"
  {
    echo "capture=requested"
    echo "package=${package_name}"
    echo "pid=${app_process_id:-unavailable}"
    echo "source=${latest_trace:-unavailable}"
    echo "max_bytes=${anr_trace_max_bytes}"
  } > "${trace_path}"

  if [[ -n "${latest_trace}" && "${latest_trace}" == /data/anr/anr_* ]]; then
    raw_trace="$(adb shell cat "${latest_trace}" 2>/dev/null | tr -d '\r' || true)"
    filtered_trace="$(printf '%s\n' "${raw_trace}" | extract_app_anr_section | head -c "${anr_trace_max_bytes}" || true)"
    if [[ -n "${filtered_trace}" ]]; then
      echo "trace=app_section" >> "${trace_path}"
      printf '%s\n' "${filtered_trace}" >> "${trace_path}"
      return 0
    fi
  fi

  if [[ -z "${latest_trace}" || "${latest_trace}" != /data/anr/anr_* ]]; then
    echo "trace=unavailable" >> "${trace_path}"
  else
    echo "trace=unavailable_or_unreadable" >> "${trace_path}"
  fi

  # Shell normally cannot read /data/anr on production-like images.  The
  # shell dumpsys path is the non-root fallback; retain only the newest
  # data_app_anr entry containing this package and then only its stack section.
  dropbox_filtered="$(
    timeout "${anr_trace_command_timeout_seconds}s" \
      adb shell dumpsys dropbox --print data_app_anr 2>/dev/null \
      | tr -d '\r' \
      | awk -v package_name="${package_name}" '
        function finish_block() {
          if (found) {
            last = block
          }
        }
        /^========================================$/ {
          if (started) {
            finish_block()
          }
          started = 1
          block = $0 "\n"
          found = 0
          next
        }
        started {
          block = block $0 "\n"
          if ($0 == ("Cmd line: " package_name)) {
            found = 1
          }
        }
        END {
          if (started) {
            finish_block()
          }
          if (last != "") {
            printf "%s", last
          }
        }
      ' \
      | extract_app_anr_section \
      | head -c "${anr_trace_max_bytes}" \
      || true
  )"
  if [[ -z "${dropbox_filtered}" ]]; then
    echo "dropbox_fallback=unavailable_or_unreadable" >> "${trace_path}"
  else
    {
      echo "dropbox_fallback=app_section"
      echo "dropbox_source=data_app_anr"
      printf '%s\n' "${dropbox_filtered}"
    } >> "${trace_path}"
  fi
}

app_is_foreground() {
  local window_output current_focus focused_app activity_output resumed_activity
  window_output="$(adb shell dumpsys window windows 2>/dev/null)" || return 1
  current_focus="$(grep -F 'mCurrentFocus' <<<"${window_output}" || true)"
  if [[ -n "${current_focus}" ]]; then
    grep -Fq "${package_name}/" <<<"${current_focus}"
    return
  fi
  focused_app="$(grep -F 'mFocusedApp' <<<"${window_output}" || true)"
  if [[ -n "${focused_app}" ]]; then
    grep -Fq "${package_name}/" <<<"${focused_app}"
    return
  fi

  # Android API 36 can omit mCurrentFocus/mFocusedApp from the `windows`
  # subcommand even while the activity is visibly resumed. Keep the stronger
  # window-focus checks when they are available, then fall back to Activity
  # Manager's live resumed-activity state rather than treating a missing dump
  # field as an app failure.
  activity_output="$(adb shell dumpsys activity activities 2>/dev/null)" || return 1
  resumed_activity="$(grep -E 'mResumedActivity|topResumedActivity' <<<"${activity_output}" || true)"
  [[ -n "${resumed_activity}" ]] && grep -Fq "${package_name}/" <<<"${resumed_activity}"
}

start_meeterm() {
  adb shell am start -W \
    -a android.intent.action.VIEW \
    -d "${foundation_url}" \
    -n "${package_name}/.MainActivity" \
    >/dev/null
}

app_pid() {
  local pid_output
  pid_output="$(adb shell pidof -s "${package_name}" 2>/dev/null | tr -d '\r\n')" || return 1
  [[ "${pid_output}" =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "${pid_output}"
}

wait_for_app_pid() {
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    local current_pid
    current_pid="$(app_pid || true)"
    if [[ -n "${current_pid}" ]]; then
      printf '%s\n' "${current_pid}"
      return 0
    fi
    sleep 1
  done
  return 1
}

same_app_pid() {
  local current_pid
  current_pid="$(app_pid || true)"
  [[ -n "${current_pid}" && "${current_pid}" == "${initial_pid}" ]]
}

wait_for_same_process_and_foreground() {
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if ! same_app_pid; then
      return 1
    fi
    if app_anr_window_present; then
      return 1
    fi
    if app_is_foreground; then
      return 0
    fi
    sleep 1
  done
  return 1
}

recover_launcher_anr() {
  launcher_anr_recovery_status="attempted"
  # This recovery is deliberately limited to the one observed Pixel Launcher
  # package.  It does not dismiss arbitrary dialogs or weaken meeterm's own
  # process/native-marker gates.
  if ! same_app_pid; then
    launcher_anr_recovery_status="refused_app_pid_changed"
    app_pid_status="changed_or_dead"
    return 1
  fi
  adb shell am force-stop "${launcher_package_name}"
  if ! same_app_pid; then
    launcher_anr_recovery_status="refused_app_pid_changed"
    app_pid_status="changed_or_dead"
    return 1
  fi
  if ! start_meeterm; then
    launcher_anr_recovery_status="failed"
    return 1
  fi
  if ! wait_for_same_process_and_foreground; then
    launcher_anr_recovery_status="failed"
    if same_app_pid; then
      app_pid_status="retained_recovery_failed"
    else
      app_pid_status="changed_or_dead"
    fi
    return 1
  fi
  launcher_anr_recovery_status="recovered"
  app_pid_status="retained_after_launcher_recovery"
}

test -f "${apk_path}"
adb wait-for-device
adb logcat -c

# sys.boot_completed can become true while System UI is still starting on a
# fresh CI AVD. Give it a bounded readiness/settling window before app launch
# so a transient platform ANR does not cover the visual observability capture.
system_ui_deadline=$((SECONDS + 60))
while (( SECONDS < system_ui_deadline )); do
  if adb shell pidof -s com.android.systemui | grep -Eq '[0-9]'; then
    break
  fi
  sleep 2
done
sleep 10

if is_github_actions; then
  # A bounded CI-only stabilizer for the observed Pixel Launcher ANR.  The
  # platform logs remain in logcat.txt for review; no meeterm process checks
  # or native smoke markers are bypassed.
  adb shell am force-stop "${launcher_package_name}"
  launcher_stabilizer_status="forced_stop_before_launch"
  sleep 2
else
  launcher_stabilizer_status="skipped_outside_ci"
fi

adb install -r "${apk_path}"
adb shell am force-stop "${package_name}"
start_meeterm
{
  echo "package=${package_name}"
  echo "launch_method=am_start_foundation_url"
  echo "launch_uri=${foundation_url}"
  echo "launcher_stabilizer=${launcher_stabilizer_status}"
} > "${artifact_dir}/launch.txt"
sleep 2

initial_pid=""
if ! initial_pid="$(wait_for_app_pid)"; then
  echo "The Android app did not expose a stable process after explicit launch." >&2
  exit 1
fi
app_pid_status="captured"

deadline=$((SECONDS + 120))
native_ready=0
first_frame=0
launcher_recovery_attempted=0
while (( SECONDS < deadline )); do
  current_log="$(adb logcat -d -v brief MeetermTerminalView:I MeetermRenderer:I '*:S')"
  if grep -Fq 'MEETERM_SMOKE_NATIVE_READY' <<<"${current_log}"; then
    native_ready=1
  fi
  if grep -Fq 'MEETERM_SMOKE_FIRST_FRAME' <<<"${current_log}"; then
    first_frame=1
  fi

  if app_anr_window_present; then
    echo "The Android app has an Application Not Responding window." >&2
    exit 1
  fi

  if is_github_actions && (( launcher_recovery_attempted == 0 )) && launcher_anr_window_present; then
    echo "Observed the known Pixel Launcher ANR; restarting the explicit app activity." >&2
    launcher_recovery_attempted=1
    recover_launcher_anr
  fi

  if (( native_ready == 1 && first_frame == 1 )); then
    break
  fi

  if ! same_app_pid; then
    app_pid_status="changed_or_dead"
    echo "The Android app exited before the native smoke markers appeared." >&2
    exit 1
  fi
  sleep 2
done

if (( native_ready != 1 || first_frame != 1 )); then
  echo "Timed out waiting for both native smoke markers." >&2
  exit 1
fi

if ! same_app_pid; then
  app_pid_status="changed_or_dead"
  echo "The Android app exited after its native smoke markers." >&2
  exit 1
fi
if app_anr_window_present; then
  echo "The Android app has an Application Not Responding window." >&2
  exit 1
fi
if ! wait_for_same_process_and_foreground; then
  if ! same_app_pid; then
    app_pid_status="changed_or_dead"
    echo "The Android app process changed or exited while checking foreground state." >&2
    exit 1
  fi
  if app_anr_window_present; then
    echo "The Android app has an Application Not Responding window." >&2
    exit 1
  fi
  if is_github_actions && (( launcher_recovery_attempted == 0 )) && launcher_anr_window_present; then
    echo "Observed the known Pixel Launcher ANR while checking foreground state; restarting the explicit app activity." >&2
    launcher_recovery_attempted=1
    recover_launcher_anr
  else
    echo "The Android app was not foreground after its native smoke markers." >&2
    exit 1
  fi
fi

# Give asynchronous GPU work and crash reporting a short observation window.
sleep 5
if ! same_app_pid; then
  app_pid_status="changed_or_dead"
  echo "The Android app exited after its first native frame." >&2
  exit 1
fi
if app_anr_window_present; then
  echo "The Android app has an Application Not Responding window." >&2
  exit 1
fi

echo "Android native terminal smoke passed."
