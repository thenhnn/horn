"""Shell bootstrap only: console plus read-only observation, never control."""

from horn_console import *
from horn_device import *
from horn_idl import *
from horn_process import *


@capset(capacity=256)
class ShellCaps:
    vm: Cap[sel4.Endpoint, Copy]
    address_space: Cap[sel4.Endpoint, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
    ready: Cap[sel4.Notification, Copy]
    service: Cap[ConsoleObject, Copy]
    events: Cap[sel4.Notification, Copy]
    signal: Cap[sel4.Notification, Copy]
    pci: Cap[DeviceDiscovery, Copy]
    processes: Cap[sel4.Endpoint, Copy]
