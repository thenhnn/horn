"""Monotonic time, immutable calibration page and independently granted alarms."""

from __future__ import annotations

from horn_idl import *


@enum(base=U32)
class ClockError:
    Denied = 1
    Invalid = 2
    Exhausted = 3
    Unavailable = 4


# Private hardware-backend authority; only timerd receives it.
@iface("net.nhnn.horn.clock.source", version=(1, 0))
class ClockSource:
    @method
    def source_now(self) -> Result[U64, ClockError]: ...
    @method
    def source_arm(
        self, deadline_us: U64, signal: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ClockError]: ...
    @method
    def source_cancel(self) -> Result[None, ClockError]: ...


@record
class ClockCalibration:
    flags: U64
    base_cycles: U64
    base_us: U64
    span_cycles: U64
    span_us: U64


@iface("net.nhnn.horn.clock", version=(1, 0))
class ClockInterface:
    @method
    def now(self) -> Result[U64, ClockError]: ...
    # Immutable calibration; clients materialize a sealed read-only local page.
    @method
    def calibration(self) -> Result[ClockCalibration, ClockError]: ...
    @method
    def alarm(self) -> Result[Cap[AlarmObject, Move], ClockError]: ...


@iface("net.nhnn.horn.clock.alarm", version=(1, 0))
class AlarmInterface:
    @method
    def arm(
        self, deadline_us: U64, signal: Cap[sel4.Notification, Borrow]
    ) -> Result[None, ClockError]: ...
    @method
    def cancel(self) -> Result[None, ClockError]: ...


@object(implements=(AlarmInterface,))
class AlarmObject:
    pass


@object(implements=(ClockInterface, AlarmInterface))
class ClockHost:
    pass
