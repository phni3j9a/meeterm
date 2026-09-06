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
  size_t known_hosts_path_length
);

int32_t meeterm_disconnect(uint64_t terminal_id);
int32_t meeterm_reconnect(uint64_t terminal_id);
int32_t meeterm_select_pane(uint64_t terminal_id, uint64_t pane_id);
uint8_t meeterm_terminal_exists(uint64_t terminal_id);

/* Low-frequency topology only, never terminal cells or output. */
typedef struct meeterm_tmux_pane {
  uint64_t window_id;
  uint64_t pane_id;
  uint64_t terminal_id;
  uint16_t window_name_len;
  uint8_t selected;
  uint8_t reserved[5];
  uint8_t window_name[256];
} meeterm_tmux_pane_t;

/* Returns required record count; copies only when capacity is sufficient.
 * SIZE_MAX indicates an unavailable session. A new connection has zero panes. */
size_t meeterm_session_panes(uint64_t terminal_id, meeterm_tmux_pane_t *output, size_t capacity);
size_t meeterm_pane_record_size(void);
size_t meeterm_connection_snapshot_size(void);

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
