#ifndef MEETERM_CORE_H
#define MEETERM_CORE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint64_t meeterm_create_terminal(uint16_t columns, uint16_t rows);
size_t meeterm_snapshot_size(uint64_t terminal_id);
size_t meeterm_snapshot(uint64_t terminal_id, uint8_t *output, size_t capacity);
int32_t meeterm_resize_terminal(uint64_t terminal_id, uint16_t columns, uint16_t rows);
uint64_t meeterm_commit_utf8(uint64_t terminal_id, const uint8_t *bytes, size_t length);
int32_t meeterm_paste_utf8(uint64_t terminal_id, const uint8_t *bytes, size_t length);
int32_t meeterm_scroll_lines(uint64_t terminal_id, int32_t lines);
/* Key codes: original 0..8, Home=9 End=10 Delete=11 Insert=12,
 * PageUp=13 PageDown=14 F1..F12=15..26. Modifier bits Ctrl=1 Alt=2 Shift=4. */
int32_t meeterm_send_key(uint64_t terminal_id, uint32_t key, uint32_t modifiers);
int32_t meeterm_commit_modified_utf8(uint64_t terminal_id, const uint8_t *bytes, size_t length, uint32_t modifiers);
int32_t meeterm_select_start(uint64_t terminal_id, uint32_t row, uint32_t column);
int32_t meeterm_select_update(uint64_t terminal_id, uint32_t row, uint32_t column);
int32_t meeterm_clear_selection(uint64_t terminal_id);
/* Native clipboard only: required bytes, zero=no selection, SIZE_MAX=error. */
size_t meeterm_selection_text(uint64_t terminal_id, uint8_t *output, size_t capacity);
int32_t meeterm_set_theme(uint64_t terminal_id, uint8_t light);
/* Applies to all live and future terminal states. */
int32_t meeterm_set_scrollback_limit(uint32_t lines);
int32_t meeterm_send_special_key(uint64_t terminal_id, uint32_t key);
/* Native-only input; never expose terminal byte streams through JavaScript. */
int32_t meeterm_send_bytes(uint64_t terminal_id, const uint8_t *bytes, size_t length);
uint64_t meeterm_input_commit_count(uint64_t terminal_id);
int32_t meeterm_destroy_terminal(uint64_t terminal_id);

/* Append-only operation-epoch input ABI. The epoch is per native terminal,
 * not the SSH/session recovery epoch; stale calls return a negative error. */
uint64_t meeterm_operation_epoch(uint64_t terminal_id);
int32_t meeterm_resize_terminal_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  uint16_t columns, uint16_t rows);
uint64_t meeterm_commit_utf8_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  const uint8_t *bytes, size_t length);
int32_t meeterm_paste_utf8_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  const uint8_t *bytes, size_t length);
int32_t meeterm_scroll_lines_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  int32_t lines);
int32_t meeterm_send_key_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  uint32_t key, uint32_t modifiers);
int32_t meeterm_commit_modified_utf8_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  const uint8_t *bytes, size_t length, uint32_t modifiers);
int32_t meeterm_send_special_key_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  uint32_t key);
int32_t meeterm_send_bytes_at_epoch(uint64_t terminal_id, uint64_t expected_epoch,
  const uint8_t *bytes, size_t length);

/*
 * SSH is a Rust-owned lifecycle. These constants and the fixed-size state
 * record are deliberately part of the C ABI so Swift can expose a typed
 * low-frequency Expo record without carrying terminal bytes or JSON through
 * JavaScript. All text fields are UTF-8, have an explicit byte length, and
 * are sanitized by Rust. A field that does not apply is an empty string.
 */
enum {
  MEETERM_SSH_STATE_DISCONNECTED = 0,
  MEETERM_SSH_STATE_CONNECTING = 1,
  MEETERM_SSH_STATE_HOST_KEY_PENDING = 2,
  MEETERM_SSH_STATE_AUTHENTICATING = 3,
  MEETERM_SSH_STATE_OPENING_PTY = 4,
  MEETERM_SSH_STATE_READY = 5,
  MEETERM_SSH_STATE_CLOSING = 6,
  MEETERM_SSH_STATE_FAILED = 7,
  MEETERM_SSH_STATE_ATTACHING_TMUX = 8,
  MEETERM_SSH_STATE_SYNCHRONIZING = 9,
  MEETERM_SSH_STATE_RECONNECTING = 10,
  MEETERM_SSH_STATE_DISCOVERING_RUNTIMES = 11,
  MEETERM_SSH_STATE_AWAITING_RUNTIME_SELECTION = 12,
  MEETERM_SSH_STATE_ATTACHING_RUNTIME = 13,
  MEETERM_SSH_STATE_CREATING_RUNTIME = 14
};

