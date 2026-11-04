"""Build serializable reference data from resolved Horn IDL schemas."""

from __future__ import annotations

from ..model import Schema
from ..types import (
    ArrayType,
    CapType,
    ListType,
    OptionType,
    ParcelType,
    ResultType,
    TaggedEnumType,
    TupleType,
)


def schema_view(name: str, schema: Schema, source_schemas: dict[str, str]) -> dict:
    """Describe declarations and resolve references to their defining schemas."""
    all_declarations = [
        *schema.records,
        *schema.enums,
        *schema.flags,
        *schema.interfaces,
        *schema.objects,
        *schema.capsets,
        *schema.rings,
        *schema.regions,
    ]

    def target(decl) -> dict:
        source = schema.declaration_sources.get(id(decl))
        return {
            "schema": source_schemas.get(source, name),
            "kind": type(decl).__name__.lower(),
            "name": decl.python_name,
        }

    def owned(decl) -> bool:
        source = schema.declaration_sources.get(id(decl))
        return source_schemas.get(source, name) == name

    targets = {}
    for decl in all_declarations:
        for key in (
            getattr(decl, "name", None),
            getattr(decl, "fqn", None),
            decl.python_name,
        ):
            if key:
                targets.setdefault(key, target(decl))

    def named(value: str) -> dict:
        return {"kind": "named", "name": value, "target": targets.get(value)}

    def method_target(interface, method) -> dict:
        return {
            "schema": target(interface)["schema"],
            "kind": "method",
            "name": interface.python_name + "." + method.name,
        }

    def type_view(t) -> dict:
        if t is None:
            return named("None")
        if isinstance(t, CapType):
            return {
                "kind": "cap",
                "target": named(t.target_name()),
                "mode": t.mode.value,
                "rights": t.rights.name or str(t.rights.value),
                "optional": t.optional,
            }
        if isinstance(t, ResultType):
            return {
                "kind": "generic",
                "name": "Result",
                "args": [type_view(t.ok), type_view(t.error)],
            }
        if isinstance(t, (ArrayType, ListType)):
            return {
                "kind": "bounded",
                "name": "Array" if isinstance(t, ArrayType) else "List",
                "element": type_view(t.element),
                "capacity": t.capacity,
            }
        if isinstance(t, (OptionType, ParcelType)):
            return {
                "kind": "generic",
                "name": "Option" if isinstance(t, OptionType) else "Parcel",
                "args": [type_view(t.inner)],
            }
        if isinstance(t, TupleType):
            return {"kind": "tuple", "args": [type_view(x) for x in t.elements]}
        return named(getattr(t, "name", str(t)))

    def cap_view(cap) -> dict:
        return {
            "path": cap.path,
            "target": named(cap.cap.target_name()),
            "mode": cap.cap.mode.value,
            "rights": cap.cap.rights.name or str(cap.cap.rights.value),
        }

    def method_view(interface, method) -> dict:
        return {
            "name": method.name,
            "target": method_target(interface, method),
            "params": [
                {"name": p.name, "type": type_view(p.type)} for p in method.params
            ],
            "result": type_view(method.result),
            "transport": method.transport.value,
            "event": method.event,
            "label": hex(method.label) if method.label is not None else "unavailable",
            "doc": method.doc,
            "request_caps": [cap_view(c) for c in method.request_caps()],
            "result_caps": [cap_view(c) for c in method.result_caps()],
        }

    def inherited_view(method) -> dict:
        parent = method.interface
        return {
            "name": parent.python_name + "." + method.name,
            "target": method_target(parent, method),
        }

    def declaration_view(decl) -> dict:
        kind = type(decl).__name__.lower()
        view = {
            "kind": kind,
            "kind_label": type(decl).__name__,
            "python_name": decl.python_name,
            "target": target(decl),
            "doc": decl.doc,
        }
        if kind == "interface":
            view.update(
                fqn=decl.fqn,
                version=decl.version,
                transport=decl.transport.value,
                strict=decl.strict,
                extends=[named(x.python_name) for x in decl.extends],
                inherited=[
                    inherited_view(m)
                    for m in decl.all_methods()
                    if m.interface is not decl
                ],
                methods=[method_view(decl, m) for m in decl.methods],
            )
        elif kind == "object":
            view.update(
                fqn=decl.fqn,
                transport=decl.transport.value,
                implements=[named(i.python_name) for i in decl.implements],
            )
        elif kind == "record":
            view.update(
                name=decl.name,
                size=decl.type.size,
                alignment=decl.type.align,
                caps=decl.type.caps,
                fields=[
                    {"name": f.name, "type": type_view(f.type), "offset": offset}
                    for f, offset in zip(decl.type.fields, decl.type.offsets)
                ],
            )
        elif kind in ("enumdecl", "flagsdecl"):
            tagged = isinstance(decl.type, TaggedEnumType)
            variants = (
                [(n, v, type_view(t)) for n, v, t in decl.type.variants]
                if tagged
                else [(n, v, None) for n, v in decl.variants]
            )
            view.update(
                name=decl.name,
                base=type_view(decl.base),
                tagged=tagged,
                variants=[
                    {"name": n, "value": v, "payload": p} for n, v, p in variants
                ],
            )
        elif kind == "capset":
            slots = dict(decl.type.slots)
            view.update(
                name=decl.name,
                fields=[
                    {"name": f.name, "type": type_view(f.type), "slot": slots[f.name]}
                    for f in decl.type.fields
                ],
            )
        elif kind in ("ringdecl", "regiondecl"):
            view.update(
                name=decl.name,
                element=type_view(decl.type.element),
                capacity=decl.type.capacity,
            )
        return view

    return {
        "name": name,
        "doc": schema.doc,
        "declarations": [declaration_view(d) for d in all_declarations if owned(d)],
    }


def reference_data(schemas: list[tuple[str, Schema]]) -> dict:
    """Describe schemas and their cross-links for a reference consumer."""
    source_schemas = {
        schema.source_path: name for name, schema in schemas if schema.source_path
    }
    return {
        "schemas": [
            {
                "name": name,
                "schema": schema_view(name, schema, source_schemas),
            }
            for name, schema in schemas
        ]
    }
