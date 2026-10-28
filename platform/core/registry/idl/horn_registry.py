"""Scoped discovery of configured service interfaces; no process/device authority."""

from __future__ import annotations

from horn_idl import (
    BOOL,
    U8,
    U32,
    U64,
    Borrow,
    Cap,
    List,
    Move,
    Result,
    enum,
    iface,
    method,
    object,
    record,
    sel4,
)


@enum(base=U32)
class RegistryError:
    Denied = 1
    NotFound = 2
    Unavailable = 3
    Invalid = 4
    Exhausted = 5


@enum(base=U32)
class RegistryKind:
    Directory = 1
    Interface = 2


@record
class RegistryEntry:
    name: List[U8, 96]
    kind: RegistryKind
    available: BOOL


@record
class RegistryPage:
    entries: List[RegistryEntry, 4]
    next: U64
    generation: U64


@iface("net.nhnn.horn.registry.directory", version=(1, 0))
class RegistryDirectory:
    @method
    def list(
        self, after: U64, recursive: BOOL
    ) -> Result[RegistryPage, RegistryError]: ...

    @method
    def lookup(
        self, path: List[U8, 96]
    ) -> Result[Cap[sel4.Endpoint, Move], RegistryError]: ...

    @method
    def open(
        self, path: List[U8, 96]
    ) -> Result[Cap[RegistryNode, Move], RegistryError]: ...

    @method
    def watch(
        self, signal: Cap[sel4.Notification, Borrow], generation: U64
    ) -> Result[U64, RegistryError]: ...


@object(implements=(RegistryDirectory,))
class RegistryNode:
    pass
