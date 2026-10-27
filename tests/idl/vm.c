#include "protocol.h"
#include <assert.h>
#include <string.h>

struct caps { horn_cap_t values[16]; size_t count, read; };
static horn_status_t push(void *ctx, horn_cap_t cap) {
    struct caps *caps = ctx;
    assert(caps->count < 16);
    caps->values[caps->count++] = cap;
    return HORN_OK;
}
static horn_status_t pop(void *ctx, horn_cap_t *cap) {
    struct caps *caps = ctx;
    if (caps->read == caps->count) return HORN_ERR_MISSING_CAP;
    *cap = caps->values[caps->read++];
    return HORN_OK;
}
static void roundtrip(horn_VmregionApplyArgs_t value, size_t count) {
    uint8_t a[VMREGION_APPLY_ARGS_SIZE], b[VMREGION_APPLY_ARGS_SIZE];
    _Static_assert(sizeof(a) <= HORN_MAX_MESSAGE_BYTES, "batch fits IPC");
    horn_encoder_t out;
    horn_encoder_init(&out, a, sizeof(a));
    assert(horn_VmregionApplyArgs_encode(&value, &out) == HORN_OK);
    struct caps caps = {0};
    horn_cap_sink_t sink = {.ctx = &caps, .push = push};
    assert(horn_VmregionApplyArgs_caps(&value, &sink) == HORN_OK);
    assert(caps.count == count);
    for (size_t i = 0; i < count; i++) assert(caps.values[i].mode == HORN_CAP_BORROW);
    horn_cap_source_t source = {.ctx = &caps, .pop = pop};
    horn_decoder_t in;
    horn_decoder_init(&in, a, sizeof(a), &source);
    horn_VmregionApplyArgs_t decoded;
    assert(horn_VmregionApplyArgs_decode(&decoded, &in) == HORN_OK);
    assert(caps.read == count);
    horn_encoder_init(&out, b, sizeof(b));
    assert(horn_VmregionApplyArgs_encode(&decoded, &out) == HORN_OK);
    assert(memcmp(a, b, sizeof(a)) == 0);
}
int main(void) {
    for (size_t len = 0; len <= 16; len++) {
        horn_VmregionApplyArgs_t value = {0};
        value.updates.len = len;
        size_t count = 0;
        for (size_t i = 0; i < len; i++) {
            horn_VmUpdate_t *op = &value.updates.data[i];
            op->tag = i % 4;
            switch (op->tag) {
            case HORN_VM_UPDATE_PREPARE:
                op->value.prepare = (horn_VmRange_t){i * 4096, 4096};
                break;
            case HORN_VM_UPDATE_MAP:
                op->value.map = (horn_VmMap_t){
                    .offset = i * 4096, .length = 4096,
                    .protection = HORN_VM_PROTECTION_READ,
                };
                count = 1;
                break;
            case HORN_VM_UPDATE_PROTECT:
                op->value.protect = (horn_VmProtect_t){i * 4096, 4096, HORN_VM_PROTECTION_READ};
                break;
            case HORN_VM_UPDATE_UNMAP:
                op->value.unmap = (horn_VmRange_t){i * 4096, 4096};
                break;
            }
        }
        value.memory.present = count;
        value.memory.value = (horn_cap_t){.slot = 1, .mode = HORN_CAP_BORROW, .rights = 15};
        roundtrip(value, count);
    }
    horn_VmregionApplyArgs_t full = {.updates.len = 16};
    for (size_t i = 0; i < 16; i++) {
        full.updates.data[i] = (horn_VmUpdate_t){
            .tag = HORN_VM_UPDATE_MAP,
            .value.map = {
                .offset = i * 4096, .length = 4096,
                .protection = HORN_VM_PROTECTION_READ,
            },
        };
    }
    full.memory.present = 1;
    full.memory.value = (horn_cap_t){.slot = 1, .mode = HORN_CAP_BORROW, .rights = 15};
    roundtrip(full, 1);
    horn_vm_region_apply_result_t error = {
        .tag = HORN_RESULT_ERR,
        .value.err = {.completed = 3, .error = HORN_VM_ERROR_UNSUPPORTED},
    };
    assert(error.value.err.completed == 3);
    return 0;
}
