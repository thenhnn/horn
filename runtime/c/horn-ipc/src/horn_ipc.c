#include "horn_ipc.h"

#include <string.h>

void horn_encoder_init(horn_encoder_t *enc, uint8_t *data, size_t len) {
    enc->data = data;
    enc->len = len;
    enc->pos = 0;
    enc->caps = NULL;
}

void horn_decoder_init(horn_decoder_t *dec, const uint8_t *data, size_t len, horn_cap_source_t *caps) {
    dec->data = data;
    dec->len = len;
    dec->pos = 0;
    dec->caps = caps;
}

void horn_encoder_align(horn_encoder_t *enc, size_t align) {
    if (align > 1) {
        size_t end = (enc->pos + align - 1) / align * align;
        horn_encoder_skip(enc, end - enc->pos);
    }
}

void horn_encoder_skip(horn_encoder_t *enc, size_t size) {
    if (enc->pos + size <= enc->len) {
        memset(enc->data + enc->pos, 0, size);
    }
    enc->pos += size;
}

horn_status_t horn_decoder_align(horn_decoder_t *dec, size_t align) {
    if (align > 1) {
        dec->pos = (dec->pos + align - 1) / align * align;
    }
    return dec->pos <= dec->len ? HORN_OK : HORN_ERR_TRUNCATED;
}

horn_status_t horn_decoder_skip(horn_decoder_t *dec, size_t size) {
    if (dec->pos + size > dec->len) {
        return HORN_ERR_TRUNCATED;
    }
    dec->pos += size;
    return HORN_OK;
}

void horn_encoder_u8(horn_encoder_t *enc, uint8_t value) {
    if (enc->pos < enc->len) {
        enc->data[enc->pos] = value;
    }
    enc->pos += 1;
}

void horn_encoder_u16(horn_encoder_t *enc, uint16_t value) {
    if (enc->pos + 2 <= enc->len) {
        memcpy(enc->data + enc->pos, &value, 2);
    }
    enc->pos += 2;
}

void horn_encoder_u32(horn_encoder_t *enc, uint32_t value) {
    if (enc->pos + 4 <= enc->len) {
        memcpy(enc->data + enc->pos, &value, 4);
    }
    enc->pos += 4;
}

void horn_encoder_u64(horn_encoder_t *enc, uint64_t value) {
    if (enc->pos + 8 <= enc->len) {
        memcpy(enc->data + enc->pos, &value, 8);
    }
    enc->pos += 8;
}

void horn_encoder_i8(horn_encoder_t *enc, int8_t value) { horn_encoder_u8(enc, (uint8_t)value); }
void horn_encoder_i16(horn_encoder_t *enc, int16_t value) { horn_encoder_u16(enc, (uint16_t)value); }
void horn_encoder_i32(horn_encoder_t *enc, int32_t value) { horn_encoder_u32(enc, (uint32_t)value); }
void horn_encoder_i64(horn_encoder_t *enc, int64_t value) { horn_encoder_u64(enc, (uint64_t)value); }

void horn_encoder_f32(horn_encoder_t *enc, float value) {
    uint32_t bits;
    memcpy(&bits, &value, 4);
    horn_encoder_u32(enc, bits);
}

void horn_encoder_f64(horn_encoder_t *enc, double value) {
    uint64_t bits;
    memcpy(&bits, &value, 8);
    horn_encoder_u64(enc, bits);
}

void horn_encoder_bool(horn_encoder_t *enc, bool value) { horn_encoder_u8(enc, value ? 1 : 0); }

horn_status_t horn_decoder_u8(horn_decoder_t *dec, uint8_t *value) {
    if (dec->pos + 1 > dec->len) return HORN_ERR_TRUNCATED;
    *value = dec->data[dec->pos++];
    return HORN_OK;
}

horn_status_t horn_decoder_u16(horn_decoder_t *dec, uint16_t *value) {
    if (dec->pos + 2 > dec->len) return HORN_ERR_TRUNCATED;
    memcpy(value, dec->data + dec->pos, 2);
    dec->pos += 2;
    return HORN_OK;
}

horn_status_t horn_decoder_u32(horn_decoder_t *dec, uint32_t *value) {
    if (dec->pos + 4 > dec->len) return HORN_ERR_TRUNCATED;
    memcpy(value, dec->data + dec->pos, 4);
    dec->pos += 4;
    return HORN_OK;
}

horn_status_t horn_decoder_u64(horn_decoder_t *dec, uint64_t *value) {
    if (dec->pos + 8 > dec->len) return HORN_ERR_TRUNCATED;
    memcpy(value, dec->data + dec->pos, 8);
    dec->pos += 8;
    return HORN_OK;
}

