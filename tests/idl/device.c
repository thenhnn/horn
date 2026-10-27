#include "protocol.h"
#include <assert.h>
#include <string.h>

int main(void) {
    _Static_assert(PCI_DEVICE_INFO_SIZE + 8 <= HORN_MAX_MESSAGE_BYTES, "info fits IPC");
    _Static_assert(DEVICE_PAGE_SIZE + 8 <= HORN_MAX_MESSAGE_BYTES, "page fits IPC");
    horn_PciDeviceInfo_t value = {0}, decoded;
    value.device.id = UINT64_C(0x100000000);
    value.device.address.segment = 65535;
    value.device.address.bus = 255;
    value.device.address.device = 31;
    value.device.address.function = 7;
    value.device.vendor_id = 0x1af4;
    value.command = 7;
    value.msix = true;
    value.bars.len = 1;
    value.bars.data[0] = (horn_PciBar_t){.index = 0, .kind = 3, .address = UINT64_C(0x123400000000), .prefetchable = true};
    value.resizable_bars.len = 1;
    value.resizable_bars.data[0] = (horn_PciResizableBar_t){.index = 0, .supported_sizes = 0x300, .current_size = UINT64_C(0x10000000)};
    uint8_t a[PCI_DEVICE_INFO_SIZE], b[PCI_DEVICE_INFO_SIZE];
    horn_encoder_t out;
    horn_encoder_init(&out, a, sizeof(a));
    assert(horn_PciDeviceInfo_encode(&value, &out) == HORN_OK);
    horn_decoder_t in;
    horn_decoder_init(&in, a, sizeof(a), NULL);
    assert(horn_PciDeviceInfo_decode(&decoded, &in) == HORN_OK);
    assert(decoded.device.id == value.device.id);
    assert(decoded.bars.data[0].address == value.bars.data[0].address);
    assert(decoded.resizable_bars.data[0].current_size == value.resizable_bars.data[0].current_size);
    assert(decoded.msix);
    horn_encoder_init(&out, b, sizeof(b));
    assert(horn_PciDeviceInfo_encode(&decoded, &out) == HORN_OK);
    assert(memcmp(a, b, sizeof(a)) == 0);
    return 0;
}
