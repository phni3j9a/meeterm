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
      DEVELOPER_DIR="/fixture/SelectedXcode/Contents/Developer" \
      MEETERM_IOS_SUITE="${1:-standard}" \
      IOS_COLLECT_TEST_XCRUN_LOG="${xcrun_log}" \
      PATH="${fake_bin}:${PATH}" \
      scripts/ci/ios-collect-artifacts.sh
  )
}

: > "${xcrun_log}"
printf '%s\n' 'mode=xcuitest-real-ssh' > "${artifact_root}/launch.txt"
run_collector full
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "collector captured a terminal screenshot before the safe post-test marker" >&2
  exit 1
fi
test ! -e "${artifact_root}/terminal.png"
grep -Fxq 'xcode_developer_dir=/fixture/SelectedXcode/Contents/Developer' "${artifact_root}/metadata.txt"
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
run_collector full
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "collector recaptured arbitrary UI over the XCTest checkpoint" >&2
  exit 1
fi
cmp "${artifact_root}/terminal.png" "${temporary_root}/expected.png"
test ! -e "${artifact_root}/screenshot-unavailable.txt"

# Invalid images remain explicit diagnostics, without making the collector a
# screenshot-existence gate or attempting to capture a credentials screen.
: > "${artifact_root}/terminal.png"
run_collector full
test ! -e "${artifact_root}/terminal.png"
grep -Fq 'screenshot capture produced no image data' \
  "${artifact_root}/screenshot-unavailable.txt"

# Standard keeps its ten direct seeded-screen checkpoints as evidence and
# never captures a replacement screenshot from the collector.
for checkpoint in \
  standard-home standard-servers standard-connection standard-password \
  standard-workspaces standard-terminal standard-settings \
  standard-workspace-name standard-terminal-name standard-handoff; do
  cp "${temporary_root}/expected.png" "${artifact_root}/${checkpoint}.png"
done
: > "${xcrun_log}"
run_collector standard
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "standard collector captured arbitrary UI over XCTest checkpoints" >&2
  exit 1
fi
grep -Fxq 'suite=standard' "${artifact_root}/screenshot-scope.txt"
test ! -e "${artifact_root}/ui-screenshots-unavailable.txt"
grep -Fq 'XCTest did not capture the fresh native foundation' \
  "${artifact_root}/screenshot-unavailable.txt"
cmp "${artifact_root}/standard-terminal-name.png" "${temporary_root}/expected.png"
: > "${artifact_root}/standard-handoff.png"
run_collector standard
grep -Fq 'standard-handoff' "${artifact_root}/ui-screenshots-unavailable.txt"
if grep -Eq 'host-trust|reconnected|forms-controls' "${artifact_root}/ui-screenshots-unavailable.txt"; then
  echo "standard evidence incorrectly requires old full or form checkpoints" >&2
  exit 1
fi

# The short SSH suite may preserve safe post-auth terminal checkpoints. It does
# not manufacture screenshots, and missing evidence remains a diagnostic.
for checkpoint in ssh-terminal-input ssh-disconnected; do
  cp "${temporary_root}/expected.png" "${artifact_root}/${checkpoint}.png"
done
: > "${xcrun_log}"
run_collector ssh
grep -Fxq 'suite=ssh' "${artifact_root}/screenshot-scope.txt"
grep -Fq 'suite=ssh; safe post-auth checkpoint screenshots are optional' \
  "${artifact_root}/screenshot-unavailable.txt"
test ! -e "${artifact_root}/ui-screenshots-unavailable.txt"
cmp "${artifact_root}/ssh-terminal-input.png" "${temporary_root}/expected.png"
: > "${artifact_root}/ssh-disconnected.png"
run_collector ssh
grep -Fq 'ssh-disconnected' "${artifact_root}/ui-screenshots-unavailable.txt"
if grep -Eq 'host-trust|reconnected|forms-controls' "${artifact_root}/ui-screenshots-unavailable.txt"; then
  echo "SSH evidence incorrectly requires old full or form checkpoints" >&2
  exit 1
fi
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "SSH collector captured arbitrary UI" >&2
  exit 1
fi

# Focused success never demands foundation/full-flow screenshots. Preserve
# only its own safe images and report a missing focused checkpoint precisely.
for checkpoint in forms-keyboard password-form-keyboard forms-controls; do
  cp "${temporary_root}/expected.png" "${artifact_root}/${checkpoint}.png"
done
run_collector forms
grep -Fq 'suite=forms; native foundation screenshot not requested' "${artifact_root}/screenshot-unavailable.txt"
test ! -e "${artifact_root}/ui-screenshots-unavailable.txt"
cmp "${artifact_root}/forms-controls.png" "${temporary_root}/expected.png"
: > "${artifact_root}/forms-controls.png"
run_collector forms
grep -Fq 'forms-controls' "${artifact_root}/ui-screenshots-unavailable.txt"
if grep -Eq 'host-trust|reconnected' "${artifact_root}/ui-screenshots-unavailable.txt"; then
  echo "focused form evidence incorrectly requires full-flow checkpoints" >&2
  exit 1
fi
run_collector native
grep -Fq 'suite=native; UI screenshots not requested' "${artifact_root}/screenshot-unavailable.txt"
test ! -e "${artifact_root}/ui-screenshots-unavailable.txt"
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "focused collector captured arbitrary UI" >&2
  exit 1
fi

run_collector names
grep -Fq 'suite=names' "${artifact_root}/ui-screenshots-unavailable.txt"
grep -Fq 'daily-created-pane' "${artifact_root}/ui-screenshots-unavailable.txt"
for checkpoint in connection-form-keyboard host-trust workspaces daily-workspace-create-form daily-created-pane; do
  cp "${temporary_root}/expected.png" "${artifact_root}/${checkpoint}.png"
done
run_collector names
test ! -e "${artifact_root}/ui-screenshots-unavailable.txt"
grep -Fq 'suite=names; native foundation screenshot not requested' "${artifact_root}/screenshot-unavailable.txt"
if grep -Fq 'simctl io' "${xcrun_log}"; then
  echo "name-operation collector captured arbitrary UI" >&2
  exit 1
fi

echo "iOS artifact screenshot boundary and focused scope regressions passed."
