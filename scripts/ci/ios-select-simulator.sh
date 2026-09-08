#!/usr/bin/env bash
set -Eeuo pipefail

readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
readonly bootstatus_timeout_seconds=900
mkdir -p "${artifact_dir}"
echo "bootstatus_timeout_seconds=${bootstatus_timeout_seconds}" >> "${artifact_dir}/metadata.txt"

devices_json="$(xcrun simctl list devices available --json)"
simulator_udid="$(jq -r '
  [.devices | to_entries[]
    | select(.key | contains("iOS"))
    | .value[]
    | select(.isAvailable == true and (.name | startswith("iPhone")))]
  | (map(select(.name | test(" Pro$"))) + .)
  | .[0].udid // empty
' <<<"${devices_json}")"

if [[ -z "${simulator_udid}" ]]; then
  echo "No available iPhone Simulator was found." >&2
  exit 1
fi

simulator_name="$(jq -r --arg udid "${simulator_udid}" '
  [.devices[][] | select(.udid == $udid)][0].name
' <<<"${devices_json}")"

xcrun simctl boot "${simulator_udid}" \
  > "${artifact_dir}/boot-request.txt" 2>&1 || true

# `simctl bootstatus -b` can wait indefinitely when CoreSimulator is wedged.
# Keep its diagnostic output in the evidence directory and bound the wait so
# the hosted job reaches its always-run artifact steps.
bootstatus_log="${artifact_dir}/bootstatus.log"
xcrun simctl bootstatus "${simulator_udid}" -b \
  > "${bootstatus_log}" 2>&1 &
bootstatus_pid=$!
bootstatus_deadline=$((SECONDS + bootstatus_timeout_seconds))
bootstatus_timed_out=0
while kill -0 "${bootstatus_pid}" 2>/dev/null; do
  if (( SECONDS >= bootstatus_deadline )); then
    bootstatus_timed_out=1
    printf '%s\n' \
      "bootstatus=timeout" \
      "timeout_seconds=${bootstatus_timeout_seconds}" \
      >> "${bootstatus_log}"
    kill "${bootstatus_pid}" 2>/dev/null || true
    for _ in 1 2 3 4 5; do
      if ! kill -0 "${bootstatus_pid}" 2>/dev/null; then
        break
      fi
      sleep 1
    done
    kill -KILL "${bootstatus_pid}" 2>/dev/null || true
    break
  fi
  sleep 2
done

bootstatus_exit=0
wait "${bootstatus_pid}" || bootstatus_exit=$?
if (( bootstatus_timed_out == 1 )); then
  printf '%s\n' \
    "bootstatus=unavailable" \
    "reason=timeout" \
    "timeout_seconds=${bootstatus_timeout_seconds}" \
    > "${artifact_dir}/bootstatus-unavailable.txt"
  exit 1
fi
if (( bootstatus_exit != 0 )); then
  printf '%s\n' \
    "bootstatus=unavailable" \
    "reason=simctl_exit_${bootstatus_exit}" \
    > "${artifact_dir}/bootstatus-unavailable.txt"
  exit 1
fi
printf '%s\n' 'bootstatus=ready' >> "${bootstatus_log}"

{
  echo "IOS_SIMULATOR_UDID=${simulator_udid}"
  echo "IOS_SIMULATOR_NAME=${simulator_name}"
} >> "${GITHUB_ENV}"
{
  echo "simulator_udid=${simulator_udid}"
  echo "simulator_name=${simulator_name}"
} >> "${artifact_dir}/metadata.txt"

xcrun simctl list devices "${simulator_udid}" | tee "${artifact_dir}/simulator.txt"