horn_status_t horn_decoder_i8(horn_decoder_t *dec, int8_t *value) {
    uint8_t tmp = 0;
    horn_status_t status = horn_decoder_u8(dec, &tmp);
    *value = (int8_t)tmp;
    return status;
}

horn_status_t horn_decoder_i16(horn_decoder_t *dec, int16_t *value) {
    uint16_t tmp = 0;
    horn_status_t status = horn_decoder_u16(dec, &tmp);
    *value = (int16_t)tmp;
    return status;
}

horn_status_t horn_decoder_i32(horn_decoder_t *dec, int32_t *value) {
    uint32_t tmp = 0;
    horn_status_t status = horn_decoder_u32(dec, &tmp);
    *value = (int32_t)tmp;
    return status;
}

horn_status_t horn_decoder_i64(horn_decoder_t *dec, int64_t *value) {
    uint64_t tmp = 0;
    horn_status_t status = horn_decoder_u64(dec, &tmp);
    *value = (int64_t)tmp;
    return status;
}

horn_status_t horn_decoder_f32(horn_decoder_t *dec, float *value) {
    uint32_t bits = 0;
    horn_status_t status = horn_decoder_u32(dec, &bits);
    memcpy(value, &bits, 4);
    return status;
}

horn_status_t horn_decoder_f64(horn_decoder_t *dec, double *value) {
    uint64_t bits = 0;
    horn_status_t status = horn_decoder_u64(dec, &bits);
    memcpy(value, &bits, 8);
    return status;
}

horn_status_t horn_decoder_bool(horn_decoder_t *dec, bool *value) {
    uint8_t tmp = 0;
    horn_status_t status = horn_decoder_u8(dec, &tmp);
    *value = tmp != 0;
    return status;
}

horn_status_t horn_cap_list_push(horn_cap_list_t *list, horn_cap_t cap) {
    if (list->len >= HORN_MAX_CAPS) {
        return HORN_ERR_TOO_MANY_CAPS;
    }
    list->refs[list->len++] = cap;
    return HORN_OK;
}

horn_status_t horn_cap_list_push_mode(horn_cap_list_t *list, horn_cap_t cap, horn_cap_mode_t mode) {
    cap.mode = mode;
    return horn_cap_list_push(list, cap);
}

horn_status_t horn_cap_list_pop(horn_cap_list_t *list, horn_cap_t *cap) {
    if (list == NULL || list->read >= list->len) {
        return HORN_ERR_MISSING_CAP;
    }
    *cap = list->refs[list->read++];
    return HORN_OK;
}

horn_status_t horn_cap_sink_push_mode(horn_cap_sink_t *sink, horn_cap_t cap, horn_cap_mode_t mode) {
    cap.mode = mode;
    return sink->push(sink->ctx, cap);
}

horn_status_t horn_cap_source_pop(horn_cap_source_t *source, horn_cap_t *cap) {
    return source->pop(source->ctx, cap);
}

static horn_status_t horn_cap_list_sink_push(void *ctx, horn_cap_t cap) {
    return horn_cap_list_push((horn_cap_list_t *)ctx, cap);
}

horn_cap_sink_t horn_cap_list_sink(horn_cap_list_t *list) {
    horn_cap_sink_t sink = { .ctx = list, .push = horn_cap_list_sink_push };
    return sink;
}

static horn_status_t horn_cap_list_source_pop(void *ctx, horn_cap_t *cap) {
    return horn_cap_list_pop((horn_cap_list_t *)ctx, cap);
}

horn_cap_source_t horn_cap_list_source(horn_cap_list_t *list) {
    horn_cap_source_t source = { .ctx = list, .pop = horn_cap_list_source_pop };
    return source;
}

void horn_cap_packer_init(horn_cap_packer_t *packer, horn_cap_space_t *space, const horn_cap_layout_t *layout) {
    memset(packer, 0, sizeof(*packer));
    packer->space = space;
    packer->layout = layout;
    packer->direct = layout->cnode == NULL;
}

