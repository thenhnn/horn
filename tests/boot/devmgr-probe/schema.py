# Imported record annotations need the complete dependency namespace.
from horn_device import *
from horn_idl import *
from horn_idl import sel4


@capset(capacity=256)
class DeviceProbeCaps:
    vm: Cap[VmFactory, Copy]
    address_space: Cap[VmAddressSpaceObject, Copy]
    observer: Cap[DeviceDiscovery, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
    ready: Cap[sel4.Notification, Copy]
