# meeterm-core

`meeterm-core` is the Rust-owned terminal state for the native Android and iOS
vertical slices. It intentionally contains no SSH, tmux, Tokio runtime,
UniFFI, or React Native code. JNI connects Android to the registry, while the
same narrow C ABI is linked as an iOS static library.

`alacritty_terminal::Term` owns the VT state. The crate exposes the JNI API used
by Android and the C ABI used by iOS; Rust tests use the safe API directly.
The built-in demo is fed from Rust so terminal output does not travel through
JavaScript.

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
the resulting commit count. In this milestone the bytes are deliberately
looped back into the same `Term` so the native IME path can be tested without
a remote process. An empty or invalid input is not counted. The commit count
is not part of the snapshot and is exposed only through the native API/test
surface.

`meeterm_send_special_key` accepts the enum values in `src/input.rs` and
encodes them explicitly as terminal bytes. It returns the encoded byte count;
negative values indicate an invalid ID or key.

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
