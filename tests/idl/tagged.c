#include "protocol.h"
#include <assert.h>
#include <string.h>

struct caps { horn_cap_t values[8]; size_t count, read; };
static horn_status_t push(void *ctx, horn_cap_t cap) {
    struct caps *caps = ctx;
    assert(caps->count < 8);
    caps->values[caps->count++] = cap;
    return HORN_OK;
}
static horn_status_t pop(void *ctx, horn_cap_t *cap) {
    struct caps *caps = ctx;
    if (caps->read == caps->count) return HORN_ERR_MISSING_CAP;
    *cap = caps->values[caps->read++];
    return HORN_OK;
}
static void roundtrip(horn_Event_t value, size_t count) {
    uint8_t a[EVENT_SIZE], b[EVENT_SIZE];
    memset(a, 0xff, sizeof(a));
    memset(b, 0xff, sizeof(b));
    horn_encoder_t encoder;
    horn_encoder_init(&encoder, a, sizeof(a));
    assert(horn_Event_encode(&value, &encoder) == HORN_OK);
    struct caps caps = {0};
    horn_cap_sink_t sink = {.ctx = &caps, .push = push};
    assert(horn_Event_caps(&value, &sink) == HORN_OK);
    assert(caps.count == count);
    horn_cap_source_t source = {.ctx = &caps, .pop = pop};
    horn_decoder_t decoder;
    horn_decoder_init(&decoder, a, sizeof(a), &source);
    horn_Event_t decoded;
    assert(horn_Event_decode(&decoded, &decoder) == HORN_OK);
    assert(caps.read == count);
    horn_encoder_init(&encoder, b, sizeof(b));
    assert(horn_Event_encode(&decoded, &encoder) == HORN_OK);
    assert(memcmp(a, b, sizeof(a)) == 0);
}
struct reply { bool remote, fail; };
static horn_status_t call(void *ctx, horn_cap_t target, uint64_t label,
                          const uint8_t *args, size_t args_len, horn_cap_list_t *caps,
                          uint8_t *result, size_t capacity, size_t *len, horn_cap_list_t *result_caps) {
    (void)target; (void)args; (void)args_len; (void)caps;
    struct reply *reply = ctx;
    if (reply->fail) return HORN_ERR_TRANSPORT;
    memset(result, 0, capacity);
    horn_encoder_t out;
    horn_encoder_init(&out, result, capacity);
    horn_encoder_u32(&out, reply->remote ? HORN_RESULT_ERR : HORN_RESULT_OK);
    if (label == PROBE_HANDOFF_LABEL) {
        result_caps->len = 1;
        result_caps->refs[0] = (horn_cap_t){.slot = 42, .mode = HORN_CAP_MOVE, .rights = 0xfu};
    } else {
        horn_encoder_align(&out, 8);
        if (label == PROBE_CHECK_LABEL && !reply->remote) {
            horn_encoder_skip(&out, EVENT_SIZE);
        } else {
            horn_Event_t event = {.tag = HORN_EVENT_NUMBER, .value.number = 7};
            assert(horn_Event_encode(&event, &out) == HORN_OK);
        }
    }
    *len = out.pos;
    return HORN_OK;
}

static void result_clients(void) {
    struct reply reply = {0};
    horn_transport_t transport = {.call = call};
    horn_cap_t target = {0};
    horn_probe_try_echo_result_t value;
    assert(horn_probe_try_echo(&transport, &reply, target, (horn_Event_t){.tag = HORN_EVENT_EMPTY}, &value) == HORN_OK);
    assert(value.tag == HORN_RESULT_OK && value.value.ok.value.number == 7);
    reply.remote = true;
    assert(horn_probe_try_echo(&transport, &reply, target, (horn_Event_t){.tag = HORN_EVENT_EMPTY}, &value) == HORN_OK);
    assert(value.tag == HORN_RESULT_ERR && value.value.err.value.number == 7);
    horn_probe_handoff_result_t handoff;
    assert(horn_probe_handoff(&transport, &reply, target, &handoff) == HORN_OK);
    assert(handoff.tag == HORN_RESULT_ERR && handoff.value.err.mode == HORN_CAP_COPY);
    reply.remote = false;
    assert(horn_probe_handoff(&transport, &reply, target, &handoff) == HORN_OK);
    assert(handoff.tag == HORN_RESULT_OK && handoff.value.ok.mode == HORN_CAP_MOVE);
    horn_probe_check_result_t checked;
    assert(horn_probe_check(&transport, &reply, target, &checked) == HORN_OK);
    assert(checked.tag == HORN_RESULT_OK);
    reply.fail = true;
    assert(horn_probe_check(&transport, &reply, target, &checked) == HORN_ERR_TRANSPORT);
}

int main(void) {
    result_clients();
    horn_Payload_t payload = {.small = 3, .count = UINT64_C(0x1122334455667788), .tail = 9};
    roundtrip((horn_Event_t){.tag = HORN_EVENT_EMPTY}, 0);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_NUMBER, .value.number = 19}, 0);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_DATA, .value.data = payload}, 0);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_LINK, .value.link = {.slot = 42}}, 1);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_SIGNAL, .value.signal = {.slot = 43}}, 1);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_SEQUENCE, .value.sequence = {.len = 1, .data = {payload}}}, 0);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_MANY, .value.many = {{.slot = 1}, {.slot = 2}, {.slot = 3}, {.slot = 4}}}, 4);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_TUPLE, .value.tuple = {.f0 = 7, .f1 = {.slot = 4}, .f2 = payload}}, 1);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_MAYBE}, 0);
    roundtrip((horn_Event_t){.tag = HORN_EVENT_MAYBE, .value.maybe = {.present = 1, .value = {.slot = 2}}}, 1);
    uint8_t bytes[EVENT_SIZE] = {0};
    horn_decoder_t decoder;
    horn_Event_t decoded;
    bytes[0] = 255;
    horn_decoder_init(&decoder, bytes, sizeof(bytes), NULL);
    assert(horn_Event_decode(&decoded, &decoder) == HORN_ERR_INVALID);
    bytes[0] = 0; bytes[sizeof(bytes) - 1] = 1;
    horn_decoder_init(&decoder, bytes, sizeof(bytes), NULL);
    assert(horn_Event_decode(&decoded, &decoder) == HORN_ERR_INVALID);
    horn_decoder_init(&decoder, bytes, sizeof(bytes) - 1, NULL);
    assert(horn_Event_decode(&decoded, &decoder) == HORN_ERR_TRUNCATED);
    return 0;
}
