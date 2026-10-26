"""Wire types and capability annotations for the Horn IDL.

The type objects in this module are deliberately small and immutable.  They
describe layout only; name resolution and declaration collection live in
:mod:`horn_idl.compile`.
"""

from __future__ import annotations

from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from enum import Enum, Flag, auto
from typing import Any


class CapMode(Enum):
    """How authority moves when a capability crosses an interface."""

    COPY = "copy"
    MOVE = "move"
    BORROW = "borrow"


class Rights(Flag):
    """The subset of seL4 capability rights exposed by the IDL."""

    NONE = 0
    READ = auto()
    WRITE = auto()
    GRANT = auto()
    GRANT_REPLY = auto()
    ALL = READ | WRITE | GRANT | GRANT_REPLY


def round_up(value: int, alignment: int) -> int:
    if alignment <= 1:
        return value
    return (value + alignment - 1) // alignment * alignment


def stride(type_: Type) -> int:
    """Distance between consecutive elements of ``type_`` in an array."""

    return round_up(type_.size, type_.align)


@dataclass(frozen=True)
class CapPath:
    """A deterministic path to one capability inside a message value."""

    path: str
    cap: CapType


@dataclass(frozen=True)
class DeferredType:
    """A type expression that needs the schema resolver to become concrete."""

    description: str
    resolve: Callable[[Any], Type]

    def __repr__(self) -> str:
        return self.description


class Type:
    """Base class for all wire types."""

    def is_value(self) -> bool:
        return getattr(self, "caps", 0) == 0

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        return iter(())

    def rust_type(self) -> str:
        raise NotImplementedError

    def c_type(self) -> str:
        raise NotImplementedError

    def c_name(self) -> str:
        return (
            self.c_type()
            .replace(" ", "_")
            .replace("*", "ptr")
            .replace("[", "_")
            .replace("]", "")
            .replace(";", "")
        )

    def __repr__(self) -> str:
        return getattr(self, "name", type(self).__name__)


@dataclass(frozen=True)
class Void(Type):
    name: str = "void"
    size: int = 0
    align: int = 1
    caps: int = 0

    def rust_type(self) -> str:
        return "()"

    def c_type(self) -> str:
        return "void"

    def c_name(self) -> str:
        return "void"


@dataclass(frozen=True)
class Scalar(Type):
    name: str
    size: int
    align: int
    rust: str
    c: str
    caps: int = 0
    signed: bool = False
    float: bool = False

    def rust_type(self) -> str:
        return self.rust

    def c_type(self) -> str:
        return self.c

    def c_name(self) -> str:
        return self.c.replace(" ", "_")


@dataclass(frozen=True)
class EnumType(Type):
    name: str
    base: Scalar
    variants: tuple[tuple[str, int], ...]
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = 0

    def __post_init__(self) -> None:
        object.__setattr__(self, "size", self.base.size)
        object.__setattr__(self, "align", self.base.align)

    def rust_type(self) -> str:
        return self.name

    def c_type(self) -> str:
        return f"enum {self.c_name()}"

    def c_name(self) -> str:
        return f"horn_{self.name}"


@dataclass(frozen=True)
class TaggedEnumType(Type):
    name: str
    base: Scalar
    variants: tuple[tuple[str, int, Type], ...]
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)
    payload_offset: int = field(init=False)

    def __post_init__(self) -> None:
        align = max(
            self.base.align, *(payload.align for _, _, payload in self.variants)
        )
        offset = round_up(self.base.size, align)
        object.__setattr__(self, "align", align)
        object.__setattr__(self, "payload_offset", offset)
        object.__setattr__(
            self,
            "size",
            round_up(
                offset + max(payload.size for _, _, payload in self.variants), align
            ),
        )
        object.__setattr__(
            self, "caps", max(payload.caps for _, _, payload in self.variants)
        )

    def rust_type(self) -> str:
        return self.name

    def c_type(self) -> str:
        return f"struct {self.c_name()}"

    def c_name(self) -> str:
        return f"horn_{self.name}"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        for name, _, payload in self.variants:
            yield from payload.walk_caps(f"{prefix}.{name}" if prefix else name)


@dataclass(frozen=True)
class ResultType(TaggedEnumType):
    @property
    def ok(self) -> Type:
        return self.variants[0][2]

    @property
    def error(self) -> Type:
        return self.variants[1][2]

    def rust_type(self) -> str:
        return f"Result<{self.ok.rust_type()}, {self.error.rust_type()}>"


@dataclass(frozen=True)
class FlagsType(Type):
    name: str
    base: Scalar
    variants: tuple[tuple[str, int], ...]
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = 0

    def __post_init__(self) -> None:
        object.__setattr__(self, "size", self.base.size)
        object.__setattr__(self, "align", self.base.align)

    def rust_type(self) -> str:
        return self.name

    def c_type(self) -> str:
        return f"{self.c_name()}_t"

    def c_name(self) -> str:
        return f"horn_{self.name}"


