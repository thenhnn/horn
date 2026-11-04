"""Schema collection, validation, and lowering."""

from __future__ import annotations

import hashlib
import importlib.util
import inspect
import sys
import types as pytypes
import typing
from enum import Enum as PyEnum
from pathlib import Path
from typing import Any, get_args, get_origin

from . import sel4
from .declarations import MethodOptions, Transport
from .layout import MAX_CAPSET_CAPS, MAX_LAYOUT_CAPS
from .model import (
    CapSet,
    EnumDecl,
    FlagsDecl,
    Interface,
    Method,
    Object,
    Record,
    RegionDecl,
    RingDecl,
    Schema,
)
from .types import (
    U32,
    VOID,
    ArrayType,
    CapMode,
    CapSetType,
    CapType,
    DeferredType,
    EnumType,
    Field,
    FlagsType,
    ListType,
    OptionType,
    ParcelType,
    RecordType,
    RegionType,
    RingType,
    Scalar,
    TaggedEnumType,
    TupleType,
    Type,
    Void,
)

MAX_CAPS = 3
MAX_MESSAGE_WORDS = 120
MAX_MESSAGE_BYTES = MAX_MESSAGE_WORDS * 8


def _contains_record(type_: Type) -> bool:
    if isinstance(type_, (RecordType, TaggedEnumType)):
        return True
    if isinstance(type_, (ArrayType, ListType)):
        return _contains_record(type_.element)
    if isinstance(type_, OptionType):
        return _contains_record(type_.inner)
    if isinstance(type_, TupleType):
        return any(_contains_record(element) for element in type_.elements)
    if isinstance(type_, ParcelType):
        return _contains_record(type_.inner)
    return False


class DeclarationError(Exception):
    """A schema is not well formed."""


LABEL_BITS = 52


def hash_label(interface: str, method: str, version: tuple[int, int]) -> int:
    """Stable 64-bit method label.

    Hash-derived labels keep schemas self-contained.  Renaming a method is an
    ABI change by design; the generator checks for collisions within a schema.
    """

    material = f"horn-idl\x00{interface}::{method}@{version[0]}.{version[1]}".encode()
    value = int.from_bytes(hashlib.blake2b(material, digest_size=8).digest(), "little")
    value &= (1 << LABEL_BITS) - 1
    if value == 0:
        material += b"\x00"
        value = int.from_bytes(
            hashlib.blake2b(material, digest_size=8).digest(), "little"
        )
        value &= (1 << LABEL_BITS) - 1
    return value


def _annotations(obj: Any, globalns: dict[str, Any]) -> dict[str, Any]:
    try:
        annotations = inspect.get_annotations(obj, eval_str=False)
    except TypeError:
        annotations = getattr(obj, "__annotations__", {})
    resolved: dict[str, Any] = {}
    for name, value in annotations.items():
        if isinstance(value, str):
            try:
                value = eval(value, globalns)
            except Exception as error:
                raise DeclarationError(
                    f"cannot resolve annotation {value!r}: {error}"
                ) from error
        resolved[name] = value
    return resolved


def _is_none(value: Any) -> bool:
    return value is None or value is type(None)