enum {
  MEETERM_SSH_HOST_CAPACITY = 256,
  MEETERM_SSH_FINGERPRINT_CAPACITY = 128,
  MEETERM_SSH_ALGORITHM_CAPACITY = 64,
  MEETERM_SSH_ERROR_CODE_CAPACITY = 64,
  MEETERM_SSH_ERROR_MESSAGE_CAPACITY = 256
};

typedef struct meeterm_ssh_connection_state {
  uint32_t state;
  uint16_t port;
  uint16_t reserved;
  uint16_t host_len;
  uint8_t host[MEETERM_SSH_HOST_CAPACITY];
  uint16_t fingerprint_len;
  uint8_t fingerprint[MEETERM_SSH_FINGERPRINT_CAPACITY];
  uint16_t algorithm_len;
  uint8_t algorithm[MEETERM_SSH_ALGORITHM_CAPACITY];
  uint16_t known_fingerprint_len;
  uint8_t known_fingerprint[MEETERM_SSH_FINGERPRINT_CAPACITY];
  uint16_t error_code_len;
  uint8_t error_code[MEETERM_SSH_ERROR_CODE_CAPACITY];
  uint16_t error_message_len;
  uint8_t error_message[MEETERM_SSH_ERROR_MESSAGE_CAPACITY];
} meeterm_ssh_connection_state_t;

/* The Rust ABI uses byte-pointer plus length pairs for all text. */
/* Legacy fresh connect is host-only; runtime binding is explicit below. */
int32_t meeterm_connect(
  uint64_t terminal_id,
  const uint8_t *host,
  size_t host_length,
  uint16_t port,
  const uint8_t *username,
  size_t username_length,
  const uint8_t *private_key,
  size_t private_key_length,
  const uint8_t *passphrase,
  size_t passphrase_length,
  const uint8_t *known_hosts_path,
  size_t known_hosts_path_length,
  const uint8_t *auth_method,
  size_t auth_method_length,
  const uint8_t *password,
  size_t password_length
);

/* Authenticate and discover runtimes without selecting or creating one. */
int32_t meeterm_connect_host(
  uint64_t terminal_id,
  const uint8_t *host,
  size_t host_length,
  uint16_t port,
  const uint8_t *username,
  size_t username_length,
  const uint8_t *private_key,
  size_t private_key_length,
  const uint8_t *passphrase,
  size_t passphrase_length,
  const uint8_t *known_hosts_path,
  size_t known_hosts_path_length,
  const uint8_t *auth_method,
  size_t auth_method_length,
  const uint8_t *password,
  size_t password_length
);

int32_t meeterm_disconnect(uint64_t terminal_id);
/* Switch-only boundary result: 0=accepted, -1..-14=rejected before the
 * owner boundary, -15=accepted after boundary but release failed. */
int32_t meeterm_disconnect_for_switch(uint64_t terminal_id);
int32_t meeterm_reconnect(uint64_t terminal_id);
/* Retry the retained intent for the exact decimal epoch observed by the
 * caller. */
int32_t meeterm_retry_recovery(uint64_t terminal_id, uint64_t expected_epoch);
/* Runtime-boundary result: 0=accepted, -1..-14=rejected before the owner
 * boundary, -15=accepted after boundary but replacement start failed. */
int32_t meeterm_change_runtime(uint64_t terminal_id, uint64_t expected_epoch);
/* 0=create window, 1=rename window, 2=close window, 3=create pane,
 * 4=rename pane, 5=close pane, 6=redraw selected pane, 7=create group,
 * 8=rename group, 9=close group, 10=select group. Targets are numeric
 * identities; names are UTF-8 arguments, never executable shell text. */
int32_t meeterm_tmux_command(uint64_t terminal_id, uint32_t operation, uint64_t target,
  const uint8_t *name, size_t name_length);
