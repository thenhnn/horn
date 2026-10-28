"""One capability-scoped byte console. No device control, TTY or name lookup."""

from horn_idl import (
    U8,
    U32,
    U64,
    Borrow,
    Cap,
    List,
    Option,
    Result,
    enum,
    flags,
    iface,
    method,
    object,
    record,
    sel4,
)


@enum(base=U32)
class ConsoleError:
    Invalid = 1
    WouldBlock = 2
    Closed = 3
    Denied = 4
    Busy = 5


@record
class ConsoleSize:
    rows: U32
    columns: U32


@record
class ConsoleGeometry:
    size: Option[ConsoleSize]
    generation: U64


@flags(base=U32)
class ConsoleEvents:
    READABLE = 1
    WRITABLE = 2
    SIZE_CHANGED = 4
    CLOSED = 8


@record
class ConsoleStatus:
    events: ConsoleEvents
    size_generation: U64


@iface("net.nhnn.horn.console", version=(1, 0))
class ConsoleInterface:
    # Positive maximum, at most 256 bytes. Empty input while open = WouldBlock.
    @method
    def read(self, maximum: U32) -> Result[List[U8, 256], ConsoleError]: ...

    # Success acknowledges acceptance, not delivery to a physical terminal.
    @method
    def write(self, data: List[U8, 256]) -> Result[U32, ConsoleError]: ...

    # None = unknown; zero rows/columns are never a valid known size.
    @method
    def size(self) -> Result[ConsoleGeometry, ConsoleError]: ...

    @method
    def status(self) -> Result[ConsoleStatus, ConsoleError]: ...

    # One subscriber per console, replaced on rearm. One-shot wake hint.
    # Already-ready interests signal immediately. Generation closes resize races.
    # Closure always wakes an armed subscriber, regardless of interests.
    # Passing no notification cancels the subscription.
    @method
    def watch(
        self,
        notification: Option[Cap[sel4.Notification, Borrow]],
        interests: ConsoleEvents,
        size_generation: U64,
    ) -> Result[ConsoleStatus, ConsoleError]: ...


@object(implements=(ConsoleInterface,))
class ConsoleObject:
    pass