class _Resolver:
    def __init__(self, module: pytypes.ModuleType) -> None:
        self.module = module
        self.globalns = vars(module)
        self.classes: dict[type, Any] = {}
        self.records: dict[type, Record] = {}
        self.record_types: dict[type, RecordType] = {}
        self.enums: dict[type, EnumDecl] = {}
        self.flags: dict[type, FlagsDecl] = {}
        self.capsets: dict[type, CapSet] = {}
        self.rings: dict[type, RingDecl] = {}
        self.regions: dict[type, RegionDecl] = {}
        self.interfaces: dict[type, Interface] = {}
        self.objects: dict[type, Object] = {}
        self.record_stack: list[type] = []
        self.interface_stack: list[type] = []
        self.enum_stack: list[type] = []
        self.capset_stack: list[type] = []

    # -- declaration collection -------------------------------------------------

    def collect(self) -> None:
        for value in self.globalns.values():
            if not isinstance(value, type):
                continue
            kind = getattr(value, "__horn_kind__", None)
            if kind is not None:
                self.classes[value] = kind

        # Create placeholders first so forward references are possible.
        for cls, kind in self.classes.items():
            name = getattr(cls, "__horn_name__", cls.__name__)
            if kind == "record":
                placeholder = RecordType(name, ())
                self.records[cls] = Record(
                    name, cls.__name__, placeholder, inspect.getdoc(cls)
                )
                self.record_types[cls] = placeholder
            elif kind == "interface":
                self.interfaces[cls] = Interface(
                    fqn=cls.__horn_fqn__,
                    version=cls.__horn_version__,
                    python_name=cls.__name__,
                    transport=cls.__horn_transport__,
                    strict=cls.__horn_strict__,
                    doc=inspect.getdoc(cls),
                )
            elif kind == "object":
                self.objects[cls] = Object(
                    fqn=cls.__horn_fqn__,
                    python_name=cls.__name__,
                    implements=[],
                    transport=cls.__horn_transport__,
                    doc=inspect.getdoc(cls),
                )
            elif kind == "capset":
                self.capsets[cls] = CapSet(
                    name=getattr(cls, "__horn_name__", cls.__name__),
                    python_name=cls.__name__,
                    type=None,  # type: ignore[arg-type]
                    doc=inspect.getdoc(cls),
                )

        for cls, kind in self.classes.items():
            if kind == "enum":
                self._build_enum(cls, flags=False)
            elif kind == "flags":
                self._build_enum(cls, flags=True)
            elif kind == "ring":
                self._build_ring(cls, region=False)
            elif kind == "region":
                self._build_ring(cls, region=True)

        for cls in self.classes:
            if getattr(cls, "__horn_kind__", None) == "record":
                self.finalize_record(cls)

        for cls in self.classes:
            if getattr(cls, "__horn_kind__", None) == "capset":
                self.finalize_capset(cls)

        for cls in self.classes:
            if getattr(cls, "__horn_kind__", None) == "interface":
                self.finalize_interface(cls)

        for cls in self.classes:
            if getattr(cls, "__horn_kind__", None) == "object":
                self.finalize_object(cls)

    # -- type resolution --------------------------------------------------------

    def resolve(self, value: Any) -> Any:
        if isinstance(value, DeferredType):
            return value.resolve(self)
        if isinstance(value, Type):
            return value
        if _is_none(value):
            return VOID
        if value is Any:
            raise DeclarationError("Any is not a wire type")
        if isinstance(value, sel4.KernelResource):
            return value
        if isinstance(value, (Interface, Object, RingType, RegionType)):
            return value

        origin = get_origin(value)
        args = get_args(value)
        if origin is tuple or value is tuple:
            return TupleType(tuple(self.resolve(arg) for arg in args))
        if origin is list:
            raise DeclarationError("unbounded list is not a wire type; use List[T, N]")
        if origin is typing.Union:
            non_none = [arg for arg in args if arg is not type(None)]
            if len(non_none) == 1 and len(non_none) != len(args):
                return OptionType(self.resolve(non_none[0]))
            raise DeclarationError("only Optional[T] unions are supported")
        if origin is typing.Annotated:
            return self.resolve(args[0])

        if isinstance(value, type):
            kind = getattr(value, "__horn_kind__", None)
            if kind == "record":
                return self.finalize_record(value)
            if kind == "enum":
                if value not in self.enums:
                    self._build_enum(value, flags=False)
                return self.enums[value].type
            if kind == "flags":
                return self.flags[value].type
            if kind == "capset":
                return self.capsets[value].type
            if kind == "ring":
                return self.rings[value].type
            if kind == "region":
                return self.regions[value].type
            if kind == "interface":
                return self.interfaces[value]
            if kind == "object":
                return self.objects[value]
            if value in self.classes:
                return self.classes[value]

        raise DeclarationError(f"unsupported wire type: {value!r}")

    # -- records ----------------------------------------------------------------

    def finalize_record(self, cls: type) -> RecordType:
        existing = self.record_types.get(cls)
        if existing is not None and existing.fields:
            return existing
        if cls in self.record_stack:
            chain = " -> ".join(item.__name__ for item in [*self.record_stack, cls])
            raise DeclarationError(f"recursive record: {chain}")
        self.record_stack.append(cls)
        try:
            annotations = _annotations(cls, self.globalns)
            fields: list[Field] = []
            for name, annotation in annotations.items():
                if name.startswith("_"):
                    continue
                type_ = self.resolve(annotation)
                self.validate_value_type(type_, f"{cls.__name__}.{name}")
                fields.append(Field(name, type_))
            record_type = RecordType(
                getattr(cls, "__horn_name__", cls.__name__), tuple(fields)
            )
            self.record_types[cls] = record_type
            self.records[cls].type = record_type
            return record_type
        finally:
            self.record_stack.pop()

    # -- enums and flags --------------------------------------------------------

    def _build_enum(self, cls: type, *, flags: bool) -> None:
        if not flags and any(
            hasattr(value, "__horn_enum_value__") for value in vars(cls).values()
        ):
            self._build_tagged_enum(cls)
            return
        base = getattr(cls, "__horn_base__", None) or U32
        base_type = self.resolve(base)
        if not isinstance(base_type, Scalar):
            raise DeclarationError(f"{cls.__name__}: enum base must be a scalar")
        variants: list[tuple[str, int]] = []
        if issubclass(cls, PyEnum):
            for name, member in cls.__members__.items():
                variants.append((name, int(member.value)))
        else:
            for name, value in cls.__dict__.items():
                if name.startswith("_") or callable(value):
                    continue
                if isinstance(value, int):
                    variants.append((name, value))
        if not variants:
            raise DeclarationError(f"{cls.__name__}: no variants")
        if not flags:
            seen: dict[int, str] = {}
            for name, value in variants:
                if value in seen:
                    raise DeclarationError(
                        f"{cls.__name__}: duplicate value {value} for {seen[value]} and {name}"
                    )
                seen[value] = name
        name = getattr(cls, "__horn_name__", cls.__name__)
        type_ = (
            FlagsType(name, base_type, tuple(variants))
            if flags
            else EnumType(name, base_type, tuple(variants))
        )
        if flags:
            self.flags[cls] = FlagsDecl(
                name, cls.__name__, base_type, variants, type_, inspect.getdoc(cls)
            )
        else:
            self.enums[cls] = EnumDecl(
                name, cls.__name__, base_type, variants, type_, inspect.getdoc(cls)
            )

    def _build_tagged_enum(self, cls: type) -> None:
        if cls in self.enums:
            return
        if cls in self.enum_stack:
            raise DeclarationError(f"{cls.__name__}: recursive tagged enum")
        self.enum_stack.append(cls)
        try:
            base = self.resolve(getattr(cls, "__horn_base__", None) or U32)
            if not isinstance(base, Scalar) or base.name not in (
                "u8",
                "u16",
                "u32",
                "u64",
            ):
                raise DeclarationError(
                    f"{cls.__name__}: tagged enum base must be an unsigned integer"
                )
            variants, seen, next_tag = [], set(), 0
            for name, value in vars(cls).items():
                if name.startswith("_"):
                    continue
                spec = getattr(value, "__horn_enum_value__", None)
                if spec is None and not isinstance(value, int):
                    continue
                if spec is not None and inspect.signature(value).parameters:
                    raise DeclarationError(
                        f"{cls.__name__}.{name}: variant functions take no arguments"
                    )
                tag = (
                    (next_tag if spec.tag is None else spec.tag)
                    if spec is not None
                    else value
                )
                if type(tag) is not int or not 0 <= tag < 1 << (base.size * 8):
                    raise DeclarationError(f"{cls.__name__}.{name}: tag out of range")
                if tag in seen:
                    raise DeclarationError(f"{cls.__name__}: duplicate tag {tag}")
                seen.add(tag)
                next_tag = tag + 1
                annotation = spec.payload if spec is not None else None
                if isinstance(annotation, str):
                    try:
                        annotation = eval(annotation, self.globalns)
                    except (NameError, SyntaxError) as error:
                        raise DeclarationError(
                            f"{cls.__name__}.{name}: invalid payload type"
                        ) from error
                payload = self.resolve(annotation)
                if not isinstance(payload, Void):
                    self.validate_value_type(payload, f"{cls.__name__}.{name}")
                variants.append((name, tag, payload))
            if not variants:
                raise DeclarationError(f"{cls.__name__}: no variants")
            name = getattr(cls, "__horn_name__", cls.__name__)
            type_ = TaggedEnumType(name, base, tuple(variants))
            self.enums[cls] = EnumDecl(
                name,
                cls.__name__,
                base,
                [(name, tag) for name, tag, _ in variants],
                type_,
                inspect.getdoc(cls),
            )
        finally:
            self.enum_stack.pop()

    # -- rings and regions ------------------------------------------------------

    def _build_ring(self, cls: type, *, region: bool) -> None:
        name = getattr(cls, "__horn_name__", cls.__name__)
        capacity = getattr(cls, "__horn_capacity__", 0)
        if not isinstance(capacity, int) or capacity <= 0:
            raise DeclarationError(f"{name}: capacity must be positive")
        if capacity & (capacity - 1):
            raise DeclarationError(f"{name}: capacity must be a power of two")
        element = self.resolve(cls.__horn_element__)
        self.validate_value_type(element, f"{name}.element")
        if element.caps:
            raise DeclarationError(
                f"{name}: ring/region elements cannot contain capabilities"
            )
        if isinstance(element, Void) or element.size == 0:
            raise DeclarationError(f"{name}: element has no wire representation")
        if _contains_record(element):
            raise DeclarationError(
                f"{name}: ring/region elements cannot be records yet "
                "(generated records are not repr(C))"
            )
        type_ = (
            RegionType(name, element, capacity)
            if region
            else RingType(name, element, capacity)
        )
        if region:
            self.regions[cls] = RegionDecl(
                name, cls.__name__, type_, inspect.getdoc(cls)
            )
        else:
            self.rings[cls] = RingDecl(name, cls.__name__, type_, inspect.getdoc(cls))

    # -- capsets ----------------------------------------------------------------

    def finalize_capset(self, cls: type) -> CapSetType:
        capset = self.capsets[cls]
        if capset.type is not None:
            return capset.type
        if cls in self.capset_stack:
            chain = " -> ".join(item.__name__ for item in [*self.capset_stack, cls])
            raise DeclarationError(f"recursive capset: {chain}")
        self.capset_stack.append(cls)
        try:
            annotations = _annotations(cls, self.globalns)
            fields: list[Field] = []
            slots: list[tuple[str, int]] = []
            seen: set[str] = set()
            for name, annotation in annotations.items():
                if name.startswith("_"):
                    continue
                type_ = self.resolve(annotation)
                if not isinstance(type_, CapType):
                    raise DeclarationError(
                        f"{capset.name}.{name}: capset slots must be Cap[...] fields"
                    )
                self.validate_cap(type_, f"{capset.name}.{name}")
                if name in seen:
                    raise DeclarationError(f"{capset.name}: duplicate slot {name}")
                seen.add(name)
                fields.append(Field(name, type_))
                slots.append((name, len(slots) + 1))
            if not fields:
                raise DeclarationError(f"{capset.name}: capset has no slots")
            if len(fields) > MAX_CAPSET_CAPS:
                raise DeclarationError(
                    f"{capset.name}: {len(fields)} slots exceed the "
                    f"{MAX_CAPSET_CAPS}-slot capset limit"
                )
            capacity = getattr(cls, "__horn_capacity__", None)
            if capacity is not None and (
                not isinstance(capacity, int)
                or capacity < len(fields) + 1
                or capacity > MAX_CAPSET_CAPS + 1
                or capacity & (capacity - 1)
            ):
                raise DeclarationError(f"{capset.name}: invalid capset capacity")
            type_ = CapSetType(capset.name, tuple(fields), tuple(slots), capacity)
            capset.type = type_
            return type_
        finally:
            self.capset_stack.pop()

    # -- interfaces -------------------------------------------------------------

    def finalize_interface(self, cls: type) -> Interface:
        interface = self.interfaces[cls]
        if interface.methods or cls in self.interface_stack:
            return interface
        self.interface_stack.append(cls)
        try:
            extends: list[Interface] = []
            candidates = list(getattr(cls, "__horn_extends__", ()))
            for base in cls.__bases__:
                if (
                    getattr(base, "__horn_kind__", None) == "interface"
                    and base not in candidates
                ):
                    candidates.append(base)
            for base in candidates:
                if (
                    not isinstance(base, type)
                    or getattr(base, "__horn_kind__", None) != "interface"
                ):
                    raise DeclarationError(
                        f"{cls.__name__}: extends {base!r} is not an interface"
                    )
                extends.append(self.finalize_interface(base))
            interface.extends = extends

            methods: list[Method] = []
            seen: set[str] = set()
            for member in cls.__dict__.values():
                if not callable(member):
                    continue
                options: MethodOptions | None = getattr(member, "__horn_method__", None)
                if options is None:
                    continue
                method = self._build_method(interface, member, options)
                if method.name in seen:
                    raise DeclarationError(
                        f"{interface.fqn}: duplicate method {method.name}"
                    )
                seen.add(method.name)
                methods.append(method)
            interface.methods = methods
            return interface
        finally:
            self.interface_stack.pop()

    def _build_method(
        self, interface: Interface, func: Any, options: MethodOptions
    ) -> Method:
        signature = inspect.signature(func)
        annotations = _annotations(func, getattr(func, "__globals__", self.globalns))
        params: list[Field] = []
        for name, parameter in signature.parameters.items():
            if name in {"self", "cls"}:
                continue
            if name not in annotations:
                raise DeclarationError(
                    f"{interface.fqn}.{func.__name__}: parameter {name} has no type"
                )
            if parameter.default is not inspect.Parameter.empty:
                raise DeclarationError(
                    f"{interface.fqn}.{func.__name__}: defaults are not supported"
                )
            type_ = self.resolve(annotations[name])
            self.validate_value_type(type_, f"{interface.fqn}.{func.__name__}({name})")
            params.append(Field(name, type_))
        result_annotation = annotations.get("return", inspect.Signature.empty)
        result: Type | None
        if result_annotation is inspect.Signature.empty or _is_none(result_annotation):
            result = None
        else:
            result = self.resolve(result_annotation)
            self.validate_value_type(result, f"{interface.fqn}.{func.__name__} result")
        transport = options.transport or interface.transport
        if options.oneway and transport is Transport.SYNC:
            transport = Transport.ONEWAY
        method = Method(
            name=options.name or func.__name__,
            params=params,
            result=result,
            transport=transport,
            oneway=options.oneway or transport is Transport.ONEWAY,
            event=options.event,
            interface=interface,
            doc=inspect.getdoc(func),
        )
        if method.event and method.result is not None:
            raise DeclarationError(
                f"{interface.fqn}.{method.name}: events cannot return a value"
            )
        if method.oneway and method.result is not None:
            raise DeclarationError(
                f"{interface.fqn}.{method.name}: one-way methods cannot return a value"
            )
        if transport is Transport.ASYNC and (method.oneway or method.event):
            raise DeclarationError(
                f"{interface.fqn}.{method.name}: async methods cannot be one-way or events"
            )
        return method

    # -- objects ----------------------------------------------------------------

    def finalize_object(self, cls: type) -> Object:
        obj = self.objects[cls]
        implements: list[Interface] = []
        for interface_cls in getattr(cls, "__horn_implements__", ()):
            if getattr(interface_cls, "__horn_kind__", None) != "interface":
                raise DeclarationError(
                    f"{obj.fqn}: {interface_cls!r} is not an interface"
                )
            implements.append(self.finalize_interface(interface_cls))
        if not implements:
            raise DeclarationError(f"{obj.fqn}: object implements no interfaces")
        obj.implements = implements
        seen: set[int] = set()
        for interface in implements:
            for method in interface.all_methods():
                if id(method) in seen:
                    raise DeclarationError(
                        f"{obj.fqn}: duplicate method {interface.fqn}.{method.name}"
                    )
                seen.add(id(method))
        return obj

    # -- validation -------------------------------------------------------------

    def validate_value_type(self, type_: Any, where: str) -> None:
        if isinstance(type_, (Interface, Object)):
            raise DeclarationError(
                f"{where}: interfaces and objects are not wire values; use Cap[...]"
            )
        if isinstance(type_, (CapSetType, RingType, RegionType)):
            raise DeclarationError(
                f"{where}: capsets, rings, and regions are not wire values; use Cap[...]"
            )
        if isinstance(type_, Void):
            raise DeclarationError(f"{where}: void is not a wire value")
        if isinstance(type_, CapType):
            self.validate_cap(type_, where)
        for cap_path in type_.walk_caps(""):
            self.validate_cap(cap_path.cap, where)

    def validate_cap(self, cap: CapType, where: str) -> None:
        target = cap.target
        if not isinstance(
            target,
            (CapSetType, Interface, Object, sel4.KernelResource, RingType, RegionType),
        ):
            raise DeclarationError(
                f"{where}: Cap target {cap.target_name()} is not a capability-backed type"
            )

    def validate_method_caps(self, method: Method) -> None:
        request_caps = method.request_caps()
        result_caps = method.result_caps()
        request_count = sum(param.type.caps for param in method.params)
        result_count = method.result.caps if method.result is not None else 0
        if request_count > MAX_LAYOUT_CAPS:
            raise DeclarationError(
                f"{method.interface.fqn}.{method.name}: {request_count} request caps exceed the layout limit"
            )
        if result_count > MAX_LAYOUT_CAPS:
            raise DeclarationError(
                f"{method.interface.fqn}.{method.name}: {result_count} result caps exceed the layout limit"
            )
        paths = [cap.path for cap in [*request_caps, *result_caps]]
        if len(paths) != len(set(paths)):
            raise DeclarationError(
                f"{method.interface.fqn}.{method.name}: duplicate cap path"
            )
        for cap in request_caps:
            if cap.cap.mode is CapMode.BORROW and (method.oneway or method.event):
                raise DeclarationError(
                    f"{method.interface.fqn}.{method.name}: borrow needs a reply boundary"
                )
        for cap in result_caps:
            if cap.cap.mode is CapMode.BORROW:
                raise DeclarationError(
                    f"{method.interface.fqn}.{method.name}: borrow is not valid in a result"
                )
        if method.result is not None and method.result.size > MAX_MESSAGE_BYTES:
            raise DeclarationError(
                f"{method.interface.fqn}.{method.name}: result is larger than one seL4 message"
            )
        request_size = sum(param.type.size for param in method.params)
        if request_size > MAX_MESSAGE_BYTES:
            raise DeclarationError(
                f"{method.interface.fqn}.{method.name}: request is larger than one seL4 message"
            )

    def build_schema(self) -> Schema:
        schema = Schema(
            self.module.__name__,
            doc=inspect.getdoc(self.module),
            source_path=(
                str(Path(self.module.__file__).resolve())
                if getattr(self.module, "__file__", None)
                else None
            ),
        )
        for declarations in (
            self.records,
            self.enums,
            self.flags,
            self.interfaces,
            self.objects,
            self.capsets,
            self.rings,
            self.regions,
        ):
            for cls, decl in declarations.items():
                module = sys.modules.get(cls.__module__)
                source = getattr(module, "__file__", None)
                if source:
                    schema.declaration_sources[id(decl)] = str(Path(source).resolve())
        schema.records = list(self.records.values())
        schema.enums = list(self.enums.values())
        schema.flags = list(self.flags.values())
        schema.interfaces = list(self.interfaces.values())
        schema.objects = list(self.objects.values())
        schema.capsets = list(self.capsets.values())
        schema.rings = list(self.rings.values())
        schema.regions = list(self.regions.values())

        labels: dict[int, tuple[Interface, Method]] = {}
        for interface in schema.interfaces:
            for method in interface.all_methods():
                method.label = hash_label(interface.fqn, method.name, interface.version)
                if method.label in labels:
                    other_interface, other = labels[method.label]
                    raise DeclarationError(
                        f"label collision: {interface.fqn}.{method.name} and "
                        f"{other_interface.fqn}.{other.name}"
                    )
                labels[method.label] = (interface, method)
                self.validate_method_caps(method)
        return schema


def compile_module(module: pytypes.ModuleType) -> Schema:
    resolver = _Resolver(module)
    resolver.collect()
    return resolver.build_schema()


def load_module(path: str | Path) -> pytypes.ModuleType:
    path = Path(path)
    name = path.stem
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise DeclarationError(f"cannot load schema {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def compile_path(path: str | Path) -> Schema:
    return compile_module(load_module(path))
