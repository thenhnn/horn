"""Minimal child-process IPC demo.

The root task packs a ChildCaps capset into a fresh CNode, loads the child
ELF, and calls the generated Counter client.  The child serves one request
and signals the wake notification.
"""

from horn_memory import Memory, MemoryAllocator, MemoryAllocatorService, MemoryInfo, MemoryObject

from horn_idl import (
    U32,
    U64,
    Cap,
    Copy,
    Move,
    capset,
    iface,
    method,
    object,
    record,
    sel4,
)


@record
class CounterValue:
    value: U64
    generation: U32


@iface("net.nhnn.horn.demo.counter", version=(1, 0))
class Counter:
    @method
    def add(self, amount: U64) -> CounterValue: ...


@object(implements=(Counter,))
class CounterService:
    pass


@object(implements=(MemoryAllocator, Memory))
class MemoryHost:
    """Internal server dispatch; badges restrict allocation versus object access."""


@capset
class ChildCaps:
    service: Cap[CounterService, Copy]
    wake: Cap[sel4.Notification, Copy]
    reply: Cap[sel4.Reply, Move]
    tcb: Cap[sel4.TCB, Copy]
    memory: Cap[MemoryAllocatorService, Copy]
