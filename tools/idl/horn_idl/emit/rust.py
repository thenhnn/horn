"""Rust code generation.

The generated module is transport-agnostic.  A small runtime is emitted with
the protocol; a seL4 backend implements ``Transport`` and maps ``CapRef`` to
real capability slots.
"""

from __future__ import annotations

import json
import re
from typing import Any

from ..declarations import Transport
from ..layout import CapLayout, build_capset_layout
from ..model import Interface, Method, Object, Schema
from ..types import (
    ArrayType,
    CapMode,
    CapSetType,
    CapType,
    EnumType,
    Field,
    FlagsType,
    ListType,
    OptionType,
    ParcelType,
    RecordType,
    RegionType,
    ResultType,
    RingType,
    Scalar,
    TaggedEnumType,
    TupleType,
    Type,
    Void,
)


def rust_str(value: str) -> str:
    return json.dumps(value)


def snake(name: str) -> str:
    if not name.isupper():
        name = re.sub(r"(?<!^)(?=[A-Z])", "_", name)
    return re.sub(r"[^a-z0-9_]", "_", name.lower())


def upper(name: str) -> str:
    return snake(name).upper()


def pascal(name: str) -> str:
    return "".join(
        part.capitalize() for part in re.split(r"[^A-Za-z0-9]+", name) if part
    )


def rust_type(type_: Any) -> str:
    if isinstance(type_, ResultType):
        return f"Result<{rust_type(type_.ok)}, {rust_type(type_.error)}>"
    if isinstance(type_, Scalar):
        return type_.rust
    if isinstance(
        type_, (EnumType, TaggedEnumType, FlagsType, RecordType, RingType, RegionType)
    ):
        return type_.name
    if isinstance(type_, CapType):
        return type_.rust_type()
    if isinstance(type_, ListType):
        return f"List<{rust_type(type_.element)}, {type_.capacity}>"
    if isinstance(type_, ArrayType):
        return f"[{rust_type(type_.element)}; {type_.capacity}]"
    if isinstance(type_, OptionType):
        return f"Option<{rust_type(type_.inner)}>"
    if isinstance(type_, ParcelType):
        return type_.rust_type()
    if isinstance(type_, TupleType):
        if len(type_.elements) == 1:
            return f"({rust_type(type_.elements[0])},)"
        return "(" + ", ".join(rust_type(element) for element in type_.elements) + ")"
    if isinstance(type_, Void):
        return "()"
    if isinstance(type_, Interface):
        return type_.python_name
    if isinstance(type_, Object):
        return type_.python_name
    if hasattr(type_, "rust_name"):
        return type_.rust_name
    raise TypeError(f"cannot emit Rust type for {type_!r}")


def rust_default(type_: Any) -> str:
    if isinstance(type_, ResultType):
        return f"Ok({rust_default(type_.ok)})"
    if isinstance(type_, TaggedEnumType):
        return f"{type_.name}::default()"
    if isinstance(type_, Scalar):
        if type_.name == "bool":
            return "false"
        if type_.float:
            return "0.0"
        return "0"
    if isinstance(type_, EnumType):
        return f"{type_.name}::{type_.variants[0][0]}"
    if isinstance(type_, FlagsType):
        return f"{type_.name}(0)"
    if isinstance(type_, RecordType):
        return f"{type_.name}::default()"
    if isinstance(type_, ListType):
        return f"List {{ len: 0, data: [{rust_default(type_.element)}; {type_.capacity}] }}"
    if isinstance(type_, ArrayType):
        return f"[{rust_default(type_.element)}; {type_.capacity}]"
    if isinstance(type_, OptionType):
        return "None"
    if isinstance(type_, ParcelType):
        return f"Parcel::inline({rust_default(type_.inner)})"
    if isinstance(type_, TupleType):
        if not type_.elements:
            return "()"
        values = ", ".join(rust_default(element) for element in type_.elements)
        if len(type_.elements) == 1:
            return f"({values},)"
        return f"({values})"
    if isinstance(type_, CapType):
        return "Cap::default()"
    if isinstance(type_, Void):
        return "()"
    if isinstance(type_, (RingType, RegionType)):
        return f"{type_.name}::default()"
    raise TypeError(f"cannot emit Rust default for {type_!r}")


def method_args_name(interface: Interface, method: Method) -> str:
    return f"{pascal(interface.python_name)}{pascal(method.name)}Args"


def method_reply_name(interface: Interface, method: Method) -> str:
    return f"{pascal(interface.python_name)}{pascal(method.name)}Reply"


def reply_value(interface: Interface, method: Method, value: str) -> str:
    if method.result is None:
        return value
    return f"&{method_reply_name(interface, method)} {{ value: *{value} }}"


def method_variant_name(interface: Interface, method: Method) -> str:
    return f"{pascal(interface.python_name)}{pascal(method.name)}"


def method_label_name(interface: Interface, method: Method) -> str:
    return f"{upper(interface.python_name)}_{upper(method.name)}_LABEL"


