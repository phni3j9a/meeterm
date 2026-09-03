//! Rust-owned terminal state for meeterm's Android and iOS vertical slices.

mod dimensions;
mod ffi;
mod input;
#[cfg(target_os = "android")]
mod jni;
mod registry;
mod snapshot;
mod terminal;

pub use ffi::{
    meeterm_commit_utf8, meeterm_create_terminal, meeterm_destroy_terminal,
    meeterm_input_commit_count, meeterm_resize_terminal, meeterm_send_special_key,
    meeterm_snapshot, meeterm_snapshot_size,
};
pub use input::{SpecialKey, encode_special_key};
pub use registry::{create_terminal, destroy_terminal, terminal_count};
pub use snapshot::Snapshot;
pub use terminal::{FIXED_DEMO_BYTES, Terminal, TerminalError};

#[cfg(test)]
mod tests;
