#!/usr/bin/env bash
set -Eeuo pipefail

readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/ios-simulator-observability"
mkdir -p "${artifact_dir}"

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

xcrun simctl boot "${simulator_udid}" 2>/dev/null || true
xcrun simctl bootstatus "${simulator_udid}" -b

{
  echo "IOS_SIMULATOR_UDID=${simulator_udid}"
  echo "IOS_SIMULATOR_NAME=${simulator_name}"
} >> "${GITHUB_ENV}"
{
  echo "simulator_udid=${simulator_udid}"
  echo "simulator_name=${simulator_name}"
} >> "${artifact_dir}/metadata.txt"

xcrun simctl list devices "${simulator_udid}" | tee "${artifact_dir}/simulator.txt"