def method_result_modes_name(interface: Interface, method: Method) -> str:
    return f"{upper(interface.python_name)}_{upper(method.name)}_RESULT_MODES"


def cap_mode(mode: CapMode) -> str:
    return f"CapMode::{mode.name}"


def rights_expr(rights: Any) -> str:
    bits = rights.value
    return f"Rights({bits})"


def encode_value(type_: Type, expr: str) -> str:
    if type_.size == 0:
        return ""
    if isinstance(type_, ParcelType):
        return encode_value(type_.inner, f"{expr}.data")
    body = []
    if isinstance(type_, ResultType):
        cases = " ".join(
            f"{name}(_value) => {{ out.u32({tag}); out.align({type_.align}); {encode_value(payload, '(*_value)')} }}"
            for name, tag, payload in type_.variants
        )
        body = [f"match &{expr} {{ {cases} }}"]
    elif isinstance(type_, TupleType):
        for index, element in enumerate(type_.elements):
            body += [
                f"out.align({element.align});",
                encode_value(element, f"{expr}.{index}"),
            ]
    elif isinstance(type_, ArrayType):
        body = [
            f"for _value in &{expr} {{ out.align({type_.element.align}); {encode_value(type_.element, '(*_value)')} }}"
        ]
    elif isinstance(type_, ListType):
        body = [
            f"out.u64({expr}.len as u64);",
            f"assert!({expr}.len as usize <= {type_.capacity});",
            f"for _value in &{expr}.data[..{expr}.len as usize] {{ out.align({type_.element.align}); {encode_value(type_.element, '(*_value)')} }}",
        ]
    elif isinstance(type_, OptionType):
        body = [
            f"out.u64(if {expr}.is_some() {{ 1 }} else {{ 0 }});",
            f"out.align({type_.inner.align});",
            f"if let Some(_value) = &{expr} {{ {encode_value(type_.inner, '(*_value)')} }}",
        ]
    else:
        return f"out.value(&{expr});"
    return f"out.scope({type_.size}, |out| {{ {' '.join(body)} }});"


def decode_value(type_: Type) -> str:
    if isinstance(type_, Void):
        return "input.scope(0, |_| Ok(()))"
    if isinstance(type_, CapType):
        marker = type_.rust_type()[4:-1]
        return f"input.cap::<{marker}>({cap_mode(type_.mode)}, {rights_expr(type_.rights)})"
    if isinstance(type_, ParcelType):
        return f"({decode_value(type_.inner)}).map(Parcel::inline)"
    body = []
    if isinstance(type_, ResultType):
        body += [
            "let tag = input.u32()?;",
            f"input.zeroes({type_.payload_offset - 4})?;",
        ]
        cases = " ".join(
            f"{tag} => {name}({decode_value(payload)}?),"
            for name, tag, payload in type_.variants
        )
        body += [
            f"let value = match tag {{ {cases} _ => return Err(RpcError::InvalidValue) }};"
        ]
        result = "value"
    elif isinstance(type_, TupleType):
        for index, element in enumerate(type_.elements):
            body += [
                f"input.align({element.align});",
                f"let f{index} = {decode_value(element)}?;",
            ]
        result = (
            "("
            + ",".join(f"f{index}" for index in range(len(type_.elements)))
            + ("," if type_.elements else "")
            + ")"
        )
    elif isinstance(type_, (ArrayType, ListType)):
        if isinstance(type_, ListType):
            body += [
                "let len = input.u64()?;",
                f"if len > {type_.capacity} {{ return Err(RpcError::InvalidValue); }}",
            ]
        body += [f"let mut data = [{rust_default(type_.element)}; {type_.capacity}];"]
        count = "len as usize" if isinstance(type_, ListType) else str(type_.capacity)
        body += [
            f"for value in &mut data[..{count}] {{ input.align({type_.element.align}); *value = {decode_value(type_.element)}?; }}"
        ]
        result = (
            "List { len: len as u32, data }" if isinstance(type_, ListType) else "data"
        )
    elif isinstance(type_, OptionType):
        body += [
            "let present = input.u64()?;",
            f"input.align({type_.inner.align});",
            f"let value = match present {{ 0 => None, 1 => Some({decode_value(type_.inner)}?), _ => return Err(RpcError::InvalidValue) }};",
        ]
        result = "value"
    else:
        return f"input.value::<{rust_type(type_)}>()"
    return f"input.scope({type_.size}, |input| {{ {' '.join(body)} input.finish_zeroed({type_.size})?; Ok({result}) }})"


