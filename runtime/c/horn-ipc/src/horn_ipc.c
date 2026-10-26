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
        enc->pos = (enc->pos + align - 1) / align * align;
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

static horn_status_t horn_cap_packer_next_leaf(horn_cap_packer_t *packer, horn_slot_ref_t *out, bool *found) {
    *found = false;
    for (;;) {
        if (packer->depth == 0) {
            if (packer->root_index >= packer->layout->root_count) {
                return HORN_OK;
            }
            const horn_cap_node_t *node = packer->layout->roots[packer->root_index++];
            horn_cap_t cnode;
            horn_status_t status = packer->space->alloc_cnode(packer->space->ctx, NULL, node->size_bits, &cnode);
            if (status != HORN_OK) {
                return status;
            }
            packer->stack[0] = (horn_cap_cursor_t){ .cnode = cnode, .node = node, .entry = 0, .attach_slot = 0 };
            packer->depth = 1;
        }
        horn_cap_cursor_t *top = &packer->stack[packer->depth - 1];
        if (top->entry >= top->node->entry_count) {
            horn_cap_cursor_t finished = *top;
            packer->depth--;
            if (packer->depth == 0) {
                horn_status_t status = horn_cap_list_push(&packer->roots, (horn_cap_t){
                    .slot = finished.cnode.slot,
                    .mode = HORN_CAP_MOVE,
                    .rights = 0xfu,
                });
                if (status != HORN_OK) {
                    return status;
                }
            } else {
                horn_cap_cursor_t *parent = &packer->stack[packer->depth - 1];
                horn_status_t status = packer->space->move_into(
                    packer->space->ctx, parent->cnode, finished.attach_slot, finished.cnode);
                if (status != HORN_OK) {
                    return status;
                }
            }
            continue;
        }
        horn_cap_entry_t entry = top->node->entries[top->entry++];
        if (!entry.is_child) {
            out->cnode = top->cnode;
            out->slot = entry.leaf.slot;
            *found = true;
            return HORN_OK;
        }
        if (packer->depth >= HORN_MAX_LAYOUT_DEPTH) {
            return HORN_ERR_INVALID;
        }
        horn_cap_t child;
        horn_slot_ref_t parent = { .cnode = top->cnode, .slot = entry.slot };
        horn_status_t status = packer->space->alloc_cnode(
            packer->space->ctx, &parent, entry.child->size_bits, &child);
        if (status != HORN_OK) {
            return status;
        }
        packer->stack[packer->depth++] = (horn_cap_cursor_t){
            .cnode = child,
            .node = entry.child,
            .entry = 0,
            .attach_slot = entry.slot,
        };
    }
}

void horn_cap_packer_init(horn_cap_packer_t *packer, horn_cap_space_t *space, const horn_cap_layout_t *layout) {
    memset(packer, 0, sizeof(*packer));
    packer->space = space;
    packer->layout = layout;
    packer->direct = layout->root_count == 0;
}

horn_status_t horn_cap_packer_push(horn_cap_packer_t *packer, horn_cap_t cap) {
    if (packer->direct) {
        return horn_cap_list_push(&packer->roots, cap);
    }
    horn_slot_ref_t slot;
    bool found = false;
    horn_status_t status = horn_cap_packer_next_leaf(packer, &slot, &found);
    if (status != HORN_OK) {
        return status;
    }
    if (!found) {
        return HORN_ERR_TOO_MANY_CAPS;
    }
    if (cap.mode == HORN_CAP_MOVE) {
        return packer->space->move_into(packer->space->ctx, slot.cnode, slot.slot, cap);
    }
    return packer->space->copy_into(packer->space->ctx, slot.cnode, slot.slot, cap, cap.rights);
}

horn_status_t horn_cap_packer_finish(horn_cap_packer_t *packer, horn_cap_list_t *roots) {
    if (!packer->direct) {
        for (;;) {
            horn_slot_ref_t slot;
            bool found = false;
            horn_status_t status = horn_cap_packer_next_leaf(packer, &slot, &found);
            if (status != HORN_OK) {
                return status;
            }
            if (!found) {
                break;
            }
        }
    }
    *roots = packer->roots;
    return HORN_OK;
}

