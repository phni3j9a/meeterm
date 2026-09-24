#!/usr/bin/env bash
# Issue #27 switcher drive: starts two disposable OpenSSH fixtures, creates the
# tmux topology, installs the built app, injects the fixture contract into a
# copied xctestrun, and runs the drive test only.
set -Eeuo pipefail

WORKSPACE="${GITHUB_WORKSPACE:-/Users/devin/repos/meeterm}"
RT="${RUNNER_TEMP:-/Users/devin/runner-temp}"
UDID="${IOS_SIMULATOR_UDID:?}"
ARTIFACT_DIR="${1:-${WORKSPACE}/artifacts/ios-simulator-observability-switcher}"
STAGE_PATH="${ARTIFACT_DIR}/ios-switcher-stages.txt"
WORKDIR="${RT}/switcher-drive-work"
DERIVED="${RT}/meeterm-derived-data"
mkdir -p "${ARTIFACT_DIR}" "${WORKDIR}"

f1_env="${RT}/meeterm-ssh.env"
f2_env="${RT}/meeterm-ssh2.env"
f1_pid=""
f2_pid=""
cleanup() {
  [[ -n "${f1_pid}" ]] && kill "${f1_pid}" 2>/dev/null || true
  [[ -n "${f2_pid}" ]] && kill "${f2_pid}" 2>/dev/null || true
  [[ -n "${f1_pid}" ]] && wait "${f1_pid}" 2>/dev/null || true
  [[ -n "${f2_pid}" ]] && wait "${f2_pid}" 2>/dev/null || true
}
trap cleanup EXIT

rm -f "${f1_env}" "${f2_env}"
python3 "${WORKSPACE}/scripts/ssh/fixture.py" --env-file "${f1_env}" & f1_pid=$!
python3 "${WORKSPACE}/scripts/ssh/fixture.py" --env-file "${f2_env}" & f2_pid=$!
deadline=$((SECONDS + 60))
while [[ ! -s "${f1_env}" || ! -s "${f2_env}" ]]; do
  if ! kill -0 "${f1_pid}" 2>/dev/null || ! kill -0 "${f2_pid}" 2>/dev/null; then
    echo "fixture exited before becoming ready" >&2; exit 1
  fi
  (( SECONDS >= deadline )) && { echo "fixture ready timeout" >&2; exit 1; }
  sleep 0.2
done

# Fixture 1 contract (sourced for the primary environment + tmux socket).
source "${f1_env}"
S1_HOST="${MEETERM_SSH_HOST}"; S1_PORT="${MEETERM_SSH_PORT}"; S1_USER="${MEETERM_SSH_USERNAME}"
S1_FP="${MEETERM_SSH_FINGERPRINT}"; S1_KEY="${MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE}"
S1_SOCKET="${MEETERM_TMUX_SOCKET}"; S1_TMPDIR="${MEETERM_TMUX_TMPDIR}"
source "${f2_env}"
S2_HOST="${MEETERM_SSH_HOST}"; S2_PORT="${MEETERM_SSH_PORT}"; S2_USER="${MEETERM_SSH_USERNAME}"
S2_FP="${MEETERM_SSH_FINGERPRINT}"; S2_KEY="${MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE}"
S2_SOCKET="${MEETERM_TMUX_SOCKET}"; S2_TMPDIR="${MEETERM_TMUX_TMPDIR}"

tmx() { # tmx <tmpdir> <socket> <args...>
  local tmpdir="$1"; local socket="$2"; shift 2
  env -u TMUX -u TMUX_PANE TMUX_TMPDIR="${tmpdir}" TERM=xterm-256color \
    /opt/homebrew/bin/tmux -f /dev/null -S "${socket}" "$@"
}
# Fixture1 topology: session `meeterm` (window `main`), session `alt` (window `alt-main`).
tmx "${S1_TMPDIR}" "${S1_SOCKET}" new-session -d -s meeterm -n main /bin/sh -i
tmx "${S1_TMPDIR}" "${S1_SOCKET}" new-session -d -s alt -n alt-main /bin/sh -i
# Fixture2 topology: session `meeterm-two` (window `two-main`).
tmx "${S2_TMPDIR}" "${S2_SOCKET}" new-session -d -s meeterm-two -n two-main /bin/sh -i
tmx "${S1_TMPDIR}" "${S1_SOCKET}" list-sessions -F '#{session_name}'
tmx "${S2_TMPDIR}" "${S2_SOCKET}" list-sessions -F '#{session_name}'

app_path="${DERIVED}/Build/Products/Release-iphonesimulator/meeterm.app"
xcrun simctl uninstall "${UDID}" dev.meeterm.app 2>/dev/null || true
xcrun simctl install "${UDID}" "${app_path}"

