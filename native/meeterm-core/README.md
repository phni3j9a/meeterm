# meeterm-core

`meeterm-core` is the Rust-owned terminal state for the native Android and iOS
vertical slices. It includes a `russh` SSH connection/session layer and one
process-wide Tokio runtime. Its byte-oriented tmux Control Mode decoder routes
each remote pane to its own Rust terminal. It contains no React Native code. JNI
connects Android to the registry, while the same C ABI is linked as an iOS
static library.

`alacritty_terminal::Term` owns the VT state. The crate exposes the JNI API used
by Android and the C ABI used by iOS; Rust tests use the safe API directly.
The built-in demo and remote SSH output are fed from Rust, so terminal output
does not travel through JavaScript.

## Native API

All handles are opaque registry IDs. They are never pointers and must be
treated as invalid after `meeterm_destroy_terminal` returns successfully.

```text
uint64_t meeterm_create_terminal(uint16_t columns, uint16_t rows)
size_t   meeterm_snapshot_size(uint64_t id)
size_t   meeterm_snapshot(uint64_t id, uint8_t *out, size_t capacity)
int32_t  meeterm_resize_terminal(uint64_t id, uint16_t columns, uint16_t rows)
uint64_t meeterm_commit_utf8(uint64_t id, const uint8_t *bytes, size_t length)
int32_t  meeterm_send_special_key(uint64_t id, uint32_t key)
uint64_t meeterm_input_commit_count(uint64_t id)
int32_t  meeterm_destroy_terminal(uint64_t id)
```

`meeterm_create_terminal` creates a terminal and feeds the built-in fixed
ANSI/VT demo exactly once. A return value of `0` means failure. Registry IDs
start at `1`. The initial ABI accepts 2..4096 columns and 1..4096 rows; zero
and oversized dimensions are rejected before allocating a `Term`.

`meeterm_commit_utf8` validates UTF-8, records one native commit, and returns
the resulting commit count. Before connecting, bytes loop back into the demo
`Term`. Starting SSH clears the demo and disables loopback; accepted input is
encoded as tmux hexadecimal input targeting that pane. Input before readiness or after close is rejected.
An empty, invalid, or rejected input is not counted. The commit count
is not part of the snapshot and is exposed only through the native API/test
surface.

`meeterm_send_special_key` accepts the enum values in `src/input.rs` and
encodes them explicitly as terminal bytes. It returns the encoded byte count;
negative values indicate rejection. Arrow encoding respects application cursor
mode. Terminal-generated responses use the same native outbound path.

## SSH control and rendering

`meeterm_connect`, `meeterm_disconnect`, `meeterm_reconnect`,
`meeterm_select_pane`, `meeterm_session_panes`, `meeterm_connection_snapshot`,
`meeterm_respond_host_key`, and `meeterm_forget_host_key` expose the small control
surface. The C header in `modules/meeterm-terminal/ios/include/meeterm_core.h`
defines the exact ABI; connection snapshots carry fixed-size UTF-8 fields with
explicit lengths. They contain lifecycle state and host identity, never
terminal output or authentication material.

OpenSSH keys, optional passphrases, and SSH passwords are passed transiently.
The selected parsed credential remains in Rust process memory for an explicit
reconnect and is never persisted to disk. Password authentication uses only the
SSH `password` method; keyboard-interactive prompts and MFA are not implemented.
The extended connect ABI appends auth_method and password pointer/length pairs
after the existing known-hosts path; callers must rebuild all native adapters
together. The selector accepts publicKey and password (an empty selector keeps
the legacy public-key default), and rejects mixed credential fields. Password
whitespace is preserved exactly.
Rust owns host-key trust decisions and the app-private trust file supplied by
each platform. Unknown keys require explicit acceptance; changed or unreadable
trust state fails closed. See [SSH validation](../../docs/SSH.md) for the real
OpenSSH tests and current authentication/lifecycle limits.

`meeterm_terminal_revision` lets attached native views detect output changes
without transferring snapshots to JavaScript. Native views check revisions
approximately every 33 ms while visible and request a frame only on change;
this polling compromise is not a permanent high-frequency render loop. Hidden
views stop polling without destroying the SSH session or terminal ID.

## Issue #17 backend

The production crate implements the common `Workspace → TerminalGroup →
Terminal` model for `Backend::Tmux` and `Backend::Herdr`. Tmux keeps its
window/pane mapping with one virtual group per window. Herdr maps workspace,
tab, and pane from its public 0.9.0 protocol 22/schema 1 API. Both backends
reuse the SSH lifecycle, terminal registry, `alacritty_terminal::Term`, native
snapshot format, input/resize transport, and native visibility lifecycle.

