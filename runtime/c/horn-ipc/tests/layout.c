#include "horn_ipc.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

#define MAX_SLOTS 64

typedef struct mock_space {
    horn_cap_t slots[MAX_SLOTS][8];
    int used[MAX_SLOTS][8];
    uint64_t next;
} mock_space_t;

static horn_status_t mock_alloc(void *ctx, const horn_slot_ref_t *parent, uint8_t size_bits, horn_cap_t *out) {
    mock_space_t *space = ctx;
    (void)size_bits;
    space->next += 1;
    out->slot = space->next;
    out->mode = HORN_CAP_COPY;
    out->rights = 0xfu;
    if (parent != NULL) {
        space->slots[parent->cnode.slot][parent->slot] = *out;
        space->used[parent->cnode.slot][parent->slot] = 1;
    }
    return HORN_OK;
}

static horn_status_t mock_free(void *ctx, horn_cap_t cnode) {
    (void)ctx;
    (void)cnode;
    return HORN_OK;
}

static horn_status_t mock_copy(void *ctx, horn_cap_t cnode, uint32_t slot, horn_cap_t src, uint32_t rights) {
    mock_space_t *space = ctx;
    space->slots[cnode.slot][slot] = src;
    space->slots[cnode.slot][slot].rights = rights;
    space->used[cnode.slot][slot] = 1;
    return HORN_OK;
}

static horn_status_t mock_move(void *ctx, horn_cap_t cnode, uint32_t slot, horn_cap_t src) {
    return mock_copy(ctx, cnode, slot, src, src.rights);
}

static horn_status_t mock_at(void *ctx, horn_cap_t cnode, uint32_t slot, horn_cap_t *out) {
    mock_space_t *space = ctx;
    if (!space->used[cnode.slot][slot]) {
        return HORN_ERR_MISSING_CAP;
    }
    *out = space->slots[cnode.slot][slot];
    return HORN_OK;
}

static horn_status_t mock_delete(void *ctx, horn_cap_t cap) {
    (void)ctx;
    (void)cap;
    return HORN_OK;
}

static const horn_cap_slot_t node_slots[] = {
    { .path = "a", .slot = 0, .mode = HORN_CAP_COPY, .rights = 0xfu },
    { .path = "b", .slot = 1, .mode = HORN_CAP_MOVE, .rights = 0xfu },
    { .path = "c", .slot = 2, .mode = HORN_CAP_BORROW, .rights = 0xfu },
    { .path = "d", .slot = 3, .mode = HORN_CAP_COPY, .rights = 0xfu },
};
static const horn_cap_node_t node = { .size_bits = 2, .slots = node_slots, .slot_count = 4 };
static const horn_cap_layout_t layout = { .direct = NULL, .direct_count = 0, .cnode = &node };

int main(void) {
    mock_space_t space = {0};
    horn_cap_space_t cap_space = {
        .ctx = &space,
        .alloc_cnode = mock_alloc,
        .free_cnode = mock_free,
        .copy_into = mock_copy,
        .move_into = mock_move,
        .cap_at = mock_at,
        .delete_cap = mock_delete,
    };

    horn_cap_packer_t packer;
    horn_cap_layout_t variable = layout;
    variable.variable = true;
    horn_cap_list_t active;
    horn_cap_packer_init(&packer, &cap_space, &variable);
    assert(horn_cap_packer_finish(&packer, &active) == HORN_OK);
    assert(active.len == 0 && space.next == 0);
    horn_cap_packer_init(&packer, &cap_space, &variable);
    assert(horn_cap_packer_push(&packer, (horn_cap_t){.slot = 42, .mode = HORN_CAP_COPY}) == HORN_OK);
    assert(horn_cap_packer_finish(&packer, &active) == HORN_OK);
    assert(active.len == 1);
    horn_cap_packer_init(&packer, &cap_space, &layout);
    for (uint64_t i = 0; i < 4; ++i) {
        horn_cap_t cap = { .slot = 10 + i, .mode = i == 1 ? HORN_CAP_MOVE : HORN_CAP_COPY, .rights = 0xfu };
        assert(horn_cap_packer_push(&packer, cap) == HORN_OK);
    }
    horn_cap_list_t root_caps;
    assert(horn_cap_packer_finish(&packer, &root_caps) == HORN_OK);
    assert(root_caps.len == 1);

    horn_cap_unpacker_t unpacker;
    horn_cap_unpacker_init(&unpacker, &cap_space, &layout, root_caps);
    for (uint64_t i = 0; i < 4; ++i) {
        horn_cap_t cap;
        assert(horn_cap_unpacker_pop(&unpacker, &cap) == HORN_OK);
        assert(cap.slot == 10 + i);
    }
    horn_cap_t extra;
    assert(horn_cap_unpacker_pop(&unpacker, &extra) == HORN_ERR_MISSING_CAP);
    puts("layout ok");
    return 0;
}
