//! Rust-owned terminal state for meeterm's Android and iOS vertical slices.

mod dimensions;
mod ffi;
mod input;
#[cfg(target_os = "android")]
mod jni;
mod registry;
mod snapshot;
mod ssh;
mod terminal;

pub use ffi::{
    meeterm_commit_utf8, meeterm_connect, meeterm_connection_snapshot,
    meeterm_connection_snapshot_size, meeterm_create_terminal, meeterm_destroy_terminal,
    meeterm_disconnect, meeterm_forget_host_key, meeterm_input_commit_count,
    meeterm_resize_terminal, meeterm_respond_host_key, meeterm_send_bytes,
    meeterm_send_special_key, meeterm_snapshot, meeterm_snapshot_size, meeterm_terminal_revision,
};
pub use input::{SpecialKey, encode_special_key};
pub use registry::{create_terminal, destroy_terminal, terminal_count};
pub use snapshot::Snapshot;
pub use ssh::{
    ALGORITHM_CAPACITY, ConnectOptions, ConnectionError, ConnectionSnapshot, ConnectionState,
    ERROR_CODE_CAPACITY, ERROR_MESSAGE_CAPACITY, FINGERPRINT_CAPACITY, HOST_CAPACITY,
    connect_terminal, connection_snapshot, disconnect_terminal, forget_host_key,
    respond_to_host_key, send_bytes, terminal_revision,
};
pub use terminal::{FIXED_DEMO_BYTES, Terminal, TerminalError};

#[cfg(test)]
mod tests;