@dataclass(frozen=True)
class RecordType(Type):
    name: str
    fields: tuple[Field, ...]
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)
    offsets: tuple[int, ...] = field(init=False)

    def __post_init__(self) -> None:
        offsets: list[int] = []
        offset = 0
        align = 1
        caps = 0
        for field_ in self.fields:
            offset = round_up(offset, field_.type.align)
            offsets.append(offset)
            offset += field_.type.size
            align = max(align, field_.type.align)
            caps += field_.type.caps
        size = round_up(offset, align) if self.fields else 0
        object.__setattr__(self, "offsets", tuple(offsets))
        object.__setattr__(self, "size", size)
        object.__setattr__(self, "align", align)
        object.__setattr__(self, "caps", caps)

    def rust_type(self) -> str:
        return self.name

    def c_type(self) -> str:
        return f"struct {self.c_name()}"

    def c_name(self) -> str:
        return f"horn_{self.name}"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        for field_ in self.fields:
            path = f"{prefix}.{field_.name}" if prefix else field_.name
            yield from field_.type.walk_caps(path)


@dataclass(frozen=True)
class Field:
    name: str
    type: Type
    doc: str | None = None


@dataclass(frozen=True)
class CapType(Type):
    target: Any
    mode: CapMode
    rights: Rights = Rights.ALL
    optional: bool = False
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = 1
    name: str = field(init=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "size", 8 if self.optional else 0)
        object.__setattr__(self, "align", 8 if self.optional else 1)
        object.__setattr__(
            self,
            "name",
            f"cap<{self.target_name()},{self.mode.value}"
            f"{',optional' if self.optional else ''}>",
        )

    def target_name(self) -> str:
        return getattr(
            self.target, "fqn", getattr(self.target, "name", str(self.target))
        )

    def rust_type(self) -> str:
        name = getattr(self.target, "rust_name", None)
        if name is None and hasattr(self.target, "python_name"):
            name = f"{self.target.python_name}Tag"
        if name is None:
            name = getattr(self.target, "name", None) or self.target_name()
        if isinstance(self.target, CapSetType):
            name = f"{name}Tag"
        return f"Cap<{name}>"

    def c_type(self) -> str:
        return "horn_cap_t"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        yield CapPath(prefix, self)


@dataclass(frozen=True)
class ParcelType(Type):
    """An inline parcel.  Wire-compatible with its inner value."""

    inner: Type
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "size", self.inner.size)
        object.__setattr__(self, "align", self.inner.align)
        object.__setattr__(self, "caps", self.inner.caps)

    def rust_type(self) -> str:
        return f"Parcel<{self.inner.rust_type()}, false>"

    def c_type(self) -> str:
        return self.inner.c_type()

    def c_name(self) -> str:
        return self.inner.c_name()

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        yield from self.inner.walk_caps(prefix)


@dataclass(frozen=True)
class ListType(Type):
    element: Type
    capacity: int
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "size", 8 + self.capacity * stride(self.element))
        object.__setattr__(self, "align", max(8, self.element.align))
        object.__setattr__(self, "caps", self.capacity * self.element.caps)

    def rust_type(self) -> str:
        return f"List<{self.element.rust_type()}, {self.capacity}>"

    def c_name(self) -> str:
        return f"horn_list_{self.capacity}_{self.element.c_name()}"

    def c_type(self) -> str:
        return f"struct {self.c_name()}"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        for index in range(self.capacity):
            yield from self.element.walk_caps(f"{prefix}[{index}]")


@dataclass(frozen=True)
class ArrayType(Type):
    element: Type
    capacity: int
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)

    def __post_init__(self) -> None:
        object.__setattr__(self, "size", self.capacity * stride(self.element))
        object.__setattr__(self, "align", self.element.align)
        object.__setattr__(self, "caps", self.capacity * self.element.caps)

    def rust_type(self) -> str:
        return f"[{self.element.rust_type()}; {self.capacity}]"

    def c_name(self) -> str:
        return f"horn_array_{self.capacity}_{self.element.c_name()}"

    def c_type(self) -> str:
        return f"struct {self.c_name()}"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        for index in range(self.capacity):
            yield from self.element.walk_caps(f"{prefix}[{index}]")


@dataclass(frozen=True)
class OptionType(Type):
    inner: Type
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)

    def __post_init__(self) -> None:
        offset = round_up(8, self.inner.align)
        object.__setattr__(self, "size", offset + self.inner.size)
        object.__setattr__(self, "align", max(8, self.inner.align))
        object.__setattr__(self, "caps", self.inner.caps)

    def rust_type(self) -> str:
        return f"Option<{self.inner.rust_type()}>"

    def c_name(self) -> str:
        return f"horn_option_{self.inner.c_name()}"

    def c_type(self) -> str:
        return f"struct {self.c_name()}"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        yield from self.inner.walk_caps(f"{prefix}?")


