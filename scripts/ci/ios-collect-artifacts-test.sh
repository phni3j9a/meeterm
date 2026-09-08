#!/usr/bin/env bash
set -Eeuo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
readonly repository_root
temporary_root="$(mktemp -d "${TMPDIR:-/tmp}/meeterm-ios-collector-test.XXXXXX")"
readonly temporary_root
readonly workspace_root="${temporary_root}/workspace"
readonly artifact_root="${workspace_root}/artifacts/ios-simulator-observability"
readonly fake_bin="${temporary_root}/bin"
readonly xcrun_log="${temporary_root}/xcrun.log"

cleanup() {
  rm -rf "${temporary_root}"
}
trap cleanup EXIT

mkdir -p "${workspace_root}/scripts/ci" "${fake_bin}" "${artifact_root}"
cp "${repository_root}/scripts/ci/ios-collect-artifacts.sh" "${workspace_root}/scripts/ci/"
cp "${repository_root}/scripts/ci/validate-png.sh" "${workspace_root}/scripts/ci/"

cat > "${fake_bin}/xcrun" <<'EOF'
#!/usr/bin/env bash
set -u
printf '%s\n' "$*" >> "${IOS_COLLECT_TEST_XCRUN_LOG}"
if [[ "${1:-}" == "simctl" && "${2:-}" == "io" ]]; then
  output_path="${!#}"
  : > "${output_path}"
fi
exit 0
EOF
chmod +x "${fake_bin}/xcrun"

run_collector() {
  (
    cd "${workspace_root}"
    GITHUB_WORKSPACE="${workspace_root}" \
      IOS_SIMULATOR_UDID="SIMULATOR" \
      IOS_COLLECT_TEST_XCRUN_LOG="${xcrun_log}" \
      PATH="${fake_bin}:${PATH}" \
      scripts/ci/ios-collect-artifacts.sh
  )
}

: > "${xcrun_log}"
printf '%s\n' 'mode=xcuitest-real-ssh' > "${artifact_root}/launch.txt"
run_collector
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "collector captured a terminal screenshot before the safe post-test marker" >&2
  exit 1
fi
test ! -e "${artifact_root}/terminal.png"
grep -Fq 'XCTest did not capture the fresh native foundation' \
  "${artifact_root}/screenshot-unavailable.txt"

# A safe XCTest checkpoint is preserved, and stale unavailable diagnostics are
# cleared. The collector must never take another screenshot of arbitrary UI.
python3 - "${artifact_root}/terminal.png" <<'PY'
import base64
from pathlib import Path
import sys
Path(sys.argv[1]).write_bytes(base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aY4sAAAAASUVORK5CYII="
))
PY
cp "${artifact_root}/terminal.png" "${temporary_root}/expected.png"
: > "${xcrun_log}"
run_collector
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "collector recaptured arbitrary UI over the XCTest checkpoint" >&2
  exit 1
fi
cmp "${artifact_root}/terminal.png" "${temporary_root}/expected.png"
test ! -e "${artifact_root}/screenshot-unavailable.txt"

# Invalid images remain explicit diagnostics, without making the collector a
# screenshot-existence gate or attempting to capture a credentials screen.
: > "${artifact_root}/terminal.png"
run_collector
test ! -e "${artifact_root}/terminal.png"
grep -Fq 'screenshot capture produced no image data' \
  "${artifact_root}/screenshot-unavailable.txt"

echo "iOS artifact screenshot boundary regression passed."
