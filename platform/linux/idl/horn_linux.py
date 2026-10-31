"""Private bootstrap for one hosted Linux vCPU."""

from horn_clock import *
from horn_console import *
from horn_idl import *
from horn_process import *


@capset(capacity=256)
class LinuxCaps:
    vm: Cap[sel4.Endpoint, Copy]
    address_space: Cap[sel4.Endpoint, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    reply: Cap[sel4.Reply, Move]
    ready: Cap[sel4.Notification, Copy]
    console: Cap[ConsoleObject, Copy]
    clock: Cap[sel4.Endpoint, Copy]
    events: Cap[sel4.Notification, Copy]
    user_signal: Cap[sel4.Notification, Copy]
    clock_signal: Cap[sel4.Notification, Copy]
    console_signal: Cap[sel4.Notification, Copy]