int32_t meeterm_set_foreground(uint64_t terminal_id, uint8_t foreground);
void meeterm_network_changed(void);
int32_t meeterm_set_terminal_visible(uint64_t terminal_id, uint8_t visible);
int32_t meeterm_set_automatic_reconnect(uint64_t terminal_id, uint8_t enabled);
int32_t meeterm_select_pane(uint64_t terminal_id, uint64_t pane_id);
uint8_t meeterm_terminal_exists(uint64_t terminal_id);

/* Low-frequency topology only, never terminal cells or output. */
typedef struct meeterm_tmux_pane {
  uint64_t window_id;
  uint64_t pane_id;
  uint64_t terminal_id;
  uint16_t window_name_len;
  uint8_t selected;
  uint8_t active;
  uint8_t reserved[4];
  uint8_t window_name[256];
  uint16_t pane_name_len;
  uint8_t reserved_name[6];
  uint8_t pane_name[256];
} meeterm_tmux_pane_t;

/* Returns required record count; copies only when capacity is sufficient.
 * SIZE_MAX indicates an unavailable session. A new connection has zero panes. */
size_t meeterm_session_panes(uint64_t terminal_id, meeterm_tmux_pane_t *output, size_t capacity);
size_t meeterm_pane_record_size(void);
size_t meeterm_connection_snapshot_size(void);

/* Bounded backend-independent workspace metadata JSON. A size query can
 * become stale when the topology changes; the copy call returns the current
 * required length without copying if capacity is insufficient. */
size_t meeterm_workspace_state_size(uint64_t terminal_id);
size_t meeterm_workspace_state(uint64_t terminal_id, uint8_t *output, size_t capacity);
enum { MEETERM_WORKSPACE_STATE_MAX_BYTES = 4 * 1024 * 1024 };

/* Fill one complete, sanitized low-frequency lifecycle snapshot. */
int32_t meeterm_connection_snapshot(
  uint64_t terminal_id,
  meeterm_ssh_connection_state_t *output
);

/* Return zero when an explicit host-key decision was accepted. */
int32_t meeterm_respond_host_key(
  uint64_t terminal_id,
  const uint8_t *fingerprint,
  size_t fingerprint_length,
  uint8_t accept
);

/* Bounded JSON runtime discovery; JSON contains only sanitized public metadata. */
size_t meeterm_runtime_discovery_size(uint64_t terminal_id);
size_t meeterm_runtime_discovery(uint64_t terminal_id, uint8_t *output, size_t capacity);
enum { MEETERM_RUNTIME_DISCOVERY_MAX_BYTES = 1024 * 1024 };

/* Explicit post-auth runtime controls. IDs/names are UTF-8 byte strings. */
int32_t meeterm_refresh_runtimes(uint64_t terminal_id);
int32_t meeterm_select_runtime(uint64_t terminal_id, const uint8_t *candidate_id, size_t candidate_id_length);
int32_t meeterm_create_tmux_session(uint64_t terminal_id, const uint8_t *name, size_t name_length);

/* Trust-store deletion is explicit and scoped to one endpoint. */
int32_t meeterm_forget_host_key(
  const uint8_t *host,
  size_t host_length,
  uint16_t port,
  const uint8_t *known_hosts_path,
  size_t known_hosts_path_length
);

/* Monotonic native terminal-content revision; zero is valid for a new term. */
uint64_t meeterm_terminal_revision(uint64_t terminal_id);

/*
 * Image attachment operations (Issue #28). Rust owns the whole operation:
 * a bounded SFTP upload over the existing authenticated SSH connection,
 * a fenced destination identity, and — only on explicit request — one
 * quoted remote-path line through the epoch-guarded native paste path.
 * No image bytes cross JavaScript; no Enter is ever generated; uploaded,
 * inserted, and CLI/model-observed are distinct milestones.
 */
enum {
  MEETERM_ATTACHMENT_PATH_CAPACITY = 512,
  MEETERM_ATTACHMENT_NAME_CAPACITY = 128,
  MEETERM_ATTACHMENT_CODE_CAPACITY = 64,
  MEETERM_ATTACHMENT_MSG_CAPACITY = 256
};

