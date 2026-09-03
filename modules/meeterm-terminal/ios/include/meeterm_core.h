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
uint64_t meeterm_input_commit_count(uint64_t terminal_id);
int32_t meeterm_destroy_terminal(uint64_t terminal_id);

#ifdef __cplusplus
}
#endif

#endif
