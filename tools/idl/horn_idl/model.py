"""Resolved schema model produced by :mod:`horn_idl.compile`."""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass, field

from .declarations import Transport
from .layout import CapLayout, build_layout
from .types import (
    CapPath,
    CapSetType,
    EnumType,
    Field,
    FlagsType,
    RecordType,
    RegionType,
    RingType,
    TaggedEnumType,
    Type,
    dynamic_caps,
    layout_caps,
)


@dataclass
class Method:
    name: str
    params: list[Field]
    result: Type | None
    transport: Transport
    oneway: bool
    event: bool
    interface: Interface | None = None
    label: int | None = None
    doc: str | None = None

    def request_caps(self) -> list[CapPath]:
        out: list[CapPath] = []
        for param in self.params:
            out.extend(param.type.walk_caps(param.name))
        return out

    def result_caps(self) -> list[CapPath]:
        if self.result is None:
            return []
        return list(self.result.walk_caps("result"))

    def request_layout(self) -> CapLayout:
        caps = [
            cap for param in self.params for cap in layout_caps(param.type, param.name)
        ]
        return build_layout(
            caps, variable=any(dynamic_caps(param.type) for param in self.params)
        )

    def result_layout(self) -> CapLayout:
        return (
            build_layout(
                layout_caps(self.result, "result"), variable=dynamic_caps(self.result)
            )
            if self.result is not None
            else build_layout([])
        )


@dataclass
class Interface:
    fqn: str
    version: tuple[int, int]
    python_name: str
    methods: list[Method] = field(default_factory=list)
    extends: list[Interface] = field(default_factory=list)
    transport: Transport = Transport.SYNC
    strict: bool = True
    doc: str | None = None

    def all_methods(self) -> Iterator[Method]:
        seen: set[str] = set()
        for base in self.extends:
            for method in base.all_methods():
                if method.name not in seen:
                    seen.add(method.name)
                    yield method
        for method in self.methods:
            if method.name not in seen:
                seen.add(method.name)
                yield method


@dataclass
class Object:
    fqn: str
    python_name: str
    implements: list[Interface]
    transport: Transport = Transport.SYNC
    doc: str | None = None

    def methods(self) -> Iterator[Method]:
        seen: set[int] = set()
        for interface in self.implements:
            for method in interface.all_methods():
                if id(method) not in seen:
                    seen.add(id(method))
                    yield method


@dataclass
class Record:
    name: str
    python_name: str
    type: RecordType
    doc: str | None = None


@dataclass
class EnumDecl:
    name: str
    python_name: str
    base: Type
    variants: list[tuple[str, int]]
    type: EnumType | TaggedEnumType
    doc: str | None = None


@dataclass
class FlagsDecl:
    name: str
    python_name: str
    base: Type
    variants: list[tuple[str, int]]
    type: FlagsType
    doc: str | None = None


@dataclass
class CapSet:
    name: str
    python_name: str
    type: CapSetType
    doc: str | None = None


@dataclass
class RingDecl:
    name: str
    python_name: str
    type: RingType
    doc: str | None = None


@dataclass
class RegionDecl:
    name: str
    python_name: str
    type: RegionType
    doc: str | None = None


@dataclass
class Schema:
    module: str
    records: list[Record] = field(default_factory=list)
    enums: list[EnumDecl] = field(default_factory=list)
    flags: list[FlagsDecl] = field(default_factory=list)
    interfaces: list[Interface] = field(default_factory=list)
    objects: list[Object] = field(default_factory=list)
    capsets: list[CapSet] = field(default_factory=list)
    rings: list[RingDecl] = field(default_factory=list)
    regions: list[RegionDecl] = field(default_factory=list)

    def methods(self) -> Iterator[tuple[Interface, Method]]:
        for interface in self.interfaces:
            for method in interface.all_methods():
                yield interface, method

    def labels(self) -> dict[int, tuple[Interface, Method]]:
        return {
            method.label: (interface, method)
            for interface, method in self.methods()
            if method.label is not None
        }
