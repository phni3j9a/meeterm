//! Rust-owned terminal state for meeterm's Android and iOS vertical slices.

mod attachment;
mod dimensions;
mod ffi;
mod herdr;
mod input;
#[cfg(target_os = "android")]
mod jni;
mod registry;
mod snapshot;
mod ssh;
mod terminal;
mod tmux;
pub mod workspace;

pub use attachment::{
    ATTACHMENT_CODE_CAPACITY, ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED,
    ATTACHMENT_FLAG_REMOTE_REMOVED, ATTACHMENT_MSG_CAPACITY, ATTACHMENT_NAME_CAPACITY,
    ATTACHMENT_PATH_CAPACITY, ATTACHMENT_SNAPSHOT_SIZE, AttachmentError, AttachmentPhase,
    AttachmentSnapshot, MAX_ATTACHMENT_BYTES, attachment_begin, attachment_cancel,
    attachment_delete_remote, attachment_dispose, attachment_insert, attachment_retry_upload,
    attachment_snapshot,
};
pub use ffi::{
    meeterm_attachment_begin, meeterm_attachment_cancel, meeterm_attachment_delete_remote,
    meeterm_attachment_dispose, meeterm_attachment_insert, meeterm_attachment_retry_upload,
    meeterm_attachment_snapshot, meeterm_attachment_snapshot_size, meeterm_change_runtime,
    meeterm_clear_selection, meeterm_commit_modified_utf8, meeterm_commit_modified_utf8_at_epoch,
    meeterm_commit_utf8, meeterm_commit_utf8_at_epoch, meeterm_connect, meeterm_connect_host,
    meeterm_connection_snapshot, meeterm_connection_snapshot_size, meeterm_create_runtime,
    meeterm_create_terminal, meeterm_create_tmux_session, meeterm_destroy_terminal,
    meeterm_disconnect, meeterm_forget_host_key, meeterm_input_commit_count, meeterm_list_runtimes,
    meeterm_network_changed, meeterm_operation_epoch, meeterm_pane_record_size, meeterm_paste_utf8,
    meeterm_paste_utf8_at_epoch, meeterm_reconnect, meeterm_refresh_runtimes,
    meeterm_resize_terminal, meeterm_resize_terminal_at_epoch, meeterm_respond_host_key,
    meeterm_retry_recovery, meeterm_runtime_discovery, meeterm_runtime_discovery_size,
    meeterm_runtime_snapshot, meeterm_runtime_snapshot_size, meeterm_scroll_lines,
    meeterm_scroll_lines_at_epoch, meeterm_select_pane, meeterm_select_runtime,
    meeterm_select_start, meeterm_select_update, meeterm_selection_text, meeterm_send_bytes,
    meeterm_send_bytes_at_epoch, meeterm_send_key, meeterm_send_key_at_epoch,
    meeterm_send_special_key, meeterm_send_special_key_at_epoch, meeterm_session_panes,
    meeterm_set_automatic_reconnect, meeterm_set_foreground, meeterm_set_scrollback_limit,
    meeterm_set_terminal_visible, meeterm_set_theme, meeterm_snapshot, meeterm_snapshot_size,
    meeterm_terminal_exists, meeterm_terminal_revision, meeterm_tmux_command,
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
    HOST_CAPACITY, close_group, close_pane, close_workspace, connect_host, connection_snapshot,
    create_group, create_pane, create_runtime, create_workspace, disconnect_terminal,
    forget_host_key, list_runtimes, reconnect_terminal, refresh_terminal, rename_group,
    rename_pane, rename_workspace, runtime_discovery_snapshot, select_group, select_pane,
    select_runtime, send_bytes, session_snapshot, set_automatic_reconnect, set_foreground,
    set_terminal_visible, terminal_revision, workspace_snapshot_json,
};
pub use terminal::{
    DEFAULT_SCROLLBACK_LINES, FIXED_DEMO_BYTES, MAX_SCROLLBACK_LINES, MIN_SCROLLBACK_LINES,
    Terminal, TerminalError,
};
pub use tmux::{PaneSnapshot, SESSION_NAME, SessionSnapshot, WindowSnapshot};

#[cfg(test)]
mod herdr_probe_tests;
#[cfg(test)]
mod semantic_input_tests;
#[cfg(test)]
mod tests;
