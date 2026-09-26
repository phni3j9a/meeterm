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
readonly attachment_tests_source="${repository_root}/scripts/ci/AttachmentTests.swift"
readonly native_input_source="${repository_root}/modules/meeterm-terminal/ios/TerminalInputView.swift"
readonly native_key_source="${repository_root}/modules/meeterm-terminal/ios/TerminalSpecialKey.swift"
readonly native_scroll_source="${repository_root}/modules/meeterm-terminal/ios/TerminalScrollGestureDelegate.swift"
readonly attachment_limits_source="${repository_root}/modules/meeterm-terminal/ios/AttachmentLimits.swift"
readonly attachment_session_source="${repository_root}/modules/meeterm-terminal/ios/AttachmentSession.swift"
readonly attachment_policy_source="${repository_root}/modules/meeterm-terminal/ios/AttachmentInsertionPolicy.swift"
readonly attachment_guard_source="${repository_root}/modules/meeterm-terminal/ios/AttachmentCompositionGuard.swift"
readonly attachment_operation_source="${repository_root}/modules/meeterm-terminal/ios/AttachmentOperation.swift"
# Production storage tests are intentionally outside this preflight: their
# @testable import requires the built production app module and app host.

for source in \
  "${driver_source}" \
  "${input_tests_source}" \
  "${attachment_tests_source}" \
  "${native_input_source}" \
  "${native_key_source}" \
  "${native_scroll_source}" \
  "${attachment_limits_source}" \
  "${attachment_session_source}" \
  "${attachment_policy_source}" \
  "${attachment_guard_source}" \
  "${attachment_operation_source}"; do
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
readonly xctest_swift_import_path="${platform_path}/Developer/usr/lib"

if [[ ! -d "${sdk_path}" ]]; then
  echo "iOS Simulator SDK was not found: ${sdk_path}" >&2
  exit 1
fi
if [[ ! -d "${xctest_frameworks_path}/XCTest.framework" ]]; then
  echo "XCTest.framework was not found: ${xctest_frameworks_path}" >&2
  exit 1
fi
if [[ ! -d "${xctest_swift_import_path}" ]]; then
  echo "XCTest Swift import path was not found: ${xctest_swift_import_path}" >&2
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
cp "${native_scroll_source}" "${staged_source_directory}/TerminalScrollGestureDelegate.swift"
cp "${attachment_limits_source}" "${staged_source_directory}/AttachmentLimits.swift"
cp "${attachment_session_source}" "${staged_source_directory}/AttachmentSession.swift"
cp "${attachment_policy_source}" "${staged_source_directory}/AttachmentInsertionPolicy.swift"
cp "${attachment_guard_source}" "${staged_source_directory}/AttachmentCompositionGuard.swift"
cp "${attachment_operation_source}" "${staged_source_directory}/AttachmentOperation.swift"

selected_developer_directory="${DEVELOPER_DIR:-$(xcode-select -p 2>/dev/null || true)}"
readonly selected_developer_directory
printf 'iOS Swift typecheck: Xcode=%s SDK=%s target=%s sources=10\n' \
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
  -Isystem "${xctest_swift_import_path}" \
  -F "${xctest_frameworks_path}" \
  "${driver_source}" \
  "${input_tests_source}" \
  "${attachment_tests_source}" \
  "${staged_source_directory}/TerminalInputView.swift" \
  "${staged_source_directory}/TerminalSpecialKey.swift" \
  "${staged_source_directory}/TerminalScrollGestureDelegate.swift" \
  "${staged_source_directory}/AttachmentLimits.swift" \
  "${staged_source_directory}/AttachmentSession.swift" \
  "${staged_source_directory}/AttachmentInsertionPolicy.swift" \
  "${staged_source_directory}/AttachmentCompositionGuard.swift" \
  "${staged_source_directory}/AttachmentOperation.swift"

echo "iOS Swift typecheck passed."

# Execute the production clipboard reader against a fault-injected C boundary.
# This catches a pane disappearing between its length query and byte copy.
MEETERM_SWIFTC="$(xcrun --find swiftc)" python3 "${script_directory}/test_ios_selection_text.py"