@dataclass(frozen=True)
class TupleType(Type):
    elements: tuple[Type, ...]
    size: int = field(init=False)
    align: int = field(init=False)
    caps: int = field(init=False)

    def __post_init__(self) -> None:
        offset = 0
        align = 1
        caps = 0
        for element in self.elements:
            offset = round_up(offset, element.align)
            offset += element.size
            align = max(align, element.align)
            caps += element.caps
        size = round_up(offset, align) if self.elements else 0
        object.__setattr__(self, "size", size)
        object.__setattr__(self, "align", align)
        object.__setattr__(self, "caps", caps)

    def rust_type(self) -> str:
        if len(self.elements) == 1:
            return f"({self.elements[0].rust_type()},)"
        return "(" + ", ".join(element.rust_type() for element in self.elements) + ")"

    def c_name(self) -> str:
        return "horn_tuple_" + (
            "_".join(element.c_name() for element in self.elements) or "unit"
        )

    def c_type(self) -> str:
        return f"struct {self.c_name()}"

    def walk_caps(self, prefix: str = "") -> Iterator[CapPath]:
        for index, element in enumerate(self.elements):
            yield from element.walk_caps(f"{prefix}[{index}]")


@dataclass(frozen=True)
class CapSetType(Type):
    """A typed CNode capset.  Reached through ``Cap[CapSet]``."""

    name: str
    fields: tuple[Field, ...]
    slots: tuple[tuple[str, int], ...]
    size: int = 0
    align: int = 1
    caps: int = 0

    def rust_type(self) -> str:
        return f"{self.name}CapSet"

    def c_type(self) -> str:
        return f"horn_{self.name}_t"

    def c_name(self) -> str:
        return f"capset_{self.name}"


@dataclass(frozen=True)
class RingType(Type):
    """A declared SPSC ring layout.  A ring is reached through ``Cap[Ring]``."""

    name: str
    element: Type
    capacity: int
    size: int = 0
    align: int = 1
    caps: int = 0

    def rust_type(self) -> str:
        return f"Ring<{self.element.rust_type()}, {self.capacity}>"

    def c_type(self) -> str:
        return f"struct horn_ring_{self.name}"


@dataclass(frozen=True)
class RegionType(Type):
    """A declared shared-memory region.  Reached through ``Cap[Region]``."""

    name: str
    element: Type
    capacity: int
    size: int = 0
    align: int = 1
    caps: int = 0

    def rust_type(self) -> str:
        return f"Region<{self.element.rust_type()}, {self.capacity}>"

    def c_type(self) -> str:
        return f"struct horn_region_{self.name}"


def layout_caps(type_: Type, prefix: str = "") -> list[CapPath]:
    if isinstance(type_, TaggedEnumType):
        name, _, payload = max(type_.variants, key=lambda variant: variant[2].caps)
        return layout_caps(payload, f"{prefix}.{name}" if prefix else name)
    if isinstance(type_, RecordType):
        return [
            cap
            for field in type_.fields
            for cap in layout_caps(
                field.type, f"{prefix}.{field.name}" if prefix else field.name
            )
        ]
    if isinstance(type_, TupleType):
        return [
            cap
            for index, element in enumerate(type_.elements)
            for cap in layout_caps(element, f"{prefix}[{index}]")
        ]
    if isinstance(type_, (ArrayType, ListType)):
        return [
            cap
            for index in range(type_.capacity)
            for cap in layout_caps(type_.element, f"{prefix}[{index}]")
        ]
    if isinstance(type_, (OptionType, ParcelType)):
        return layout_caps(type_.inner, prefix)
    return list(type_.walk_caps(prefix))


def dynamic_caps(type_: Type) -> bool:
    if isinstance(type_, (TaggedEnumType, OptionType, ListType)):
        return type_.caps != 0
    if isinstance(type_, RecordType):
        return any(dynamic_caps(field.type) for field in type_.fields)
    if isinstance(type_, TupleType):
        return any(dynamic_caps(element) for element in type_.elements)
    if isinstance(type_, ArrayType):
        return dynamic_caps(type_.element)
    if isinstance(type_, ParcelType):
        return dynamic_caps(type_.inner)
    return False


U8 = Scalar("u8", 1, 1, "u8", "uint8_t")
U16 = Scalar("u16", 2, 2, "u16", "uint16_t")
U32 = Scalar("u32", 4, 4, "u32", "uint32_t")
U64 = Scalar("u64", 8, 8, "u64", "uint64_t")
I8 = Scalar("i8", 1, 1, "i8", "int8_t", signed=True)
I16 = Scalar("i16", 2, 2, "i16", "int16_t", signed=True)
I32 = Scalar("i32", 4, 4, "i32", "int32_t", signed=True)
I64 = Scalar("i64", 8, 8, "i64", "int64_t", signed=True)
BOOL = Scalar("bool", 1, 1, "bool", "bool")
F32 = Scalar("f32", 4, 4, "f32", "float", float=True)
F64 = Scalar("f64", 8, 8, "f64", "double", float=True)
WORD = Scalar("word", 8, 8, "u64", "uint64_t")
CPTR = Scalar("cptr", 8, 8, "u64", "uint64_t")

VOID = Void()
