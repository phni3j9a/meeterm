#!/usr/bin/env bash
set -Eeuo pipefail

readonly project_path="${GITHUB_WORKSPACE}/ios/meeterm.xcodeproj/project.pbxproj"
readonly scheme_path="${GITHUB_WORKSPACE}/ios/meeterm.xcodeproj/xcshareddata/xcschemes/meeterm.xcscheme"
readonly source_path="${GITHUB_WORKSPACE}/scripts/ci/MeetermSmokeUITests.swift"

test -f "${project_path}"
test -f "${scheme_path}"
test -f "${source_path}"

python3 "${GITHUB_WORKSPACE}/scripts/ci/ios-inject-ui-test.py" \
  "${project_path}" \
  "${scheme_path}" \
  "${source_path}" \
  "${GITHUB_WORKSPACE}/scripts/ci/TerminalInputViewTests.swift" \
  "${GITHUB_WORKSPACE}/modules/meeterm-terminal/ios/TerminalInputView.swift" \
  "${GITHUB_WORKSPACE}/modules/meeterm-terminal/ios/TerminalSpecialKey.swift" \
  "${GITHUB_WORKSPACE}/scripts/ci/ClientStoreTests.swift"
