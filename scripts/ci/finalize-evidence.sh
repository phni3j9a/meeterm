#!/usr/bin/env bash
set -u

readonly evidence_name="${1:?evidence directory name is required}"
readonly artifact_dir="${GITHUB_WORKSPACE}/artifacts/${evidence_name}"

mkdir -p "${artifact_dir}"

if [[ ! -s "${artifact_dir}/terminal.png" && ! -s "${artifact_dir}/screenshot-unavailable.txt" ]]; then
  echo "app launch was not reached, or screenshot collection did not run" \
    > "${artifact_dir}/screenshot-unavailable.txt"
fi

if [[ ! -s "${artifact_dir}/logcat.txt" && ! -s "${artifact_dir}/simulator.log" ]]; then
  echo "native runtime log was unavailable before artifact upload" \
    > "${artifact_dir}/runtime-log-unavailable.txt"
fi