enum {
  MEETERM_ATTACHMENT_PENDING = 0,
  MEETERM_ATTACHMENT_UPLOADING = 1,
  MEETERM_ATTACHMENT_UPLOADED = 2,
  MEETERM_ATTACHMENT_INSERTED = 3,
  MEETERM_ATTACHMENT_FAILED = 4,
  MEETERM_ATTACHMENT_CANCELLED = 5
};

enum {
  MEETERM_ATTACHMENT_FLAG_INSERT_ENQUEUED_UNCONFIRMED = 0x1,
  /* The meeterm-created remote file was explicitly deleted. Composes with
   * the phase: inserted is not revoked; uploaded+removed = path is gone. */
  MEETERM_ATTACHMENT_FLAG_REMOTE_REMOVED = 0x2
};

typedef struct meeterm_attachment_snapshot {
  uint32_t phase;
  uint32_t flags;
  uint64_t attachment_id;
  uint64_t bytes_uploaded;
  uint64_t size_bytes;
  uint16_t remote_path_len;
  uint8_t remote_path[MEETERM_ATTACHMENT_PATH_CAPACITY];
  uint16_t display_name_len;
  uint8_t display_name[MEETERM_ATTACHMENT_NAME_CAPACITY];
  uint16_t error_code_len;
  uint8_t error_code[MEETERM_ATTACHMENT_CODE_CAPACITY];
  uint16_t error_message_len;
  uint8_t error_message[MEETERM_ATTACHMENT_MSG_CAPACITY];
} meeterm_attachment_snapshot_t;

/* Record the destination intent when the attachment sheet is confirmed:
 * pass the *picked pane's* native terminal id (any pane, not just the
 * connection owner); the core resolves the owning SSH connection and
 * stores the stable endpoint/backend/runtime/pane identity. Returns an
 * opaque positive intent id; zero = not a usable destination. */
uint64_t meeterm_attachment_intent(uint64_t target_terminal_id);
/* Drop a recorded intent. Idempotent; live ops keep their own copy. */
int32_t meeterm_attachment_intent_dispose(uint64_t intent_id);
/* Opaque positive attachment id; zero = synchronously rejected.
 * `intent_id` is a live meeterm_attachment_intent handle.
 * `remote_dir` is an optional explicit remote directory (clean absolute
 * or ~/-prefixed path expanded against realpath(".")); NULL/0 selects the
 * app-private default under the SFTP start dir
 * (.local/share/meeterm/attachments). */
uint64_t meeterm_attachment_begin(
  uint64_t intent_id,
  const uint8_t *local_path,
  size_t local_path_length,
  const uint8_t *display_name,
  size_t display_name_length,
  const uint8_t *remote_dir,
  size_t remote_dir_length,
  uint64_t size_bytes);
/* Explicit transfer retry for pending/failed, or remote re-verification
 * for an uploaded op whose fence was revoked by recovery. Re-fences the
 * same intent identity; target_terminal_id must be the intent's pane. */
int32_t meeterm_attachment_retry_upload(uint64_t target_terminal_id, uint64_t attachment_id);
/* One quoted path line into the intent's recorded pane only; never sends
 * Enter, never retargets to the currently selected pane. */
int32_t meeterm_attachment_insert(uint64_t target_terminal_id, uint64_t attachment_id);
/* Cancels in-flight work and discards delayed completion idempotently. */
int32_t meeterm_attachment_cancel(uint64_t attachment_id);
/* Drops the operation record, cancelling active work first; remote files
 * are never auto-deleted — see meeterm_attachment_delete_remote. */
int32_t meeterm_attachment_dispose(uint64_t attachment_id);
/* Explicit remote deletion of only this operation's generated
 * meeterm-* / .meeterm-partial-* names, on the same authenticated SSH
 * endpoint recorded by the intent; the canonical upload base is
 * re-resolved and every component must still be a real directory.
 * target_terminal_id must be the intent's pane. Idempotent; phase kept. */
int32_t meeterm_attachment_delete_remote(uint64_t target_terminal_id, uint64_t attachment_id);
/* Poll: fills one complete sanitized snapshot; negative = unknown id. */
int32_t meeterm_attachment_snapshot(
  uint64_t attachment_id,
  meeterm_attachment_snapshot_t *output);
size_t meeterm_attachment_snapshot_size(void);

#ifdef __cplusplus
}
#endif

#endif