xctestrun_src=$(ls "${DERIVED}"/Build/Products/*.xctestrun | head -1)
xctestrun="${DERIVED}/Build/Products/meeterm-switcher-drive.xctestrun"
cp "${xctestrun_src}" "${xctestrun}"

S1_HOST="${S1_HOST}" S1_PORT="${S1_PORT}" S1_USER="${S1_USER}" S1_FP="${S1_FP}" S1_KEY="${S1_KEY}" \
S2_HOST="${S2_HOST}" S2_PORT="${S2_PORT}" S2_USER="${S2_USER}" S2_FP="${S2_FP}" S2_KEY="${S2_KEY}" \
S1_SOCKET="${S1_SOCKET}" S2_SOCKET="${S2_SOCKET}" \
ARTIFACT_DIR="${ARTIFACT_DIR}" STAGE_PATH="${STAGE_PATH}" WORKDIR="${WORKDIR}" XCTESTRUN="${xctestrun}" \
python3 - <<'PY'
import os, plistlib
path = os.environ["XCTESTRUN"]
with open(path, "rb") as stream:
    doc = plistlib.load(stream)
env = {
    "MEETERM_SSH_HOST": os.environ["S1_HOST"],
    "MEETERM_SSH_PORT": os.environ["S1_PORT"],
    "MEETERM_SSH_USERNAME": os.environ["S1_USER"],
    "MEETERM_SSH_FINGERPRINT": os.environ["S1_FP"],
    "MEETERM_SSH_UNENCRYPTED_PRIVATE_KEY_FILE": os.environ["S1_KEY"],
    "MEETERM_SSH2_HOST": os.environ["S2_HOST"],
    "MEETERM_SSH2_PORT": os.environ["S2_PORT"],
    "MEETERM_SSH2_USERNAME": os.environ["S2_USER"],
    "MEETERM_SSH2_FINGERPRINT": os.environ["S2_FP"],
    "MEETERM_SSH2_UNENCRYPTED_PRIVATE_KEY_FILE": os.environ["S2_KEY"],
    "MEETERM_SWITCH_SOCKET1": os.environ["S1_SOCKET"],
    "MEETERM_SWITCH_SOCKET2": os.environ["S2_SOCKET"],
    "MEETERM_SWITCH_WORKDIR": os.environ["WORKDIR"],
    "MEETERM_IOS_ARTIFACT_DIR": os.environ["ARTIFACT_DIR"],
    "MEETERM_IOS_STAGE_PATH": os.environ["STAGE_PATH"],
    "MEETERM_IOS_MARKER_PATH": os.environ["WORKDIR"] + "/ios-marker.txt",
    "MEETERM_IOS_MARKER_VALUE": "ios-switcher-marker-0001",
    "MEETERM_IOS_HANDOFF_VALUE": "ios-switcher-handoff-0001",
    "MEETERM_IOS_TRANSPORT_LOSS_MARKER_PATH": os.environ["WORKDIR"] + "/ios-tl-marker.txt",
    "MEETERM_IOS_TRANSPORT_LOSS_PRE_VALUE": "ios-switcher-tl-pre-0001",
    "MEETERM_IOS_TRANSPORT_LOSS_POST_VALUE": "ios-switcher-tl-post-0001",
}
targets = []
def visit(value):
    if isinstance(value, dict):
        if "TestBundlePath" in value or "UITargetAppPath" in value:
            targets.append(value)
        for child in value.values():
            visit(child)
    elif isinstance(value, list):
        for child in value:
            visit(child)
visit(doc)
assert targets, "no test targets"
for target in targets:
    for key in ("TestingEnvironmentVariables", "EnvironmentVariables"):
        existing = target.get(key)
        if not isinstance(existing, dict):
            existing = {}
        existing.update(env)
        target[key] = existing
with open(path, "wb") as stream:
    plistlib.dump(doc, stream, sort_keys=False)
print("injected", len(env), "env vars into", len(targets), "targets")
PY

raw_log="${RT}/meeterm-ios-switcher-drive-xcodebuild.log"
rm -f "${STAGE_PATH}"
rm -rf "${RT}/meeterm-ios-switcher-drive.xcresult"
set +e
xcodebuild test-without-building \
  -xctestrun "${xctestrun}" \
  -destination "platform=iOS Simulator,id=${UDID}" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
  -only-testing:meetermTests/MeetermSmokeUITests/testIssue27SessionSwitcherDrive \
  -resultBundlePath "${RT}/meeterm-ios-switcher-drive.xcresult" 2>&1 | tee "${raw_log}" | tail -30
status=${PIPESTATUS[0]}
set -e

# Host-side remote proof: record the surviving tmux sessions on both fixture
# sockets before teardown (the UI test cannot exec on the host).
{
  echo "socket1_sessions=$(tmx "${S1_TMPDIR}" "${S1_SOCKET}" list-sessions -F '#{session_name}' 2>/dev/null | tr '\n' ',' | sed 's/,$//')"
  echo "socket2_sessions=$(tmx "${S2_TMPDIR}" "${S2_SOCKET}" list-sessions -F '#{session_name}' 2>/dev/null | tr '\n' ',' | sed 's/,$//')"
} > "${ARTIFACT_DIR}/ios-issue27-remote-sessions.txt"
cat "${ARTIFACT_DIR}/ios-issue27-remote-sessions.txt"
exit "${status}"
