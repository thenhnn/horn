from __future__ import annotations

import argparse
import json
from pathlib import Path

from .compile import compile_path
from .emit.c import emit as emit_c
from .emit.reference import reference_data
from .emit.rust import emit as emit_rust
from .sources import cargo_dependencies, configure_sources


def write_reference(path: Path, schemas) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(reference_data(schemas), indent=2) + "\n", encoding="utf-8"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description="Compile a Horn IDL schema")
    parser.add_argument(
        "schema", type=Path, nargs="?", help="Schema path or catalog name"
    )
    parser.add_argument("--rust", type=Path)
    parser.add_argument("--header", type=Path)
    parser.add_argument("--c", type=Path)
    parser.add_argument(
        "--capsets", type=Path, help="Export named bootstrap slots as JSON"
    )
    parser.add_argument(
        "--cargo-deps", action="store_true", help="Print Cargo rebuild inputs"
    )
    parser.add_argument(
        "--doc-json",
        type=Path,
        help="Export reference data as JSON (all catalog schemas if schema is omitted)",
    )
    args = parser.parse_args()

    catalog, sources = configure_sources()
    if args.schema is None:
        if not args.doc_json:
            parser.error("a schema or --doc-json is required")
        if args.rust or args.header or args.c or args.capsets or args.cargo_deps:
            parser.error("code generation requires a schema")
        schemas = [(name, compile_path(path)) for name, path in sorted(sources.items())]
        write_reference(args.doc_json, schemas)
        return

    path = sources.get(str(args.schema), args.schema)
    schema = compile_path(path)
    if args.doc_json:
        name = str(args.schema) if str(args.schema) in sources else path.stem
        write_reference(args.doc_json, [(name, schema)])
    if args.capsets:
        args.capsets.parent.mkdir(parents=True, exist_ok=True)
        args.capsets.write_text(
            json.dumps(
                {
                    capset.python_name: {
                        "slots": dict(capset.type.slots),
                        "types": {
                            field.name: field.type.target_name()
                            for field in capset.type.fields
                        },
                        "capacity": capset.type.capacity,
                    }
                    for capset in schema.capsets
                },
                sort_keys=True,
            )
        )
    if args.rust:
        args.rust.parent.mkdir(parents=True, exist_ok=True)
        args.rust.write_text(emit_rust(schema))
    if args.header or args.c:
        header, source = emit_c(schema)
        if args.header:
            args.header.parent.mkdir(parents=True, exist_ok=True)
            args.header.write_text(header)
        if args.c:
            args.c.parent.mkdir(parents=True, exist_ok=True)
            args.c.write_text(source)
    if args.cargo_deps:
        cargo_dependencies(catalog, sources, path)


if __name__ == "__main__":
    main()
