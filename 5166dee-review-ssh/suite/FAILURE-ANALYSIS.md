# SSH suite failure — 5166dee (xcuitest_ssh: ui_test_failed)

- Assert: `MeetermSmokeUITests.swift:3304` — `XCTFail("The short field did not retain the expected input after one retry.")` in `testShortSshInputAndDisconnect`, elapsed 133.7s, xcodebuild elapsed 220s (well inside 900s budget; NOT a timeout).
- Failing leg: `fill_username` — tap → clear → clear_verified → type → readback.
- `ios-ui-short-field-diagnostics.txt`: attempt 0 readback `value_length=1` vs `expected_length=5` (observed value IS an expected prefix, case-insensitive mismatch=0), keyboard present/hittable; retry_prefix_mismatch attempt 1 → XCTFail.
- Interpretation: the username UITextField retained only 1 of 5 typed characters across both attempts. Partial keystroke delivery under the measured CPU oversubscription (load ~78 on 12 cores, two booted simulators, dual SpringBoard/diagnosticd, concurrent rustc/Spotlight) is a plausible contention flake — inference, not proof. The assertion itself is unchanged and the failure is recorded as real.
- Fixture preflight PASSED (sshd :59956, tmux, real_openssh_existing_tmux_runtime_selection ok). No Herdr involvement; this suite uses tmux only.
- Diagnostics: runner_launch/connection/initialization/bundle-load/simulator-boot/device-prep all 0; disk_full=0; testing_cancelled=0; xcodebuild_result_line=failed; forced_exit_after_result=1.
- Only 1 PNG (ssh-entry-initial) — no theme/loss legs were reached. No raw xcresult uploaded.