static horn_status_t horn_cap_packer_sink_push(void *ctx, horn_cap_t cap) {
    return horn_cap_packer_push((horn_cap_packer_t *)ctx, cap);
}

horn_cap_sink_t horn_cap_packer_sink(horn_cap_packer_t *packer) {
    horn_cap_sink_t sink = { .ctx = packer, .push = horn_cap_packer_sink_push };
    return sink;
}

static horn_status_t horn_cap_unpacker_next_leaf(horn_cap_unpacker_t *unpacker, horn_cap_t *out, bool *found) {
    *found = false;
    for (;;) {
        if (unpacker->depth == 0) {
            horn_cap_t root;
            horn_status_t status = horn_cap_list_pop(&unpacker->roots, &root);
            if (status != HORN_OK) {
                return status;
            }
            if (unpacker->root_index >= unpacker->layout->root_count) {
                return HORN_ERR_MISSING_CAP;
            }
            const horn_cap_node_t *node = unpacker->layout->roots[unpacker->root_index++];
            unpacker->stack[0] = (horn_cap_cursor_t){ .cnode = root, .node = node, .entry = 0, .attach_slot = 0 };
            unpacker->depth = 1;
        }
        horn_cap_cursor_t *top = &unpacker->stack[unpacker->depth - 1];
        if (top->entry >= top->node->entry_count) {
            unpacker->depth--;
            continue;
        }
        horn_cap_entry_t entry = top->node->entries[top->entry++];
        horn_cap_t cnode = top->cnode;
        if (!entry.is_child) {
            horn_cap_t cap;
            horn_status_t status = unpacker->space->cap_at(
                unpacker->space->ctx, cnode, entry.leaf.slot, &cap);
            if (status != HORN_OK) {
                return status;
            }
            cap.mode = entry.leaf.mode;
            cap.rights = entry.leaf.rights;
            *out = cap;
            *found = true;
            return HORN_OK;
        }
        if (unpacker->depth >= HORN_MAX_LAYOUT_DEPTH) {
            return HORN_ERR_INVALID;
        }
        horn_cap_t child;
        horn_status_t status = unpacker->space->cap_at(
            unpacker->space->ctx, cnode, entry.slot, &child);
        if (status != HORN_OK) {
            return status;
        }
        unpacker->stack[unpacker->depth++] = (horn_cap_cursor_t){
            .cnode = child,
            .node = entry.child,
            .entry = 0,
            .attach_slot = entry.slot,
        };
    }
}

void horn_cap_unpacker_init(horn_cap_unpacker_t *unpacker, horn_cap_space_t *space, const horn_cap_layout_t *layout, horn_cap_list_t roots) {
    memset(unpacker, 0, sizeof(*unpacker));
    unpacker->space = space;
    unpacker->layout = layout;
    unpacker->direct = layout->root_count == 0;
    unpacker->roots = roots;
}

horn_status_t horn_cap_unpacker_pop(horn_cap_unpacker_t *unpacker, horn_cap_t *cap) {
    if (unpacker->direct) {
        if (unpacker->direct_index >= unpacker->layout->direct_count) {
            return HORN_ERR_MISSING_CAP;
        }
        const horn_cap_slot_t *slot = &unpacker->layout->direct[unpacker->direct_index++];
        horn_status_t status = horn_cap_list_pop(&unpacker->roots, cap);
        if (status != HORN_OK) {
            return status;
        }
        cap->mode = slot->mode;
        cap->rights = slot->rights;
        return HORN_OK;
    }
    bool found = false;
    horn_status_t status = horn_cap_unpacker_next_leaf(unpacker, cap, &found);
    if (status != HORN_OK) {
        return status;
    }
    return found ? HORN_OK : HORN_ERR_MISSING_CAP;
}

static horn_status_t horn_cap_unpacker_source_pop(void *ctx, horn_cap_t *cap) {
    return horn_cap_unpacker_pop((horn_cap_unpacker_t *)ctx, cap);
}

horn_cap_source_t horn_cap_unpacker_source(horn_cap_unpacker_t *unpacker) {
    horn_cap_source_t source = { .ctx = unpacker, .pop = horn_cap_unpacker_source_pop };
    return source;
}
