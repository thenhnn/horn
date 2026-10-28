from horn_idl import *
from horn_idl import sel4
from horn_vm import VmFactory, VmAddressSpaceObject
from horn_device import DeviceDiscovery


@capset(capacity=256)
class DeviceProbeCaps:
    vm: Cap[VmFactory, Copy]
    address_space: Cap[VmAddressSpaceObject, Copy]
    observer: Cap[DeviceDiscovery, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
    ready: Cap[sel4.Notification, Copy]
