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
mod tmux;

pub use ffi::{
    meeterm_clear_selection, meeterm_commit_modified_utf8, meeterm_commit_utf8, meeterm_connect,
    meeterm_connection_snapshot, meeterm_connection_snapshot_size, meeterm_create_terminal,
    meeterm_destroy_terminal, meeterm_disconnect, meeterm_forget_host_key,
    meeterm_input_commit_count, meeterm_pane_record_size, meeterm_paste_utf8, meeterm_reconnect,
    meeterm_resize_terminal, meeterm_respond_host_key, meeterm_scroll_lines, meeterm_select_pane,
    meeterm_select_start, meeterm_select_update, meeterm_selection_text, meeterm_send_bytes,
    meeterm_send_key, meeterm_send_special_key, meeterm_session_panes,
    meeterm_set_automatic_reconnect, meeterm_set_foreground, meeterm_set_scrollback_limit,
    meeterm_set_theme, meeterm_snapshot, meeterm_snapshot_size, meeterm_terminal_exists,
    meeterm_terminal_revision, meeterm_tmux_command,
};
pub use input::{
    KeyCode, Modifiers, SpecialKey, encode_key, encode_key_for_mode, encode_special_key,
    encode_text,
};
pub use registry::{
    clear_selection, commit_modified_utf8, create_terminal, destroy_terminal, scrollback_lines,
    select_start, select_update, selection_text, send_key, set_scrollback_limit, set_theme,
    terminal_count,
};
pub use snapshot::{Snapshot, Theme};
pub use ssh::{
    ALGORITHM_CAPACITY, AuthOptions, ConnectOptions, ConnectionError, ConnectionSnapshot,
    ConnectionState, ERROR_CODE_CAPACITY, ERROR_MESSAGE_CAPACITY, FINGERPRINT_CAPACITY,
    HOST_CAPACITY, close_pane, close_workspace, connect_terminal, connection_snapshot, create_pane,
    create_workspace, disconnect_terminal, forget_host_key, reconnect_terminal, refresh_terminal,
    rename_pane, rename_workspace, select_pane, send_bytes, session_snapshot,
    set_automatic_reconnect, set_foreground, terminal_revision,
};
pub use terminal::{
    DEFAULT_SCROLLBACK_LINES, FIXED_DEMO_BYTES, MAX_SCROLLBACK_LINES, MIN_SCROLLBACK_LINES,
    Terminal, TerminalError,
};
pub use tmux::{PaneSnapshot, SESSION_NAME, SessionSnapshot, WindowSnapshot};

#[cfg(test)]
mod tests;
