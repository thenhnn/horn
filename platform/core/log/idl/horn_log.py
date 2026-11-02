"""Synchronous whole-record logging. Success means output backend completed."""

from __future__ import annotations

from horn_idl import *


@enum(base=U32)
class LogLevel:
    Error = 1
    Warn = 2
    Info = 3
    Debug = 4
    Trace = 5


@enum(base=U32)
class LogError:
    Invalid = 1
    Output = 2
    Denied = 3
    Exhausted = 4


@record
class LogRecord:
    level: LogLevel
    target: List[U8, 96]
    message: List[U8, 768]


@iface("net.nhnn.horn.log", version=(1, 0))
class LogSink:
    @method
    def write(self, record: LogRecord) -> Result[None, LogError]: ...


@object(implements=(LogSink,))
class LogSinkObject:
    pass


@iface("net.nhnn.horn.log.factory", version=(1, 0))
class LogFactory:
    @method
    def create_sink(
        self, name: List[U8, 32]
    ) -> Result[Cap[LogSinkObject, Move], LogError]: ...


@object(implements=(LogSink, LogFactory))
class LogHost:
    pass
