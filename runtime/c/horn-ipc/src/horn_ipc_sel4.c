#include "horn_ipc_sel4.h"

#include <string.h>

void horn_sel4_transport_init(horn_sel4_transport_t *transport,
                              seL4_CPtr cnode,
                              seL4_Word cnode_depth,
                              seL4_CPtr recv_index,
                              seL4_Word recv_depth,
                              seL4_CPtr reply_cap) {
    transport->cnode = cnode;
    transport->cnode_depth = cnode_depth;
    transport->recv_index = recv_index;
    transport->recv_depth = recv_depth;
    transport->reply_cap = reply_cap;
}

static void horn_sel4_install_caps(horn_cap_list_t *caps) {
    if (caps == NULL) {
        return;
    }
    for (uint32_t i = 0; i < caps->len; ++i) {
        seL4_SetCap(i, caps->refs[i].slot);
    }
}

static void horn_sel4_delete_moves(horn_sel4_transport_t *transport, horn_cap_list_t *caps) {
    if (caps == NULL) {
        return;
    }
    for (uint32_t i = 0; i < caps->len; ++i) {
        if (caps->refs[i].mode == HORN_CAP_MOVE) {
            seL4_CNode_Delete(transport->cnode, caps->refs[i].slot, transport->cnode_depth);
        }
    }
}

horn_status_t horn_sel4_call(void *ctx, horn_cap_t target, uint64_t label,
                             const uint8_t *args, size_t args_len, horn_cap_list_t *caps,
                             uint8_t *result, size_t result_capacity, size_t *result_len,
                             horn_cap_list_t *result_caps) {
    horn_sel4_transport_t *transport = (horn_sel4_transport_t *)ctx;
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    if (args_len > sizeof(ipc->msg)) {
        return HORN_ERR_INVALID;
    }
    memcpy(ipc->msg, args, args_len);
    horn_sel4_install_caps(caps);
    seL4_SetCapReceivePath(transport->cnode, transport->recv_index, transport->recv_depth);
    seL4_MessageInfo_t info = seL4_MessageInfo_new(label, 0, caps ? caps->len : 0, (args_len + 7) / 8);
    seL4_MessageInfo_t reply = seL4_Call(target.slot, info);
    seL4_Word length = seL4_MessageInfo_get_length(reply);
    seL4_Word extra = seL4_MessageInfo_get_extraCaps(reply);
    if (length == 0) {
        return HORN_ERR_TRUNCATED;
    }
    uint32_t status;
    memcpy(&status, ipc->msg, sizeof(status));
    if (status != 0) {
        return HORN_ERR_TRANSPORT;
    }
    size_t payload_len = length * sizeof(seL4_Word) - sizeof(status);
    if (payload_len > result_capacity) {
        return HORN_ERR_TRUNCATED;
    }
    if (result != NULL && payload_len > 0) {
        memcpy(result, ((const uint8_t *)ipc->msg) + sizeof(status), payload_len);
    }
    if (result_len != NULL) {
        *result_len = payload_len;
    }
    if (result_caps != NULL) {
        if (extra > HORN_MAX_CAPS) {
            return HORN_ERR_TOO_MANY_CAPS;
        }
        for (seL4_Word i = 0; i < extra; ++i) {
            result_caps->refs[i].slot = seL4_GetCap(i);
            result_caps->refs[i].rights = 0xfu;
        }
        result_caps->len = extra;
        result_caps->read = 0;
    }
    horn_sel4_delete_moves(transport, caps);
    return HORN_OK;
}

horn_status_t horn_sel4_send(void *ctx, horn_cap_t target, uint64_t label,
                             const uint8_t *args, size_t args_len, horn_cap_list_t *caps) {
    horn_sel4_transport_t *transport = (horn_sel4_transport_t *)ctx;
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    if (args_len > sizeof(ipc->msg)) {
        return HORN_ERR_INVALID;
    }
    memcpy(ipc->msg, args, args_len);
    horn_sel4_install_caps(caps);
    seL4_Send(target.slot, seL4_MessageInfo_new(label, 0, caps ? caps->len : 0, (args_len + 7) / 8));
    horn_sel4_delete_moves(transport, caps);
    return HORN_OK;
}

horn_status_t horn_sel4_recv(horn_sel4_transport_t *transport,
                             seL4_CPtr endpoint,
                             uint64_t *label,
                             uint8_t *args,
                             size_t args_capacity,
                             size_t *args_len,
                             horn_cap_list_t *caps) {
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    seL4_SetCapReceivePath(transport->cnode, transport->recv_index, transport->recv_depth);
    seL4_Word badge = 0;
#ifdef CONFIG_KERNEL_MCS
    seL4_MessageInfo_t info = seL4_Recv(endpoint, &badge, transport->reply_cap);
#else
    seL4_MessageInfo_t info = seL4_Recv(endpoint, &badge);
#endif
    seL4_Word length = seL4_MessageInfo_get_length(info);
    seL4_Word extra = seL4_MessageInfo_get_extraCaps(info);
    size_t payload_len = length * sizeof(seL4_Word);
    if (payload_len > args_capacity) {
        return HORN_ERR_TRUNCATED;
    }
    memcpy(args, ipc->msg, payload_len);
    if (args_len != NULL) {
        *args_len = payload_len;
    }
    if (label != NULL) {
        *label = seL4_MessageInfo_get_label(info);
    }
    if (caps != NULL) {
        if (extra > HORN_MAX_CAPS) {
            return HORN_ERR_TOO_MANY_CAPS;
        }
        for (seL4_Word i = 0; i < extra; ++i) {
            caps->refs[i].slot = seL4_GetCap(i);
            caps->refs[i].rights = 0xfu;
        }
        caps->len = extra;
        caps->read = 0;
    }
    return HORN_OK;
}

