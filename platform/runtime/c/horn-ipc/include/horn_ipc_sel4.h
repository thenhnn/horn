/* seL4 transport backend for horn-ipc. */
#pragma once

#include <sel4/sel4.h>

#include "horn_ipc.h"

typedef struct horn_sel4_transport {
    seL4_CPtr cnode;
    seL4_Word cnode_depth;
    seL4_CPtr recv_index;
    seL4_Word recv_depth;
    seL4_CPtr reply_cap;
} horn_sel4_transport_t;

void horn_sel4_transport_init(horn_sel4_transport_t *transport,
                              seL4_CPtr cnode,
                              seL4_Word cnode_depth,
                              seL4_CPtr recv_index,
                              seL4_Word recv_depth,
                              seL4_CPtr reply_cap);

horn_status_t horn_sel4_call(void *ctx, horn_cap_t target, uint64_t label,
                             const uint8_t *args, size_t args_len, horn_cap_list_t *caps,
                             uint8_t *result, size_t result_capacity, size_t *result_len,
                             horn_cap_list_t *result_caps);

horn_status_t horn_sel4_send(void *ctx, horn_cap_t target, uint64_t label,
                             const uint8_t *args, size_t args_len, horn_cap_list_t *caps);

horn_status_t horn_sel4_recv(horn_sel4_transport_t *transport,
                             seL4_CPtr endpoint,
                             uint64_t *label,
                             uint8_t *args,
                             size_t args_capacity,
                             size_t *args_len,
                             horn_cap_list_t *caps);

horn_status_t horn_sel4_reply(horn_sel4_transport_t *transport,
                              uint64_t label,
                              const uint8_t *result,
                              size_t result_len,
                              horn_cap_list_t *caps);

horn_status_t horn_sel4_reply_status(horn_sel4_transport_t *transport,
                                     uint64_t label,
                                     uint32_t status);

typedef struct horn_sel4_reply {
    seL4_CPtr reply_cap;
    uint64_t label;
} horn_sel4_reply_t;

horn_status_t horn_sel4_recv_deferred(horn_sel4_transport_t *transport,
                                      seL4_CPtr endpoint,
                                      seL4_CPtr reply_cap,
                                      uint64_t *label,
                                      uint8_t *args,
                                      size_t args_capacity,
                                      size_t *args_len,
                                      horn_cap_list_t *caps,
                                      horn_sel4_reply_t *reply);

horn_status_t horn_sel4_reply_deferred(horn_sel4_transport_t *transport,
                                       horn_sel4_reply_t *reply,
                                       const uint8_t *result,
                                       size_t result_len,
                                       horn_cap_list_t *caps);
