"""Bounded Ethernet transport, TCP listeners and independently scoped connections."""

from __future__ import annotations

from horn_idl import *


@enum(base=U32)
class NetError:
    Invalid = 1
    WouldBlock = 2
    Closed = 3
    Denied = 4
    Exhausted = 5


@record
class EthernetInfo:
    mac: List[U8, 6]
    mtu: U32
    generation: U64


@record
class FrameChunk:
    data: List[U8, 512]
    remaining: U32


@iface("net.nhnn.horn.net.ethernet", version=(1, 0))
class EthernetInterface:
    @method
    def info(self) -> Result[EthernetInfo, NetError]: ...
    @method
    def receive(self) -> Result[FrameChunk, NetError]: ...
    @method
    def begin(self, length: U32) -> Result[None, NetError]: ...
    @method
    def append(self, data: List[U8, 512]) -> Result[None, NetError]: ...
    @method
    def commit(self) -> Result[None, NetError]: ...
    @method
    def watch(
        self, signal: Cap[sel4.Notification, Borrow], generation: U64
    ) -> Result[None, NetError]: ...


@object(implements=(EthernetInterface,))
class EthernetObject:
    pass


@flags(base=U32)
class TcpEvents:
    READABLE = 1
    WRITABLE = 2
    ACCEPT = 4
    CLOSED = 8


@iface("net.nhnn.horn.net.tcp.factory", version=(1, 0))
class TcpFactory:
    @method
    def listen(self, port: U32) -> Result[Cap[TcpServerObject, Move], NetError]: ...


@iface("net.nhnn.horn.net.tcp.server", version=(2, 0))
class TcpServer:
    @method
    def accept(self) -> Result[Cap[TcpConnectionObject, Move], NetError]: ...
    @method
    def watch(
        self, signal: Cap[sel4.Notification, Borrow], interests: TcpEvents
    ) -> Result[None, NetError]: ...


@iface("net.nhnn.horn.net.tcp.connection", version=(1, 0))
class TcpConnection:
    @method
    def read(self) -> Result[List[U8, 256], NetError]: ...
    @method
    def write(self, data: List[U8, 256]) -> Result[U32, NetError]: ...
    # Retire this capability now; queued bytes/FIN drain in netd independently.
    # Delete the local cap after success and never call it again.
    @method
    def finish(self) -> Result[None, NetError]: ...
    # One-shot readiness; rechecks state on arm, so no missed wakeup.
    @method
    def watch(
        self, signal: Cap[sel4.Notification, Borrow], interests: TcpEvents
    ) -> Result[None, NetError]: ...


@object(implements=(TcpServer,))
class TcpServerObject:
    pass


@object(implements=(TcpConnection,))
class TcpConnectionObject:
    pass


@object(implements=(TcpFactory, TcpServer, TcpConnection))
class TcpHost:
    pass