def caps_value(type_: Type, expr: str) -> str:
    if not type_.caps:
        return ""
    if isinstance(type_, CapType):
        return f"out.push({{ let mut cap = {expr}.as_ref(); cap.mode = {cap_mode(type_.mode)}; cap.rights = {rights_expr(type_.rights)}; cap }})?;"
    if isinstance(type_, ResultType):
        cases = " ".join(
            f"{name}(_value) => {{ {caps_value(payload, '(*_value)')} }}"
            for name, _, payload in type_.variants
        )
        return f"match &{expr} {{ {cases} }}"
    if isinstance(type_, TupleType):
        return " ".join(
            caps_value(element, f"{expr}.{index}")
            for index, element in enumerate(type_.elements)
        )
    if isinstance(type_, ArrayType):
        return f"for value in &{expr} {{ {caps_value(type_.element, '(*value)')} }}"
    if isinstance(type_, ListType):
        return f"if {expr}.len as usize > {type_.capacity} {{ return Err(RpcError::InvalidValue); }} for value in &{expr}.data[..{expr}.len as usize] {{ {caps_value(type_.element, '(*value)')} }}"
    if isinstance(type_, OptionType):
        return (
            f"if let Some(value) = &{expr} {{ {caps_value(type_.inner, '(*value)')} }}"
        )
    if isinstance(type_, ParcelType):
        return caps_value(type_.inner, f"{expr}.data")
    return f"{expr}.caps(out)?;"


def _wire_field_encode(field_type: Type, expr: str) -> list[str]:
    return [
        f"        out.align({field_type.align});",
        f"        {encode_value(field_type, expr)}",
    ]


def _wire_field_decode(field_type: Type, name: str) -> list[str]:
    return [
        f"        input.align({field_type.align});",
        f"        let {name}: {rust_type(field_type)} = {decode_value(field_type)}?;",
    ]


HEADER = r"""
// Generated by horn-idl.  Do not edit.

use horn_ipc::*;
"""


