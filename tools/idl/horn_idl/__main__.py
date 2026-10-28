from __future__ import annotations

import argparse
import json
from pathlib import Path

from .compile import compile_path
from .emit.c import emit as emit_c
from .emit.rust import emit as emit_rust


def main() -> None:
    parser = argparse.ArgumentParser(description="Compile a Horn IDL schema")
    parser.add_argument("schema", type=Path)
    parser.add_argument("--rust", type=Path)
    parser.add_argument("--header", type=Path)
    parser.add_argument("--c", type=Path)
    parser.add_argument(
        "--capsets", type=Path, help="Export named bootstrap slots as JSON"
    )
    args = parser.parse_args()

    schema = compile_path(args.schema)
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


if __name__ == "__main__":
    main()
