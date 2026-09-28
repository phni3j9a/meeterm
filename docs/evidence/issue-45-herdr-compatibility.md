# Issue #45: Herdr compatibility contract

The production contract is protocol 22, API schema 1, the JSON methods used by
meeterm, and the list/status/direct-control CLI. SemVer and absolute executable
path equality are not connection gates. Host authentication, selected runtime,
stable terminal identity, ordinary controller acquisition without takeover, and
the first authoritative full frame remain required for retained recovery.

The bundled schema advertises JSON methods. Read-only CLI help probes cover the
list/status/control entry points and controller target/size options. Actual
list/status, direct stream-local subscription/snapshot, lease and frame handling
are verified when used. Input/resize/scroll/release are protocol 22 semantics;
discovery never sends these mutations merely to probe support. Unknown additive
schema fields/methods are allowed; required method omissions are rejected.

## Reproducible fixtures

| Verified fixture | Official Linux x86_64 SHA-256 | Role |
| --- | --- | --- |
| Herdr 0.9.0 | `4fa1a01158dd8043da92d31b270780b0dcc10603038d9b61cac4d81ab63fb71f` | Existing full native integration baseline |
| Herdr 0.9.1 | `2a02fed16beb651ef006e1d43f048f652ca4dc58ad053cd2d44450563d5c54b7` | Additional compatibility/recovery CI fixture |

These are tested fixtures, not a production version allowlist or a minimum
supported release. The tests use isolated Herdr processes and a test-only russh
endpoint; they do not modify user Herdr installations or sessions.

The new live regression advertises incompatible protocol/schema/missing-method
candidates before the compatible binary, changes the endpoint's resolved CLI
alias during transport loss, verifies the same native terminal and input after
recovery, then advertises only an incompatible replacement and requires a
stopped read-only state. Alias and negative-schema substitutions are explicit
test endpoint behavior; the actual runtime, controller, frames and input use
the official binary.

## Validation record

Local development checks on 2026-09-28:

- Rust: 238 unit tests, 4 Herdr fixture/parser tests, 1 tmux layout test passed;
  opt-in live tests are reported separately. `cargo fmt --check` and Clippy
  with `-D warnings` passed.
- TypeScript typecheck and App tests: 130 passed.
- Herdr Python driver regression: 6 passed.
- SHA-verified Herdr 0.9.0: all 4 ignored native integration cases passed
  (21.86 seconds), including the new compatibility/recovery case.
- SHA-verified Herdr 0.9.1: all 4 ignored native integration cases passed
  (22.06 seconds). Real OpenSSH/tmux session integration passed (27.31 seconds).

Initial GitHub CI on `690b09b2` exposed a race in the new standalone test:
[push run 36438261747](https://github.com/phni3j9a/meeterm/actions/runs/36438261747)
failed its 0.9.0 compatibility marker, while
[PR run 36438394226](https://github.com/phni3j9a/meeterm/actions/runs/36438394226)
passed all 0.9.0 cases and failed the 0.9.1 compatibility marker. The test called
`set_terminal_visible(true)` and immediately sent input while that accepted
visibility transition temporarily closed the actor gate. It now waits for the
existing `control.terminalInputReady` acknowledgment before the first marker.
The input/identity/recovery assertions and deadlines are unchanged. This fixes
only the Rust integration test, not product or mobile-suite code.

GitHub CI after that correction and exact product-source `690b09b2` Android full /
iOS standard / iOS ssh acceptance are pending. No mobile or visual success is claimed by these local results.
Mobile SSH suites exercise tmux; seeded Herdr screens only verify presentation.
Physical-device GPU, fonts and Japanese IME parity remain outside this change's
emulator/Simulator acceptance scope. Prior Issue #17 evidence remains unchanged.
