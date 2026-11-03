"""Counter demo with capability-provisioned root-hosted VM endpoints."""

from horn_idl import (
    U32,
    U64,
    Cap,
    Copy,
    Move,
    capset,
    enum,
    iface,
    method,
    object,
    record,
    sel4,
)


@enum
class CounterState:
    @enum.value(0)
    def Empty(): ...

    @enum.value(1, type=Cap[sel4.Notification, Copy])
    def Ready(): ...


@record
class CounterValue:
    value: U64
    generation: U32
    state: CounterState


@iface("net.nhnn.horn.demo.counter", version=(1, 0))
class Counter:
    @method
    def add(self, amount: U64) -> CounterValue: ...


@object(implements=(Counter,))
class CounterService:
    pass


@capset(capacity=256)
class ChildCaps:
    service: Cap[CounterService, Copy]
    wake: Cap[sel4.Notification, Copy]
    reply: Cap[sel4.Reply, Move]
    # Interfaces/codecs live in horn-vm; no raw frame or VSpace authority.
    vm: Cap[sel4.Endpoint, Copy]
    address_space: Cap[sel4.Endpoint, Copy]
    lifecycle: Cap[sel4.Endpoint, Copy]
    process_factory: Cap[sel4.Endpoint, Copy]
    process_observer: Cap[sel4.Endpoint, Copy]
    elf: Cap[sel4.Endpoint, Copy]