horn_status_t horn_cap_packer_push(horn_cap_packer_t *packer, horn_cap_t cap) {
    if (packer->direct) {
        if (packer->caps.len >= packer->layout->direct_count) {
            return HORN_ERR_TOO_MANY_CAPS;
        }
        return horn_cap_list_push(&packer->caps, cap);
    }
    const horn_cap_node_t *node = packer->layout->cnode;
    if (!packer->have_cnode) {
        horn_status_t status = packer->space->alloc_cnode(
            packer->space->ctx, NULL, node->size_bits, &packer->cnode);
        if (status != HORN_OK) {
            return status;
        }
        packer->have_cnode = true;
    }
    if (packer->next_slot >= node->slot_count) {
        return HORN_ERR_TOO_MANY_CAPS;
    }
    const horn_cap_slot_t *slot = &node->slots[packer->next_slot++];
    if (cap.mode == HORN_CAP_MOVE) {
        return packer->space->move_into(
            packer->space->ctx, packer->cnode, slot->slot, cap);
    }
    return packer->space->copy_into(
        packer->space->ctx, packer->cnode, slot->slot, cap, cap.rights);
}

horn_status_t horn_cap_packer_finish(horn_cap_packer_t *packer, horn_cap_list_t *caps) {
    if (packer->direct) {
        if (!packer->layout->variable && packer->caps.len != packer->layout->direct_count) {
            return HORN_ERR_MISSING_CAP;
        }
    } else {
        const horn_cap_node_t *node = packer->layout->cnode;
        if (packer->layout->variable && packer->next_slot == 0) { *caps = packer->caps; return HORN_OK; }
        if (!packer->have_cnode || (!packer->layout->variable && packer->next_slot != node->slot_count)) {
            return HORN_ERR_MISSING_CAP;
        }
        horn_status_t status = horn_cap_list_push(&packer->caps, (horn_cap_t){
            .slot = packer->cnode.slot,
            .mode = HORN_CAP_MOVE,
            .rights = 0xfu,
        });
        if (status != HORN_OK) {
            return status;
        }
    }
    *caps = packer->caps;
    return HORN_OK;
}

static horn_status_t horn_cap_packer_sink_push(void *ctx, horn_cap_t cap) {
    return horn_cap_packer_push((horn_cap_packer_t *)ctx, cap);
}

horn_cap_sink_t horn_cap_packer_sink(horn_cap_packer_t *packer) {
    horn_cap_sink_t sink = { .ctx = packer, .push = horn_cap_packer_sink_push };
    return sink;
}

void horn_cap_unpacker_init(horn_cap_unpacker_t *unpacker, horn_cap_space_t *space, const horn_cap_layout_t *layout, horn_cap_list_t caps) {
    memset(unpacker, 0, sizeof(*unpacker));
    unpacker->space = space;
    unpacker->layout = layout;
    unpacker->direct = layout->cnode == NULL;
    unpacker->caps = caps;
}

horn_status_t horn_cap_unpacker_pop(horn_cap_unpacker_t *unpacker, horn_cap_t *cap) {
    if (unpacker->direct) {
        if (unpacker->direct_index >= unpacker->layout->direct_count) {
            return HORN_ERR_MISSING_CAP;
        }
        const horn_cap_slot_t *slot = &unpacker->layout->direct[unpacker->direct_index++];
        horn_status_t status = horn_cap_list_pop(&unpacker->caps, cap);
        if (status != HORN_OK) {
            return status;
        }
        cap->mode = slot->mode;
        cap->rights = slot->rights;
        return HORN_OK;
    }
    const horn_cap_node_t *node = unpacker->layout->cnode;
    if (unpacker->next_slot >= node->slot_count) {
        return HORN_ERR_MISSING_CAP;
    }
    if (!unpacker->have_cnode) {
        horn_status_t status = horn_cap_list_pop(&unpacker->caps, &unpacker->cnode);
        if (status != HORN_OK) {
            return status;
        }
        unpacker->have_cnode = true;
    }
    const horn_cap_slot_t *slot = &node->slots[unpacker->next_slot++];
    horn_status_t status = unpacker->space->cap_at(
        unpacker->space->ctx, unpacker->cnode, slot->slot, cap);
    if (status != HORN_OK) {
        return status;
    }
    cap->mode = slot->mode;
    cap->rights = slot->rights;
    return HORN_OK;
}

static horn_status_t horn_cap_unpacker_source_pop(void *ctx, horn_cap_t *cap) {
    return horn_cap_unpacker_pop((horn_cap_unpacker_t *)ctx, cap);
}

horn_status_t horn_cap_unpacker_finish(const horn_cap_unpacker_t *unpacker) {
    return unpacker->caps.read == unpacker->caps.len ? HORN_OK : HORN_ERR_INVALID;
}

horn_cap_source_t horn_cap_unpacker_source(horn_cap_unpacker_t *unpacker) {
    horn_cap_source_t source = { .ctx = unpacker, .pop = horn_cap_unpacker_source_pop };
    return source;
}
