"""Example schema for a dynamic seL4 system.

This is intentionally close to the shape of a process/device manager stack:
a service exports a small control interface, hands out a capability-backed
counter object, and exposes an SPSC console ring for bulk data.
"""

from horn_idl import (
    U8,
    U32,
    U64,
    Borrow,
    Cap,
    Copy,
    Move,
    Rights,
    iface,
    method,
    object,
    record,
    ring,
    sel4,
)


@record
class CounterValue:
    value: U64
    generation: U32


@record
class Connection:
    service: Cap[sel4.Endpoint, Copy]
    wake: Cap[sel4.Notification, Borrow]


@iface("gpos.counter", version=(1, 0))
class Counter:
    @method
    def add(self, amount: U64) -> CounterValue: ...

    @method
    def exchange(self, connection: Connection) -> Cap[sel4.Notification, Move]: ...


@iface("gpos.control", version=(1, 0))
class Control:
    @method
    def version(self) -> U32: ...


@object(implements=(Counter, Control))
class CounterService:
    pass


@ring(element=U8, capacity=4096)
class ConsoleTx:
    pass
