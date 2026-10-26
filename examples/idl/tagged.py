from horn_idl import (
    U8,
    U32,
    U64,
    Array,
    Cap,
    Copy,
    List,
    Move,
    Option,
    Result,
    Rights,
    enum,
    iface,
    method,
    record,
    sel4,
)


@record
class Payload:
    small: U8
    count: U64
    tail: U8


@enum
class Event:
    @enum.value(0)
    def Empty(): ...
    @enum.value(1, type=U32)
    def Number(): ...
    @enum.value(2, type=Payload)
    def Data(): ...
    @enum.value(3, type=Cap[sel4.Endpoint, Move, Rights.WRITE])
    def Link(): ...
    @enum.value(4, type=Cap[sel4.Notification, Copy])
    def Signal(): ...
    @enum.value(5, type=List[Payload, 2])
    def Sequence(): ...
    @enum.value(6, type=Array[Cap[sel4.Endpoint, Move], 4])
    def Many(): ...
    @enum.value(7, type=tuple[U8, Cap[sel4.Notification, Copy], Payload])
    def Tuple(): ...
    @enum.value(8, type=Option[Cap[sel4.Endpoint, Move]])
    def Maybe(): ...


@record
class Nested:
    prefix: U8
    event: Event
    suffix: U64


@record
class Outcomes:
    primary: Result[U32, Event]
    nested: Result[Result[None, Event], Event]
    optional: Option[Result[U32, Event]]
    array: Array[Result[U32, Event], 2]
    sequence: List[Result[U32, Event], 2]


@iface("net.nhnn.horn.tagged.probe")
class Probe:
    @method
    def echo(self, event: Event) -> Event: ...

    @method
    def try_echo(self, event: Event) -> Result[Event, Event]: ...

    @method
    def handoff(
        self,
    ) -> Result[Cap[sel4.Endpoint, Move], Cap[sel4.Notification, Copy]]: ...

    @method
    def check(self) -> Result[None, Event]: ...

    @method
    def outcomes(self, values: Outcomes) -> Outcomes: ...
