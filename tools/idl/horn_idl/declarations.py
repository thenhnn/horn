"""The Python DSL.

Schemas are ordinary Python modules.  Decorators only record declarations;
they never execute method bodies.  Resolution happens in :mod:`horn_idl.compile`,
which allows forward references and keeps import-time errors out of the way.
"""

from __future__ import annotations

import hashlib
from collections.abc import Callable
from dataclasses import dataclass
from enum import Enum
from typing import Any

from .types import (
    U32,
    ArrayType,
    CapMode,
    CapType,
    DeferredType,
    ListType,
    OptionType,
    ParcelType,
    ResultType,
    Rights,
    Void,
)

Copy = CapMode.COPY
Move = CapMode.MOVE
Borrow = CapMode.BORROW


class Transport(Enum):
    """How a method's messages reach the peer."""

    SYNC = "sync"
    ASYNC = "async"
    ONEWAY = "oneway"


@dataclass(frozen=True)
class MethodOptions:
    transport: Transport | None
    oneway: bool
    name: str | None
    event: bool


def method(
    func: Callable[..., Any] | None = None,
    *,
    transport: Transport | None = None,
    oneway: bool = False,
    name: str | None = None,
) -> Any:
    """Declare a two-way method (or a one-way method with ``oneway=True``)."""

    def decorate(function: Callable[..., Any]) -> Callable[..., Any]:
        function.__horn_method__ = MethodOptions(transport, oneway, name, False)
        return function

    return decorate(func) if func is not None else decorate


def event(
    func: Callable[..., Any] | None = None,
    *,
    transport: Transport | None = None,
    name: str | None = None,
) -> Any:
    """Declare a server-to-client event."""

    def decorate(function: Callable[..., Any]) -> Callable[..., Any]:
        function.__horn_method__ = MethodOptions(transport, True, name, True)
        return function

    return decorate(func) if func is not None else decorate


def iface(
    fqn: str,
    *,
    version: tuple[int, int] = (1, 0),
    transport: Transport = Transport.SYNC,
    extends: tuple[type, ...] = (),
    strict: bool = True,
) -> Callable[[type], type]:
    def decorate(cls: type) -> type:
        cls.__horn_kind__ = "interface"
        cls.__horn_fqn__ = fqn
        cls.__horn_version__ = version
        cls.__horn_transport__ = transport
        cls.__horn_extends__ = extends
        cls.__horn_strict__ = strict
        return cls

    return decorate


def object(
    *,
    fqn: str | None = None,
    implements: tuple[type, ...],
    transport: Transport = Transport.SYNC,
) -> Callable[[type], type]:
    def decorate(cls: type) -> type:
        cls.__horn_kind__ = "object"
        cls.__horn_fqn__ = fqn or cls.__name__
        cls.__horn_implements__ = implements
        cls.__horn_transport__ = transport
        return cls

    return decorate


def record(
    cls: type | None = None,
    *,
    name: str | None = None,
) -> Any:
    def decorate(klass: type) -> type:
        klass.__horn_kind__ = "record"
        klass.__horn_name__ = name or klass.__name__
        return klass

    return decorate(cls) if cls is not None else decorate


@dataclass(frozen=True)
class EnumValue:
    tag: int | None
    payload: Any


class _Enum:
    def __call__(
        self, cls: type | None = None, *, base: Any = None, name: str | None = None
    ) -> Any:
        def decorate(klass: type) -> type:
            klass.__horn_kind__ = "enum"
            klass.__horn_name__ = name or klass.__name__
            klass.__horn_base__ = base
            return klass

        return decorate(cls) if cls is not None else decorate

    def value(self, tag: int | None = None, *, type: Any = None) -> Any:
        def decorate(function: Callable[..., Any]) -> Callable[..., Any]:
            function.__horn_enum_value__ = EnumValue(tag, type)
            return function

        return decorate


enum = _Enum()


def flags(
    cls: type | None = None,
    *,
    base: Any = None,
    name: str | None = None,
) -> Any:
    def decorate(klass: type) -> type:
        klass.__horn_kind__ = "flags"
        klass.__horn_name__ = name or klass.__name__
        klass.__horn_base__ = base
        return klass

    return decorate(cls) if cls is not None else decorate


def capset(
    cls: type | None = None,
    *,
    name: str | None = None,
) -> Any:
    def decorate(klass: type) -> type:
        klass.__horn_kind__ = "capset"
        klass.__horn_name__ = name or klass.__name__
        return klass

    return decorate(cls) if cls is not None else decorate


