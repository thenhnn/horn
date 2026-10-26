/* Platform-independent wire, capability, and transport primitives. */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define HORN_MAX_CAPS 3u
#define HORN_MAX_MESSAGE_BYTES 960u

typedef enum horn_cap_mode {
    HORN_CAP_COPY = 0,
    HORN_CAP_MOVE = 1,
    HORN_CAP_BORROW = 2,
} horn_cap_mode_t;

#define HORN_RESULT_OK 0u
#define HORN_RESULT_ERR 1u

typedef struct horn_cap {
    uint64_t slot;
    horn_cap_mode_t mode;
    uint32_t rights;
} horn_cap_t;

typedef struct horn_cap_list {
    horn_cap_t refs[HORN_MAX_CAPS];
    uint32_t len;
    uint32_t read;
} horn_cap_list_t;

typedef struct horn_cap_slot {
    const char *path;
    uint32_t slot;
    horn_cap_mode_t mode;
    uint32_t rights;
} horn_cap_slot_t;

typedef struct horn_cap_node {
    uint8_t size_bits;
    const horn_cap_slot_t *slots;
    size_t slot_count;
} horn_cap_node_t;

typedef struct horn_cap_layout {
    const horn_cap_slot_t *direct;
    size_t direct_count;
    const horn_cap_node_t *cnode;
    bool variable;
} horn_cap_layout_t;

typedef enum horn_status {
    HORN_OK = 0,
    HORN_ERR_TRUNCATED,
    HORN_ERR_TOO_MANY_CAPS,
    HORN_ERR_MISSING_CAP,
    HORN_ERR_UNKNOWN_METHOD,
    HORN_ERR_INVALID,
    HORN_ERR_TRANSPORT,
} horn_status_t;

typedef struct horn_cap_sink horn_cap_sink_t;
typedef struct horn_cap_source horn_cap_source_t;

typedef struct horn_encoder {
    uint8_t *data;
    size_t len;
    size_t pos;
    horn_cap_list_t *caps;
} horn_encoder_t;

typedef struct horn_decoder {
    const uint8_t *data;
    size_t len;
    size_t pos;
    horn_cap_source_t *caps;
} horn_decoder_t;

void horn_encoder_init(horn_encoder_t *enc, uint8_t *data, size_t len);
void horn_decoder_init(horn_decoder_t *dec, const uint8_t *data, size_t len, horn_cap_source_t *caps);
void horn_encoder_align(horn_encoder_t *enc, size_t align);
void horn_encoder_skip(horn_encoder_t *enc, size_t size);
horn_status_t horn_decoder_align(horn_decoder_t *dec, size_t align);
horn_status_t horn_decoder_skip(horn_decoder_t *dec, size_t size);
void horn_encoder_u8(horn_encoder_t *enc, uint8_t value);
void horn_encoder_u16(horn_encoder_t *enc, uint16_t value);
void horn_encoder_u32(horn_encoder_t *enc, uint32_t value);
void horn_encoder_u64(horn_encoder_t *enc, uint64_t value);
void horn_encoder_i8(horn_encoder_t *enc, int8_t value);
void horn_encoder_i16(horn_encoder_t *enc, int16_t value);
void horn_encoder_i32(horn_encoder_t *enc, int32_t value);
void horn_encoder_i64(horn_encoder_t *enc, int64_t value);
void horn_encoder_f32(horn_encoder_t *enc, float value);
void horn_encoder_f64(horn_encoder_t *enc, double value);
void horn_encoder_bool(horn_encoder_t *enc, bool value);
horn_status_t horn_decoder_u8(horn_decoder_t *dec, uint8_t *value);
horn_status_t horn_decoder_u16(horn_decoder_t *dec, uint16_t *value);
horn_status_t horn_decoder_u32(horn_decoder_t *dec, uint32_t *value);
horn_status_t horn_decoder_u64(horn_decoder_t *dec, uint64_t *value);
horn_status_t horn_decoder_i8(horn_decoder_t *dec, int8_t *value);
horn_status_t horn_decoder_i16(horn_decoder_t *dec, int16_t *value);
horn_status_t horn_decoder_i32(horn_decoder_t *dec, int32_t *value);
horn_status_t horn_decoder_i64(horn_decoder_t *dec, int64_t *value);
horn_status_t horn_decoder_f32(horn_decoder_t *dec, float *value);
horn_status_t horn_decoder_f64(horn_decoder_t *dec, double *value);
horn_status_t horn_decoder_bool(horn_decoder_t *dec, bool *value);
horn_status_t horn_cap_list_push(horn_cap_list_t *list, horn_cap_t cap);
horn_status_t horn_cap_list_push_mode(horn_cap_list_t *list, horn_cap_t cap, horn_cap_mode_t mode);
horn_status_t horn_cap_list_pop(horn_cap_list_t *list, horn_cap_t *cap);