horn_status_t horn_sel4_reply(horn_sel4_transport_t *transport,
                              uint64_t label,
                              const uint8_t *result,
                              size_t result_len,
                              horn_cap_list_t *caps) {
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    uint32_t status = 0;
    size_t total = sizeof(status) + result_len;
    if (total > sizeof(ipc->msg)) {
        return HORN_ERR_INVALID;
    }
    memcpy(ipc->msg, &status, sizeof(status));
    if (result_len > 0) {
        memcpy(((uint8_t *)ipc->msg) + sizeof(status), result, result_len);
    }
    horn_sel4_install_caps(caps);
    seL4_MessageInfo_t info = seL4_MessageInfo_new(label, 0, caps ? caps->len : 0, (total + 7) / 8);
#ifdef CONFIG_KERNEL_MCS
    seL4_Send(transport->reply_cap, info);
#else
    seL4_Reply(info);
#endif
    horn_sel4_delete_moves(transport, caps);
    return HORN_OK;
}

horn_status_t horn_sel4_reply_status(horn_sel4_transport_t *transport,
                                     uint64_t label,
                                     uint32_t status) {
    (void)transport;
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    memcpy(ipc->msg, &status, sizeof(status));
    seL4_MessageInfo_t info = seL4_MessageInfo_new(label, 0, 0, 1);
#ifdef CONFIG_KERNEL_MCS
    seL4_Send(transport->reply_cap, info);
#else
    seL4_Reply(info);
#endif
    return HORN_OK;
}

horn_status_t horn_sel4_recv_deferred(horn_sel4_transport_t *transport,
                                      seL4_CPtr endpoint,
                                      seL4_CPtr reply_cap,
                                      uint64_t *label,
                                      uint8_t *args,
                                      size_t args_capacity,
                                      size_t *args_len,
                                      horn_cap_list_t *caps,
                                      horn_sel4_reply_t *reply) {
#ifdef CONFIG_KERNEL_MCS
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    seL4_SetCapReceivePath(transport->cnode, transport->recv_index, transport->recv_depth);
    seL4_Word badge = 0;
    seL4_MessageInfo_t info = seL4_Recv(endpoint, &badge, reply_cap);
    seL4_Word length = seL4_MessageInfo_get_length(info);
    seL4_Word extra = seL4_MessageInfo_get_extraCaps(info);
    size_t payload_len = length * sizeof(seL4_Word);
    if (payload_len > args_capacity) {
        return HORN_ERR_TRUNCATED;
    }
    memcpy(args, ipc->msg, payload_len);
    if (args_len != NULL) {
        *args_len = payload_len;
    }
    if (label != NULL) {
        *label = seL4_MessageInfo_get_label(info);
    }
    if (caps != NULL) {
        if (extra > HORN_MAX_CAPS) {
            return HORN_ERR_TOO_MANY_CAPS;
        }
        for (seL4_Word i = 0; i < extra; ++i) {
            caps->refs[i].slot = seL4_GetCap(i);
            caps->refs[i].rights = 0xfu;
        }
        caps->len = extra;
        caps->read = 0;
    }
    if (reply != NULL) {
        reply->reply_cap = reply_cap;
        reply->label = seL4_MessageInfo_get_label(info);
    }
    return HORN_OK;
#else
    (void)transport;
    (void)endpoint;
    (void)reply_cap;
    (void)label;
    (void)args;
    (void)args_capacity;
    (void)args_len;
    (void)caps;
    (void)reply;
    return HORN_ERR_INVALID;
#endif
}

horn_status_t horn_sel4_reply_deferred(horn_sel4_transport_t *transport,
                                       horn_sel4_reply_t *reply,
                                       const uint8_t *result,
                                       size_t result_len,
                                       horn_cap_list_t *caps) {
#ifdef CONFIG_KERNEL_MCS
    seL4_IPCBuffer *ipc = seL4_GetIPCBuffer();
    uint32_t status = 0;
    size_t total = sizeof(status) + result_len;
    if (total > sizeof(ipc->msg)) {
        return HORN_ERR_INVALID;
    }
    memcpy(ipc->msg, &status, sizeof(status));
    if (result_len > 0) {
        memcpy(((uint8_t *)ipc->msg) + sizeof(status), result, result_len);
    }
    horn_sel4_install_caps(caps);
    seL4_Send(reply->reply_cap,
              seL4_MessageInfo_new(reply->label, 0, caps ? caps->len : 0, (total + 7) / 8));
    horn_sel4_delete_moves(transport, caps);
    return HORN_OK;
#else
    (void)transport;
    (void)reply;
    (void)result;
    (void)result_len;
    (void)caps;
    return HORN_ERR_INVALID;
#endif
}