def ring(
    cls: type | None = None,
    *,
    element: Any,
    capacity: int,
    name: str | None = None,
) -> Any:
    def decorate(klass: type) -> type:
        klass.__horn_kind__ = "ring"
        klass.__horn_name__ = name or klass.__name__
        klass.__horn_element__ = element
        klass.__horn_capacity__ = capacity
        return klass

    return decorate(cls) if cls is not None else decorate


def region(
    cls: type | None = None,
    *,
    element: Any,
    capacity: int,
    name: str | None = None,
) -> Any:
    def decorate(klass: type) -> type:
        klass.__horn_kind__ = "region"
        klass.__horn_name__ = name or klass.__name__
        klass.__horn_element__ = element
        klass.__horn_capacity__ = capacity
        return klass

    return decorate(cls) if cls is not None else decorate


class Cap:
    """``Cap[Target, Move]`` or ``Cap[Target, Copy, Rights.READ]``."""

    def __class_getitem__(cls, args: Any) -> DeferredType:
        if not isinstance(args, tuple) or len(args) < 2:
            raise TypeError("Cap requires a target and an explicit transfer mode")
        target, mode, *rest = args
        if not isinstance(mode, CapMode):
            raise TypeError("Cap mode must be Copy, Move, or Borrow")
        if len(rest) > 1:
            raise TypeError("Cap accepts at most one rights argument")
        rights = rest[0] if rest else Rights.ALL
        if not isinstance(rights, Rights):
            raise TypeError("Cap rights must be a Rights value")

        def resolve(resolver: Any) -> CapType:
            return CapType(resolver.resolve(target), mode, rights)

        return DeferredType(f"cap<{target!r},{mode.value}>", resolve)


class Parcel:
    def __class_getitem__(cls, inner: Any) -> DeferredType:
        if isinstance(inner, tuple):
            raise TypeError(
                "shared Parcel storage is not implemented yet; use an @region field"
            )

        def resolve(resolver: Any) -> ParcelType:
            return ParcelType(resolver.resolve(inner))

        return DeferredType(f"parcel<{inner!r}>", resolve)


class List:
    def __class_getitem__(cls, args: Any) -> DeferredType:
        if not isinstance(args, tuple) or len(args) != 2:
            raise TypeError("List requires an element type and a capacity")
        element, capacity = args
        if not isinstance(capacity, int) or capacity <= 0:
            raise TypeError("List capacity must be a positive integer")

        def resolve(resolver: Any) -> ListType:
            return ListType(resolver.resolve(element), capacity)

        return DeferredType(f"list<{element!r},{capacity}>", resolve)


class Array:
    def __class_getitem__(cls, args: Any) -> DeferredType:
        if not isinstance(args, tuple) or len(args) != 2:
            raise TypeError("Array requires an element type and a length")
        element, length = args
        if not isinstance(length, int) or length <= 0:
            raise TypeError("Array length must be a positive integer")

        def resolve(resolver: Any) -> ArrayType:
            return ArrayType(resolver.resolve(element), length)

        return DeferredType(f"array<{element!r},{length}>", resolve)


class Option:
    def __class_getitem__(cls, inner: Any) -> DeferredType:
        def resolve(resolver: Any) -> OptionType:
            return OptionType(resolver.resolve(inner))

        return DeferredType(f"option<{inner!r}>", resolve)


class Result:
    def __class_getitem__(cls, args: Any) -> DeferredType:
        if not isinstance(args, tuple) or len(args) != 2:
            raise TypeError("Result requires success and error types")

        def resolve(resolver: Any) -> ResultType:
            ok, error = (resolver.resolve(value) for value in args)
            for payload in (ok, error):
                if not isinstance(payload, Void):
                    resolver.validate_value_type(payload, "Result payload")
            caps = [
                (
                    cap.path,
                    cap.cap.target_name(),
                    cap.cap.mode.value,
                    cap.cap.rights.value,
                )
                for payload in (ok, error)
                for cap in payload.walk_caps()
            ]
            key = f"{ok.rust_type()}|{error.rust_type()}|{caps}"
            name = "Result_" + hashlib.sha256(key.encode()).hexdigest()[:16]
            return ResultType(name, U32, (("Ok", 0, ok), ("Err", 1, error)))

        return DeferredType(f"result<{args[0]!r},{args[1]!r}>", resolve)


class String:
    def __class_getitem__(cls, capacity: int) -> DeferredType:
        if not isinstance(capacity, int) or capacity <= 0:
            raise TypeError("String capacity must be a positive integer")

        def resolve(resolver: Any) -> ListType:
            from .types import U8

            return ListType(U8, capacity)

        return DeferredType(f"string<{capacity}>", resolve)