typedef struct horn_slot_ref {
    horn_cap_t cnode;
    uint32_t slot;
} horn_slot_ref_t;

typedef struct horn_cap_space {
    void *ctx;
    horn_status_t (*alloc_cnode)(void *ctx, const horn_slot_ref_t *parent, uint8_t size_bits, horn_cap_t *out);
    horn_status_t (*free_cnode)(void *ctx, horn_cap_t cnode);
    horn_status_t (*copy_into)(void *ctx, horn_cap_t cnode, uint32_t slot, horn_cap_t src, uint32_t rights);
    horn_status_t (*move_into)(void *ctx, horn_cap_t cnode, uint32_t slot, horn_cap_t src);
    horn_status_t (*cap_at)(void *ctx, horn_cap_t cnode, uint32_t slot, horn_cap_t *out);
    horn_status_t (*delete_cap)(void *ctx, horn_cap_t cap);
} horn_cap_space_t;

struct horn_cap_sink {
    void *ctx;
    horn_status_t (*push)(void *ctx, horn_cap_t cap);
};

struct horn_cap_source {
    void *ctx;
    horn_status_t (*pop)(void *ctx, horn_cap_t *cap);
};

horn_status_t horn_cap_sink_push_mode(horn_cap_sink_t *sink, horn_cap_t cap, horn_cap_mode_t mode);
horn_status_t horn_cap_source_pop(horn_cap_source_t *source, horn_cap_t *cap);
horn_cap_sink_t horn_cap_list_sink(horn_cap_list_t *list);
horn_cap_source_t horn_cap_list_source(horn_cap_list_t *list);

typedef struct horn_cap_packer {
    horn_cap_space_t *space;
    const horn_cap_layout_t *layout;
    bool direct;
    horn_cap_t cnode;
    bool have_cnode;
    size_t next_slot;
    horn_cap_list_t caps;
} horn_cap_packer_t;

void horn_cap_packer_init(horn_cap_packer_t *packer, horn_cap_space_t *space, const horn_cap_layout_t *layout);
horn_status_t horn_cap_packer_push(horn_cap_packer_t *packer, horn_cap_t cap);
horn_status_t horn_cap_packer_finish(horn_cap_packer_t *packer, horn_cap_list_t *caps);
horn_cap_sink_t horn_cap_packer_sink(horn_cap_packer_t *packer);

typedef struct horn_cap_unpacker {
    horn_cap_space_t *space;
    const horn_cap_layout_t *layout;
    bool direct;
    horn_cap_list_t caps;
    size_t direct_index;
    horn_cap_t cnode;
    bool have_cnode;
    size_t next_slot;
} horn_cap_unpacker_t;

void horn_cap_unpacker_init(horn_cap_unpacker_t *unpacker, horn_cap_space_t *space, const horn_cap_layout_t *layout, horn_cap_list_t caps);
horn_status_t horn_cap_unpacker_pop(horn_cap_unpacker_t *unpacker, horn_cap_t *cap);
horn_status_t horn_cap_unpacker_finish(const horn_cap_unpacker_t *unpacker);
horn_cap_source_t horn_cap_unpacker_source(horn_cap_unpacker_t *unpacker);

typedef struct horn_transport {
    horn_cap_space_t *space;
    horn_status_t (*call)(void *ctx, horn_cap_t target, uint64_t label,
                          const uint8_t *args, size_t args_len, horn_cap_list_t *caps,
                          uint8_t *result, size_t result_capacity, size_t *result_len,
                          horn_cap_list_t *result_caps);
    horn_status_t (*send)(void *ctx, horn_cap_t target, uint64_t label,
                          const uint8_t *args, size_t args_len, horn_cap_list_t *caps);
} horn_transport_t;
