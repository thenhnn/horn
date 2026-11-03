"""Reply-only owning Rust values. Raw Wire/server types remain unchanged."""

from ..types import (
    ArrayType,
    CapType,
    ListType,
    OptionType,
    ParcelType,
    RecordType,
    ResultType,
    TaggedEnumType,
    TupleType,
    Void,
)


def owned_type(t):
    from .rust import rust_type

    if not t.caps:
        return rust_type(t)
    if isinstance(t, CapType):
        return "Owned" + rust_type(t)
    if isinstance(t, (RecordType, TaggedEnumType)) and not isinstance(t, ResultType):
        return t.name + "Owned"
    if isinstance(t, ResultType):
        return f"Result<{owned_type(t.ok)}, {owned_type(t.error)}>"
    if isinstance(t, OptionType):
        return f"Option<{owned_type(t.inner)}>"
    if isinstance(t, ParcelType):
        return f"Parcel<{owned_type(t.inner)}>"
    if isinstance(t, ArrayType):
        return f"[{owned_type(t.element)}; {t.capacity}]"
    if isinstance(t, ListType):
        return f"OwnedList<{owned_type(t.element)}, {t.capacity}>"
    if isinstance(t, TupleType):
        return "(" + "".join(owned_type(v) + "," for v in t.elements) + ")"
    raise TypeError(t)


def raw_value(t, value):
    from .rust import rust_default

    if not t.caps:
        return value
    if isinstance(t, (CapType, RecordType, TaggedEnumType)) and not isinstance(
        t, ResultType
    ):
        return f"{value}.into_raw()"
    if isinstance(t, ResultType):
        return f"{value}.map(|v| {raw_value(t.ok, 'v')}).map_err(|v| {raw_value(t.error, 'v')})"
    if isinstance(t, OptionType):
        return f"{value}.map(|v| {raw_value(t.inner, 'v')})"
    if isinstance(t, ParcelType):
        return f"Parcel::inline({raw_value(t.inner, value + '.data')})"
    if isinstance(t, ArrayType):
        return f"{value}.map(|v| {raw_value(t.element, 'v')})"
    if isinstance(t, ListType):
        return f"{value}.into_raw({rust_default(t.element)}, |v| {raw_value(t.element, 'v')})"
    if isinstance(t, TupleType):
        return (
            "("
            + "".join(
                raw_value(v, f"{value}.{i}") + "," for i, v in enumerate(t.elements)
            )
            + ")"
        )
    raise TypeError(t)


def decode_owned(t):
    from .rust import cap_mode, decode_value, rights_expr

    if not t.caps:
        return decode_value(t)
    if isinstance(t, CapType):
        marker = t.rust_type()[4:-1]
        return f"OwnedCap::<{marker}>::decode(input, {cap_mode(t.mode)}, {rights_expr(t.rights)})"
    if isinstance(t, (RecordType, TaggedEnumType)) and not isinstance(t, ResultType):
        return f"input.scope({t.size}, <{owned_type(t)} as ReplyDecode>::decode_reply)"
    if isinstance(t, ParcelType):
        return f"({decode_owned(t.inner)}).map(Parcel::inline)"
    body = []
    if isinstance(t, ResultType):
        body = ["let tag = input.u32()?;", f"input.zeroes({t.payload_offset - 4})?;"]
        cases = " ".join(
            f"{tag} => {name}({decode_owned(payload)}?),"
            for name, tag, payload in t.variants
        )
        body += [
            f"let value = match tag {{ {cases} _ => return Err(RpcError::InvalidValue) }};"
        ]
        result = "value"
    elif isinstance(t, OptionType):
        body = [
            "let present = input.u64()?;",
            f"input.align({t.inner.align});",
            f"let value = match present {{ 0 => None, 1 => Some({decode_owned(t.inner)}?), _ => return Err(RpcError::InvalidValue) }};",
        ]
        result = "value"
    elif isinstance(t, TupleType):
        for i, item in enumerate(t.elements):
            body += [
                f"input.align({item.align});",
                f"let f{i} = {decode_owned(item)}?;",
            ]
        result = "(" + "".join(f"f{i}," for i in range(len(t.elements))) + ")"
    elif isinstance(t, (ArrayType, ListType)):
        if isinstance(t, ListType):
            body += [
                "let len = input.u64()?;",
                f"if len > {t.capacity} {{ return Err(RpcError::InvalidValue); }}",
            ]
        count = "len as usize" if isinstance(t, ListType) else str(t.capacity)
        body += [
            f"let mut data = alloc::vec::Vec::with_capacity({count});",
            f"for _ in 0..{count} {{ input.align({t.element.align}); data.push({decode_owned(t.element)}?); }}",
        ]
        result = (
            "OwnedList { len: len as u32, data }"
            if isinstance(t, ListType)
            else "data.try_into().map_err(|_| RpcError::InvalidValue)?"
        )
    else:
        raise TypeError(t)
    return f"input.scope({t.size}, |input| {{ {' '.join(body)} input.finish_zeroed({t.size})?; Ok({result}) }})"


def emit_owned(emitter, t):
    if not t.caps:
        return
    from .rust import snake

    add = emitter.add
    name = owned_type(t)
    add("", "#[derive(Debug, PartialEq)]")
    if isinstance(t, RecordType):
        add(f"pub struct {name} {{")
        for field in t.fields:
            add(f"    pub {snake(field.name)}: {owned_type(field.type)},")
        add("}")
    else:
        add(f"pub enum {name} {{")
        for variant, _, payload in t.variants:
            suffix = "" if isinstance(payload, Void) else f"({owned_type(payload)})"
            add(f"    {variant}{suffix},")
        add("}")
    add(
        f"impl ReplyDecode for {name} {{",
        f"    const REPLY_SIZE: usize = {t.size};",
        "    fn decode_reply(input: &mut Decoder<'_>) -> Result<Self, RpcError> {",
    )
    if isinstance(t, RecordType):
        for field in t.fields:
            add(
                f"        input.align({field.type.align});",
                f"        let {snake(field.name)} = {decode_owned(field.type)}?;",
            )
        add("        input.finish(Self::REPLY_SIZE)?;", "        Ok(Self {")
        for field in t.fields:
            add(f"            {snake(field.name)},")
        add("        })")
    else:
        add(
            f"        let tag = input.{t.base.name}()?;",
            f"        input.zeroes({t.payload_offset - t.base.size})?;",
            "        let value = match tag {",
        )
        for variant, tag, payload in t.variants:
            suffix = "" if isinstance(payload, Void) else f"({decode_owned(payload)}?)"
            add(f"            {tag} => Self::{variant}{suffix},")
        add(
            "            _ => return Err(RpcError::InvalidValue),",
            "        };",
            "        input.finish_zeroed(Self::REPLY_SIZE)?;",
            "        Ok(value)",
        )
    add("    }", "}", f"impl {name} {{", f"    pub fn into_raw(self) -> {t.name} {{")
    if isinstance(t, RecordType):
        add(f"        {t.name} {{")
        for field in t.fields:
            f = snake(field.name)
            add(f"            {f}: {raw_value(field.type, 'self.' + f)},")
        add("        }")
    else:
        add("        match self {")
        for variant, _, payload in t.variants:
            if isinstance(payload, Void):
                add(f"            Self::{variant} => {t.name}::{variant},")
            else:
                add(
                    f"            Self::{variant}(v) => {t.name}::{variant}({raw_value(payload, 'v')}),"
                )
        add("        }")
    add("    }", "}")
