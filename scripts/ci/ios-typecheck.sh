#!/usr/bin/env bash
set -Eeuo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_directory
repository_root="$(cd -- "${script_directory}/../.." && pwd)"
readonly repository_root
readonly deployment_target="16.4"
readonly swift_target="x86_64-apple-ios${deployment_target}-simulator"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "iOS Swift typecheck requires macOS with Xcode (host=$(uname -s))." >&2
  exit 1
fi

if ! command -v xcrun >/dev/null 2>&1; then
  echo "iOS Swift typecheck requires xcrun from Xcode." >&2
  exit 1
fi

readonly driver_source="${repository_root}/scripts/ci/MeetermSmokeUITests.swift"
readonly input_tests_source="${repository_root}/scripts/ci/TerminalInputViewTests.swift"
readonly native_input_source="${repository_root}/modules/meeterm-terminal/ios/TerminalInputView.swift"
readonly native_key_source="${repository_root}/modules/meeterm-terminal/ios/TerminalSpecialKey.swift"
# Production storage tests are intentionally outside this preflight: their
# @testable import requires the built production app module and app host.

for source in \
  "${driver_source}" \
  "${input_tests_source}" \
  "${native_input_source}" \
  "${native_key_source}"; do
  if [[ ! -f "${source}" ]]; then
    echo "iOS Swift typecheck source is missing: ${source}" >&2
    exit 1
  fi
done

sdk_path="$(xcrun --sdk iphonesimulator --show-sdk-path)"
readonly sdk_path
sdk_version="$(xcrun --sdk iphonesimulator --show-sdk-version)"
readonly sdk_version
platform_path="$(xcrun --sdk iphonesimulator --show-sdk-platform-path)"
readonly platform_path
readonly xctest_frameworks_path="${platform_path}/Developer/Library/Frameworks"

if [[ ! -d "${sdk_path}" ]]; then
  echo "iOS Simulator SDK was not found: ${sdk_path}" >&2
  exit 1
fi
if [[ ! -d "${xctest_frameworks_path}/XCTest.framework" ]]; then
  echo "XCTest.framework was not found: ${xctest_frameworks_path}" >&2
  exit 1
fi

temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/meeterm-ios-typecheck.XXXXXX")"
readonly temporary_directory
cleanup() {
  rm -rf "${temporary_directory}"
}
trap cleanup EXIT

# The injected UI target copies these native files beside the XCTest sources.
# Stage the same files so this preflight checks the exact source combination
# without generating a native project or touching the working tree.
readonly staged_source_directory="${temporary_directory}/meetermTests"
mkdir -p "${staged_source_directory}" "${temporary_directory}/module-cache"
cp "${native_input_source}" "${staged_source_directory}/TerminalInputView.swift"
cp "${native_key_source}" "${staged_source_directory}/TerminalSpecialKey.swift"

selected_developer_directory="${DEVELOPER_DIR:-$(xcode-select -p 2>/dev/null || true)}"
readonly selected_developer_directory
printf 'iOS Swift typecheck: Xcode=%s SDK=%s target=%s sources=4\n' \
  "${selected_developer_directory:-unavailable}" \
  "${sdk_version}" \
  "${swift_target}"

xcrun swiftc \
  -typecheck \
  -sdk "${sdk_path}" \
  -target "${swift_target}" \
  -swift-version 5 \
  -module-name meeterm_ios_preflight \
  -module-cache-path "${temporary_directory}/module-cache" \
  -F "${xctest_frameworks_path}" \
  "${driver_source}" \
  "${input_tests_source}" \
  "${staged_source_directory}/TerminalInputView.swift" \
  "${staged_source_directory}/TerminalSpecialKey.swift"

echo "iOS Swift typecheck passed."