`ConnectOptions` carries the selected backend and an optional runtime. A missing
backend in an old saved profile defaults to tmux; an empty Herdr runtime means
`default`. The control bridge exposes the backend-independent workspace snapshot
and group operations (`create_group`, `rename_group`, `close_group`,
`select_group`) as well as `set_terminal_visible`. On Herdr, `close_group`
maps to `tab.close`; `close_workspace` maps to `workspace.close` with
`close_group: false`, so tab deletion and workspace cascade remain distinct.
The JSON snapshot is bounded and opaque to the terminal data plane; terminal
bytes, cells, scrollback, and render frames stay in Rust/native code.

Herdr control uses direct SSH stream-local requests, stable remote
`terminal_id` values, and mutable `pane_id` aliases. Display frames are ANSI
`terminal.frame` data. Semantic text, key, and paste input uses the corresponding
public `pane.send_text`, `pane.send_keys`, and `pane.send_input` operations;
`pane.scroll` is sent with offset zero before input. Home/End/Insert/Delete/
PageUp/PageDown use fixed xterm normal-mode bytes because the 0.9.0 parser does
not expose those names. Controller release drains closed/EOF and reacquires the
same remote process by stable ID.

The live integration test is intentionally ignored because it needs a real
Herdr 0.9.0 binary:

```sh
MEETERM_HERDR_INTEGRATION=1 \
MEETERM_HERDR_BINARY=/path/to/herdr-0.9.0 \
cargo test --locked --manifest-path native/meeterm-core/Cargo.toml \
  --test herdr -- --ignored --exact real_herdr_native_backend_over_russh_fixture
```

`tests/herdr.rs` starts an isolated XDG Herdr driver and a test-only `russh`
SSH endpoint implementing the production direct stream-local operations. It is
separate from the existing OpenSSH/tmux fixture; the older
`herdr_probe_tests::replay_live_frames` test and
`scripts/herdr/feasibility.py` remain historical diagnostics. The general CI
job downloads the official binary only into `RUNNER_TEMP` and verifies its
pinned digest before running this test. The new live run is still pending, so
this README does not mark Issue #17 accepted.

## Snapshot format

Snapshots are native-only Rust-to-Kotlin bytes. They must never be forwarded
to JavaScript. All multi-byte integers are little-endian.

```text
Header (28 bytes)
  0..4   magic             ASCII `MTRM`
  4..6   version           u16 = 1
  6..8   header_size       u16 = 28
  8..12  columns           u32
 12..16  rows              u32
 16..20  cursor_row        u32 (viewport-relative)
 20..24  cursor_column     u32
 24..28  cell_count        u32

Cell record (28-byte metadata followed by two UTF-8 payloads)
  0..4   row               u32 (viewport-relative)
  4..8   column            u32
  8      width             u8 (1 or 2)
  9      reserved          u8 = 0
 10..12  flags             u16 (alacritty cell flags)
 12..16  foreground RGBA   four u8 values
 16..20  background RGBA   four u8 values
 20..24  base_len          u32 byte length
 24..28  combining_len     u32 byte length
 28..    base UTF-8 bytes, followed by combining UTF-8 bytes
```

One record is emitted for each visible non-spacer cell, including blank cells
whose colors or flags matter. `WIDE_CHAR_SPACER` cells are omitted; the
leading wide cell retains `width = 2`. The base payload contains the cell's
base character (normally one UTF-8 scalar), and the combining payload contains
all zero-width characters attached to that cell. Invalid or impossible data
is rejected before a snapshot is returned.

The current snapshot color conversion uses the deterministic ANSI/xterm
palette for named and indexed colors. A future renderer may use the same
cell records directly, but this crate does not render pixels.

## iOS static library

The crate emits `libmeeterm_core.a` for all currently supported iOS slices:

- `aarch64-apple-ios` for physical devices;
- `aarch64-apple-ios-sim` for Apple Silicon simulators;
- `x86_64-apple-ios` for Intel simulators.

`modules/meeterm-terminal/MeetermTerminal.podspec` invokes
`ios/build-rust.sh` as a CocoaPods build phase. The script derives the Rust
target from Xcode's `PLATFORM_NAME` and `ARCHS`, uses the lockfile, and places
the target-specific archive in `BUILT_PRODUCTS_DIR`. Rust targets are pinned
in `rust-toolchain.toml`; Xcode and the matching iOS SDK are still required to
link the archive.

## Checks

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo check --locked --target aarch64-apple-ios
cargo check --locked --target aarch64-apple-ios-sim
cargo check --locked --target x86_64-apple-ios
```
