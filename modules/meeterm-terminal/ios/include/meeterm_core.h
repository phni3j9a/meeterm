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
  MEETERM_SSH_STATE_RECONNECTING = 10
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

/* Explicit backend/runtime variant. The legacy meeterm_connect ABI above
 * remains the tmux/default path for existing native callers. */
int32_t meeterm_connect_backend(
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
  size_t password_length,
  const uint8_t *backend,
  size_t backend_length,
  const uint8_t *runtime,
  size_t runtime_length
);

int32_t meeterm_disconnect(uint64_t terminal_id);
int32_t meeterm_reconnect(uint64_t terminal_id);
/* 0=create window, 1=rename window, 2=close window, 3=create pane,
 * 4=rename pane, 5=close pane, 6=redraw selected pane, 7=create group,
 * 8=rename group, 9=close group, 10=select group. Targets are numeric
 * identities; names are UTF-8 arguments, never executable shell text. */
int32_t meeterm_tmux_command(uint64_t terminal_id, uint32_t operation, uint64_t target,
  const uint8_t *name, size_t name_length);
int32_t meeterm_set_foreground(uint64_t terminal_id, uint8_t foreground);
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

#ifdef __cplusplus
}
#endif

#endif
