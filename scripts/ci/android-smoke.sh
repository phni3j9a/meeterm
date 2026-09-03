#!/usr/bin/env bash
set -Eeuo pipefail

readonly package_name="dev.meeterm.app"
readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/android-emulator-observability"
readonly apk_path="${GITHUB_WORKSPACE}/android/app/build/outputs/apk/release/app-release.apk"

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
    | grep -E 'Meeterm|MEETERM_SMOKE_|AndroidRuntime|FATAL EXCEPTION|dev\.meeterm\.app' \
    > "${artifact_dir}/logcat.txt"
  adb shell dumpsys activity processes \
    | grep -A 16 -B 4 "${package_name}" \
    > "${artifact_dir}/process.txt"
  {
    echo "api=$(adb shell getprop ro.build.version.sdk | tr -d '\r')"
    echo "abi=$(adb shell getprop ro.product.cpu.abi | tr -d '\r')"
    echo "device=$(adb shell getprop ro.product.model | tr -d '\r')"
    echo "smoke_exit=${exit_status}"
  } >> "${artifact_dir}/metadata.txt"

  exit "${exit_status}"
}
trap collect_artifacts EXIT

test -f "${apk_path}"
adb wait-for-device
adb logcat -c
adb install -r "${apk_path}"
adb shell am force-stop "${package_name}"
adb shell monkey -p "${package_name}" -c android.intent.category.LAUNCHER 1
echo "package=${package_name}" > "${artifact_dir}/launch.txt"
sleep 2

deadline=$((SECONDS + 120))
native_ready=0
first_frame=0
while (( SECONDS < deadline )); do
  current_log="$(adb logcat -d -v brief MeetermTerminalView:I MeetermRenderer:I '*:S')"
  if grep -Fq 'MEETERM_SMOKE_NATIVE_READY' <<<"${current_log}"; then
    native_ready=1
  fi
  if grep -Fq 'MEETERM_SMOKE_FIRST_FRAME' <<<"${current_log}"; then
    first_frame=1
  fi
  if (( native_ready == 1 && first_frame == 1 )); then
    break
  fi

  if ! adb shell pidof -s "${package_name}" | grep -Eq '[0-9]'; then
    echo "The Android app exited before the native smoke markers appeared." >&2
    exit 1
  fi
  sleep 2
done

if (( native_ready != 1 || first_frame != 1 )); then
  echo "Timed out waiting for both native smoke markers." >&2
  exit 1
fi

# Give asynchronous GPU work and crash reporting a short observation window.
sleep 5
if ! adb shell pidof -s "${package_name}" | grep -Eq '[0-9]'; then
  echo "The Android app exited after its first native frame." >&2
  exit 1
fi

echo "Android native terminal smoke passed."