class RustEmitter:
    def __init__(self, schema: Schema) -> None:
        self.schema = schema
        self.lines: list[str] = []

    def emit(self) -> str:
        self.lines = [HEADER]
        for record in self.schema.records:
            self.emit_record(record.type)
        for enum in self.schema.enums:
            if isinstance(enum.type, TaggedEnumType):
                self.emit_tagged_enum(enum.type)
            else:
                self.emit_enum(enum.type)
        for flags in self.schema.flags:
            self.emit_flags(flags.type)
        for capset in self.schema.capsets:
            self.emit_capset(capset.type)
        for ring in self.schema.rings:
            self.emit_ring(ring.type)
        for region in self.schema.regions:
            self.emit_region(region.type)
        for interface in self.schema.interfaces:
            self.emit_interface(interface)
        for object_ in self.schema.objects:
            self.emit_object(object_)
        self.emit_async_methods()
        return "\n".join(self.lines) + "\n"

    def add(self, *lines: str) -> None:
        self.lines.extend(lines)

    def emit_record(self, type_: RecordType) -> None:
        self.add("", "#[derive(Debug, Clone, Copy, PartialEq)]")
        self.add(f"pub struct {type_.name} {{")
        for field in type_.fields:
            self.add(f"    pub {snake(field.name)}: {rust_type(field.type)},")
        self.add("}")
        self.add("")
        self.add(f"impl Default for {type_.name} {{")
        self.add("    fn default() -> Self {")
        self.add("        Self {")
        for field in type_.fields:
            self.add(f"            {snake(field.name)}: {rust_default(field.type)},")
        self.add("        }")
        self.add("    }")
        self.add("}")
        self.add("")
        self.add(f"impl Wire for {type_.name} {{")
        self.add(f"    const SIZE: usize = {type_.size};")
        self.add(f"    const ALIGN: usize = {type_.align};")
        self.add(f"    const CAPS: usize = {type_.caps};")
        self.add("    fn encode(&self, out: &mut Encoder<'_>) {")
        for field in type_.fields:
            self.add(*_wire_field_encode(field.type, f"self.{snake(field.name)}"))
        self.add("        out.finish(Self::SIZE);")
        self.add("    }")
        self.add("    fn decode(input: &mut Decoder<'_>) -> Result<Self, RpcError> {")
        for field in type_.fields:
            self.add(*_wire_field_decode(field.type, snake(field.name)))
        self.add("        input.finish(Self::SIZE)?;")
        self.add("        Ok(Self {")
        for field in type_.fields:
            self.add(f"            {snake(field.name)},")
        self.add("        })")
        self.add("    }")
        out_name = "out" if type_.caps else "_out"
        self.add(
            f"    fn caps(&self, {out_name}: &mut dyn CapSink) -> Result<(), RpcError> {{"
        )
        for field in type_.fields:
            if field.type.caps:
                self.add(
                    f"        {caps_value(field.type, 'self.' + snake(field.name))}"
                )
        self.add("        Ok(())")
        self.add("    }")
        self.add("}")

    def emit_tagged_enum(self, type_: TaggedEnumType) -> None:
        self.add(
            "", "#[derive(Debug, Clone, Copy, PartialEq)]", f"pub enum {type_.name} {{"
        )
        for name, _, payload in type_.variants:
            suffix = "" if isinstance(payload, Void) else f"({rust_type(payload)})"
            self.add(f"    {name}{suffix},")
        self.add("}", f"impl Default for {type_.name} {{")
        name, _, payload = type_.variants[0]
        value = f"Self::{name}" + (
            "" if isinstance(payload, Void) else f"({rust_default(payload)})"
        )
        self.add(f"    fn default() -> Self {{ {value} }}", "}")
        self.add(
            f"impl Wire for {type_.name} {{",
            f"    const SIZE: usize = {type_.size};",
            f"    const ALIGN: usize = {type_.align};",
            f"    const CAPS: usize = {type_.caps};",
        )
        self.add(
            "    fn encode(&self, out: &mut Encoder<'_>) {",
            "        out.scope(Self::SIZE, |out| {",
            "            match self {",
        )
        for name, tag, payload in type_.variants:
            pattern = "" if isinstance(payload, Void) else "(_value)"
            self.add(
                f"                Self::{name}{pattern} => {{ out.{type_.base.name}({tag}); out.align({type_.align}); {encode_value(payload, '(*_value)')} }},"
            )
        self.add("            }", "        });", "    }")
        self.add(
            "    fn decode(input: &mut Decoder<'_>) -> Result<Self, RpcError> {",
            "        input.scope(Self::SIZE, |input| {",
            f"            let tag = input.{type_.base.name}()?;",
            f"            input.zeroes({type_.payload_offset - type_.base.size})?;",
            "            let value = match tag {",
        )
        for name, tag, payload in type_.variants:
            suffix = "" if isinstance(payload, Void) else f"({decode_value(payload)}?)"
            self.add(f"                {tag} => Self::{name}{suffix},")
        self.add(
            "                _ => return Err(RpcError::InvalidValue),",
            "            };",
            "            input.finish_zeroed(Self::SIZE)?;",
            "            Ok(value)",
            "        })",
            "    }",
        )
        out_name = "out" if type_.caps else "_out"
        self.add(
            f"    fn caps(&self, {out_name}: &mut dyn CapSink) -> Result<(), RpcError> {{",
            "        match self {",
        )
        for name, _, payload in type_.variants:
            pattern = "" if isinstance(payload, Void) else "(_value)"
            self.add(
                f"            Self::{name}{pattern} => {{ {caps_value(payload, '(*_value)')} }},"
            )
        self.add("        }", "        Ok(())", "    }", "}")

    def emit_enum(self, type_: EnumType) -> None:
        base = type_.base.rust
        self.add("", f"#[repr({base})]")
        self.add("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
        self.add(f"pub enum {type_.name} {{")
        for name, value in type_.variants:
            self.add(f"    {name} = {value},")
        self.add("}")
        self.add("")
        self.add(f"impl Default for {type_.name} {{")
        self.add(f"    fn default() -> Self {{ Self::{type_.variants[0][0]} }}")
        self.add("}")
        self.add("")
        self.add(f"impl Wire for {type_.name} {{")
        self.add(f"    const SIZE: usize = {type_.size};")
        self.add(f"    const ALIGN: usize = {type_.align};")
        self.add("    const CAPS: usize = 0;")
        self.add("    fn encode(&self, out: &mut Encoder<'_>) {")
        self.add(f"        out.{base}(*self as {base});")
        self.add("    }")
        self.add("    fn decode(input: &mut Decoder<'_>) -> Result<Self, RpcError> {")
        self.add(f"        let value = input.{base}()?;")
        self.add("        match value {")
        for name, value in type_.variants:
            self.add(f"            {value} => Ok(Self::{name}),")
        self.add("            _ => Err(RpcError::InvalidValue),")
        self.add("        }")
        self.add("    }")
        self.add(
            "    fn caps(&self, _out: &mut dyn CapSink) -> Result<(), RpcError> { Ok(()) }"
        )
        self.add("}")

    def emit_flags(self, type_: FlagsType) -> None:
        base = type_.base.rust
        self.add("", "#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]")
        self.add(f"pub struct {type_.name}(pub {base});")
        self.add("")
        self.add(f"impl {type_.name} {{")
        for name, value in type_.variants:
            self.add(f"    pub const {name}: Self = Self({value});")
        self.add("}")
        self.add("")
        self.add(f"impl Wire for {type_.name} {{")
        self.add(f"    const SIZE: usize = {type_.size};")
        self.add(f"    const ALIGN: usize = {type_.align};")
        self.add("    const CAPS: usize = 0;")
        self.add("    fn encode(&self, out: &mut Encoder<'_>) {")
        self.add(f"        out.{base}(self.0);")
        self.add("    }")
        self.add("    fn decode(input: &mut Decoder<'_>) -> Result<Self, RpcError> {")
        self.add(f"        Ok(Self(input.{base}()?))")
        self.add("    }")
        self.add(
            "    fn caps(&self, _out: &mut dyn CapSink) -> Result<(), RpcError> { Ok(()) }"
        )
        self.add("}")

    def emit_capset(self, type_: CapSetType) -> None:
        marker = f"{type_.name}Tag"
        self.add("", f"pub struct {marker};")
        self.add(f"impl CapType for {marker} {{")
        self.add(f"    const NAME: &'static str = {rust_str(type_.name)};")
        self.add('    const KERNEL: &\'static str = "CNode";')
        self.add("}")
        self.add(
            f"pub const {upper(type_.name)}_SLOTS: &[(&str, u32)] = &["
            + ", ".join(f"({rust_str(name)}, {slot})" for name, slot in type_.slots)
            + "];"
        )
        for name, slot in type_.slots:
            self.add(f"pub const {upper(type_.name)}_{upper(name)}_SLOT: u32 = {slot};")
        self.add(f"pub const {upper(type_.name)}_SELF_SLOT: u32 = 0;")
        self.add(
            f"pub const {upper(type_.name)}_FIRST_FREE_SLOT: u32 = {len(type_.slots) + 1};"
        )
        layout = build_capset_layout(type_.fields, type_.capacity)
        self.emit_layout_const(f"{upper(type_.name)}_CAPSET", layout)
        self.add(
            f"pub const {upper(type_.name)}_SIZE_BITS: u8 = "
            f"{layout.cnode.size_bits if layout.cnode else 0};"
        )
        self.add("")
        self.add("#[derive(Debug, Clone, Copy, PartialEq)]")
        self.add(f"pub struct {type_.name} {{")
        for field in type_.fields:
            self.add(f"    pub {snake(field.name)}: {rust_type(field.type)},")
        self.add("}")
        self.add("")
        self.add(f"impl {type_.name} {{")
        self.add(
            f"    pub fn pack<S: CapSpace>(&self, space: &mut S) -> Result<Cap<{marker}>, RpcError> {{"
        )
        self.add(
            f"        let mut packer = CapPacker::new(space, &{upper(type_.name)}_CAPSET_LAYOUT);"
        )
        for field in type_.fields:
            self.add(f"        self.{snake(field.name)}.caps(&mut packer)?;")
        self.add("        let caps = packer.finish()?;")
        self.add(
            "        let cnode = caps.as_slice().first().and_then(|cap| *cap).ok_or(RpcError::MissingCap)?;"
        )
        self.add("        space.install_self_cnode(cnode)?;")
        self.add("        Ok(Cap::from_ref(cnode))")
        self.add("    }")
        self.add(
            f"    pub fn unpack<S: CapSpace>(cnode: Cap<{marker}>, space: &mut S) -> Result<Self, RpcError> {{"
        )
        self.add("        let mut caps = WireCaps::default();")
        self.add("        caps.push(cnode.as_ref())?;")
        self.add(
            f"        let mut unpacker = CapUnpacker::new_localizing(space, &{upper(type_.name)}_CAPSET_LAYOUT, caps);"
        )
        self.add("        let mut dec = Decoder::new(&[], &mut unpacker);")
        self.add("        Ok(Self {")
        for field in type_.fields:
            self.add(f"            {snake(field.name)}: Wire::decode(&mut dec)?,")
        self.add("        })")
        self.add("    }")
        self.add("}")

    def emit_ring(self, type_: RingType) -> None:
        self.add("", f"pub struct {type_.name};")
        self.add(f"impl CapType for {type_.name} {{")
        self.add(f"    const NAME: &'static str = {rust_str(type_.name)};")
        self.add('    const KERNEL: &\'static str = "Frame";')
        self.add("}")
        self.add(
            f"pub type {type_.name}Ring = Ring<{rust_type(type_.element)}, {type_.capacity}>;"
        )

    def emit_region(self, type_: RegionType) -> None:
        self.add("", f"pub struct {type_.name};")
        self.add(f"impl CapType for {type_.name} {{")
        self.add(f"    const NAME: &'static str = {rust_str(type_.name)};")
        self.add('    const KERNEL: &\'static str = "Frame";')
        self.add("}")
        self.add(
            f"pub type {type_.name}Region = Region<{rust_type(type_.element)}, {type_.capacity}>;"
        )

    def emit_interface(self, interface: Interface) -> None:
        marker = f"{interface.python_name}Tag"
        self.add("", f"pub struct {marker};")
        self.add(f"impl CapType for {marker} {{")
        self.add(f"    const NAME: &'static str = {rust_str(interface.fqn)};")
        self.add('    const KERNEL: &\'static str = "Endpoint";')
        self.add("}")
        for method in interface.all_methods():
            self.emit_method_types(interface, method)
            if method.result is not None:
                self.emit_record(
                    RecordType(
                        method_reply_name(interface, method),
                        (Field("value", method.result),),
                    )
                )
        self.add("")
        self.add(f"pub trait {interface.python_name} {{")
        for method in interface.all_methods():
            params = ", ".join(
                f"{snake(param.name)}: {rust_type(param.type)}"
                for param in method.params
            )
            result = "()" if method.result is None else rust_type(method.result)
            self.add(f"    fn {snake(method.name)}(&mut self, {params}) -> {result};")
        self.add("}")
        self.emit_client(interface)

    def emit_method_types(self, interface: Interface, method: Method) -> None:
        args_name = method_args_name(interface, method)
        self.add("", "#[derive(Debug, Clone, Copy, PartialEq)]")
        self.add(f"pub struct {args_name} {{")
        for param in method.params:
            self.add(f"    pub {snake(param.name)}: {rust_type(param.type)},")
        self.add("}")
        self.add("")
        self.add(f"impl Wire for {args_name} {{")
        offset = 0
        align = 1
        caps = 0
        for param in method.params:
            offset = (
                (offset + param.type.align - 1) // param.type.align * param.type.align
            )
            offset += param.type.size
            align = max(align, param.type.align)
            caps += param.type.caps
        size = (offset + align - 1) // align * align if method.params else 0
        self.add(f"    const SIZE: usize = {size};")
        self.add(f"    const ALIGN: usize = {align};")
        self.add(f"    const CAPS: usize = {caps};")
        self.add("    fn encode(&self, out: &mut Encoder<'_>) {")
        for param in method.params:
            self.add(*_wire_field_encode(param.type, f"self.{snake(param.name)}"))
        self.add("        out.finish(Self::SIZE);")
        self.add("    }")
        self.add("    fn decode(input: &mut Decoder<'_>) -> Result<Self, RpcError> {")
        for param in method.params:
            self.add(*_wire_field_decode(param.type, snake(param.name)))
        self.add("        input.finish(Self::SIZE)?;")
        self.add("        Ok(Self {")
        for param in method.params:
            self.add(f"            {snake(param.name)},")
        self.add("        })")
        self.add("    }")
        out_name = "out" if caps else "_out"
        self.add(
            f"    fn caps(&self, {out_name}: &mut dyn CapSink) -> Result<(), RpcError> {{"
        )
        for param in method.params:
            if param.type.caps:
                self.add(
                    f"        {caps_value(param.type, 'self.' + snake(param.name))}"
                )
        self.add("        Ok(())")
        self.add("    }")
        self.add("}")
        self.add("")
        self.add(
            f"pub const {method_label_name(interface, method)}: u64 = {method.label};"
        )
        request_caps = method.request_caps()
        result_caps = method.result_caps()
        self.add(
            f"pub const {upper(interface.python_name)}_{upper(method.name)}_REQ_CAPS: "
            f"&[(&str, CapMode, Rights)] = &["
            + ", ".join(
                f"({rust_str(cap.path)}, {cap_mode(cap.cap.mode)}, {rights_expr(cap.cap.rights)})"
                for cap in request_caps
            )
            + "];"
        )
        self.add(
            f"pub const {upper(interface.python_name)}_{upper(method.name)}_RESULT_CAPS: "
            f"&[(&str, CapMode, Rights)] = &["
            + ", ".join(
                f"({rust_str(cap.path)}, {cap_mode(cap.cap.mode)}, {rights_expr(cap.cap.rights)})"
                for cap in result_caps
            )
            + "];"
        )
        self.emit_layout(interface, method, "REQ", method.request_layout())
        self.emit_layout(interface, method, "RESULT", method.result_layout())

    def emit_layout_const(self, prefix: str, layout: CapLayout) -> None:
        variable = str(layout.variable).lower()
        if layout.direct:
            self.add(f"pub const {prefix}_LAYOUT: CapLayout = CapLayout {{")
            self.add("    direct: &[")
            for slot in layout.direct:
                self.add(
                    "        CapSlot { "
                    f"path: {rust_str(slot.path)}, slot: {slot.slot}, "
                    f"mode: {cap_mode(slot.mode)}, rights: {rights_expr(slot.rights)} }},"
                )
            self.add("    ],")
            self.add("    cnode: None,")
            self.add(f"    variable: {variable},")
            self.add("};")
            return
        if layout.cnode is None:
            self.add(
                f"pub const {prefix}_LAYOUT: CapLayout = "
                f"CapLayout {{ direct: &[], cnode: None, variable: {variable} }};"
            )
            return
        node = layout.cnode
        node_name = f"{prefix}_NODE"
        self.add(f"const {node_name}: CapNode = CapNode {{")
        self.add(f"    size_bits: {node.size_bits},")
        self.add("    slots: &[")
        for slot in node.slots:
            self.add(
                "        CapSlot { "
                f"path: {rust_str(slot.path)}, slot: {slot.slot}, "
                f"mode: {cap_mode(slot.mode)}, rights: {rights_expr(slot.rights)} }},"
            )
        self.add("    ],")
        self.add("};")
        self.add(f"pub const {prefix}_LAYOUT: CapLayout = CapLayout {{")
        self.add("    direct: &[],")
        self.add(f"    cnode: Some(&{node_name}),")
        self.add(f"    variable: {variable},")
        self.add("};")

    def emit_layout(
        self,
        interface: Interface,
        method: Method,
        kind: str,
        layout: CapLayout,
    ) -> None:
        prefix = f"{upper(interface.python_name)}_{upper(method.name)}_{kind}"
        self.emit_layout_const(prefix, layout)

    def emit_async_methods(self) -> None:
        seen: set[int] = set()
        for interface, method in self.schema.methods():
            if id(method) in seen or method.transport is not Transport.ASYNC:
                continue
            seen.add(id(method))
            prefix = f"{snake(interface.python_name)}_{snake(method.name)}"
            args_type = method_args_name(interface, method)
            result_type = "()" if method.result is None else rust_type(method.result)
            self.add("")
            self.add(f"pub fn {prefix}_recv_async<S: AsyncServer>(")
            self.add("    server: &mut S,")
            self.add("    authority: S::Authority,")
            self.add(f") -> Result<({args_type}, S::Reply), RpcError> {{")
            self.add(
                f"    let (_label, args, reply) = server.recv_request(&{upper(interface.python_name)}_{upper(method.name)}_REQ_LAYOUT, authority, |label, input| {{"
            )
            self.add(f"        if label != {method_label_name(interface, method)} {{")
            self.add("            return Err(RpcError::UnknownMethod);")
            self.add("        }")
            self.add("        Wire::decode(input)")
            self.add("    })?;")
            self.add("    Ok((args, reply))")
            self.add("}")
            self.add("")
            self.add(f"pub fn {prefix}_reply_async<S: CapSpace, R: AsyncReply>(")
            self.add("    reply: R,")
            self.add("    space: &mut S,")
            self.add(f"    result: &{result_type},")
            self.add(") -> Result<(), RpcError> {")
            self.add(
                f"    reply.reply_wire(space, {reply_value(interface, method, 'result')}, &{upper(interface.python_name)}_{upper(method.name)}_RESULT_LAYOUT)"
            )
            self.add("}")

    def emit_client(self, interface: Interface) -> None:
        name = f"{interface.python_name}Client"
        self.add("")
        self.add(f"pub struct {name}<T: Transport> {{")
        self.add("    pub transport: T,")
        self.add(f"    pub target: Cap<{interface.python_name}Tag>,")
        self.add("}")
        self.add("")
        self.add(f"impl<T: Transport> {name}<T> {{")
        self.add(
            "    pub fn new(transport: T, target: Cap<"
            + interface.python_name
            + "Tag>) -> Self {"
        )
        self.add("        Self { transport, target }")
        self.add("    }")
        for method in interface.all_methods():
            params = []
            for param in method.params:
                if isinstance(param.type, CapType) and param.type.mode in (
                    CapMode.COPY,
                    CapMode.BORROW,
                ):
                    params.append(f"{snake(param.name)}: &{rust_type(param.type)}")
                else:
                    params.append(f"{snake(param.name)}: {rust_type(param.type)}")
            signature = ", ".join(params)
            if method.oneway or method.result is None:
                result = "()"
            else:
                result = rust_type(method.result)
            error_type = "RpcError"
            if isinstance(method.result, ResultType):
                result = rust_type(method.result.ok)
                error_type = f"CallError<{rust_type(method.result.error)}>"
            self.add(
                f"    pub fn {snake(method.name)}(&mut self, {signature}) -> Result<{result}, {error_type}> {{"
            )
            self.add(f"        let args = {method_args_name(interface, method)} {{")
            for param in method.params:
                field = snake(param.name)
                if isinstance(param.type, CapType):
                    self.add(
                        f"            {field}: Cap::from_ref({field}.as_ref().with_mode({cap_mode(param.type.mode)})),"
                    )
                else:
                    self.add(f"            {field},")
            self.add("        };")
            label = method_label_name(interface, method)
            if method.oneway:
                self.add(
                    f"        self.transport.send(self.target.as_ref(), {label}, &args, "
                    f"&{upper(interface.python_name)}_{upper(method.name)}_REQ_LAYOUT)"
                )
            else:
                call = (
                    f"self.transport.call(self.target.as_ref(), {label}, &args, "
                    f"&{upper(interface.python_name)}_{upper(method.name)}_REQ_LAYOUT, "
                    f"&{upper(interface.python_name)}_{upper(method.name)}_RESULT_LAYOUT)"
                )
                if method.result is None:
                    self.add(f"        {call}")
                else:
                    self.add(
                        f"        let reply: {method_reply_name(interface, method)} = {call}?;"
                    )
                    self.add(
                        "        reply.value.map_err(CallError::Remote)"
                        if isinstance(method.result, ResultType)
                        else "        Ok(reply.value)"
                    )
            self.add("    }")
        self.add("}")

    def emit_object(self, object_: Object) -> None:
        marker = f"{object_.python_name}Tag"
        self.add("", f"pub struct {marker};")
        self.add(f"impl CapType for {marker} {{")
        self.add(f"    const NAME: &'static str = {rust_str(object_.fqn)};")
        self.add('    const KERNEL: &\'static str = "Endpoint";')
        self.add("}")
        self.add("")
        self.add("#[derive(Debug)]")
        self.add(f"pub enum {object_.python_name}Request {{")
        for interface, method in self._object_methods(object_):
            self.add(
                f"    {method_variant_name(interface, method)}({method_args_name(interface, method)}),"
            )
        self.add("}")
        self.add("")
        self.add(f"impl {object_.python_name}Request {{")
        self.add(
            "    pub fn decode(label: u64, input: &mut Decoder<'_>) -> Result<Self, RpcError> {"
        )
        self.add("        match label {")
        for interface, method in self._object_methods(object_):
            self.add(
                f"            {method_label_name(interface, method)} => Ok(Self::"
                f"{method_variant_name(interface, method)}(Wire::decode(input)?)),"
            )
        self.add("            _ => Err(RpcError::UnknownMethod),")
        self.add("        }")
        self.add("    }")
        self.add("}")
        self.add("")
        self.add(f"pub enum {object_.python_name}Response {{")
        for interface, method in self._object_methods(object_):
            result = "()" if method.result is None else rust_type(method.result)
            self.add(f"    {method_variant_name(interface, method)}({result}),")
        self.add("}")
        self.add("")
        self.add(f"impl {object_.python_name}Response {{")
        self.add("    pub fn encode(&self, out: &mut Encoder<'_>) {")
        self.add("        match self {")
        for interface, method in self._object_methods(object_):
            variant = method_variant_name(interface, method)
            body = (
                encode_value(method.result, "(*_value)")
                if method.result is not None
                else ""
            )
            self.add(f"            Self::{variant}(_value) => {{ {body} }},")
        self.add("        }")
        self.add("    }")
        caps_arg = (
            "out"
            if any(method.result_caps() for _, method in self._object_methods(object_))
            else "_out"
        )
        self.add(
            f"    pub fn caps(&self, {caps_arg}: &mut dyn CapSink) -> Result<(), RpcError> {{"
        )
        self.add("        match self {")
        for interface, method in self._object_methods(object_):
            variant = method_variant_name(interface, method)
            body = (
                caps_value(method.result, "(*_value)")
                if method.result is not None
                else ""
            )
            self.add(f"            Self::{variant}(_value) => {{ {body} Ok(()) }},")
        self.add("        }")
        self.add("    }")
        self.add("}")
        self.add("")
        prefix = snake(object_.python_name)
        self.add(
            f"pub fn {prefix}_request_layout(label: u64) -> Result<&'static CapLayout, RpcError> {{"
        )
        self.add("    match label {")
        for interface, method in self._object_methods(object_):
            self.add(
                f"        {method_label_name(interface, method)} => "
                f"Ok(&{upper(interface.python_name)}_{upper(method.name)}_REQ_LAYOUT),"
            )
        self.add("        _ => Err(RpcError::UnknownMethod),")
        self.add("    }")
        self.add("}")
        self.add("")
        self.add(f"pub fn {prefix}_recv<S: AsyncServer>(")
        self.add("    server: &mut S,")
        self.add("    authority: S::Authority,")
        self.add(f") -> Result<({object_.python_name}Request, S::Reply), RpcError> {{")
        self.add("    let (_label, request, reply) = server.recv_request_dynamic(")
        self.add(f"        {prefix}_request_layout,")
        self.add("        authority,")
        self.add(
            f"        |label, input| {object_.python_name}Request::decode(label, input),"
        )
        self.add("    )?;")
        self.add("    Ok((request, reply))")
        self.add("}")
        self.add("")
        self.add(f"pub fn {prefix}_reply<S: CapSpace, R: AsyncReply>(")
        self.add("    reply: R,")
        self.add("    space: &mut S,")
        self.add(f"    response: &{object_.python_name}Response,")
        self.add(") -> Result<(), RpcError> {")
        self.add("    match response {")
        for interface, method in self._object_methods(object_):
            variant = method_variant_name(interface, method)
            if method.oneway:
                self.add(
                    f"        {object_.python_name}Response::{variant}(_value) => Ok(()),"
                )
            else:
                self.add(
                    f"        {object_.python_name}Response::{variant}(value) => "
                    f"reply.reply_wire(space, {reply_value(interface, method, 'value')}, &{upper(interface.python_name)}_{upper(method.name)}_RESULT_LAYOUT),"
                )
        self.add("    }")
        self.add("}")
        self.add("")
        bounds = " + ".join(interface.python_name for interface in object_.implements)
        self.add(
            f"pub fn {prefix}_dispatch<T: {bounds}>(request: {object_.python_name}Request, handler: &mut T) -> {object_.python_name}Response {{"
        )
        self.add("    match request {")
        for interface, method in self._object_methods(object_):
            variant = method_variant_name(interface, method)
            pattern = "args" if method.params else "_args"
            args = [f"args.{snake(param.name)}" for param in method.params]
            call = f"{interface.python_name}::{snake(method.name)}(handler"
            if args:
                call += ", " + ", ".join(args)
            call += ")"
            self.add(
                f"        {object_.python_name}Request::{variant}({pattern}) => {object_.python_name}Response::{variant}({call}),"
            )
        self.add("    }")
        self.add("}")

    def _object_methods(self, object_: Object) -> list[tuple[Interface, Method]]:
        out: list[tuple[Interface, Method]] = []
        for interface in object_.implements:
            for method in interface.all_methods():
                out.append((interface, method))
        return out


def emit(schema: Schema) -> str:
    return RustEmitter(schema).emit()
