#!/usr/bin/env python3
"""Render website templates using the IDL compiler's reference JSON."""

import json
import sys
from pathlib import Path
from urllib.parse import quote

from jinja2 import Environment, FileSystemLoader, StrictUndefined, select_autoescape


def schema_filename(name: str) -> str:
    return quote(name, safe="") + ".html"


def schema_href(name: str) -> str:
    return quote(schema_filename(name), safe="")


def anchor_id(target: dict) -> str:
    return f"{target['kind']}-{target['name']}"


def reference_href(target: dict, current_schema: str) -> str:
    prefix = "" if target["schema"] == current_schema else schema_href(target["schema"])
    return prefix + "#" + quote(anchor_id(target), safe="")


def render_site(templates: Path, reference_path: Path, output: Path) -> None:
    env = Environment(
        loader=FileSystemLoader(templates),
        autoescape=select_autoescape(["html"]),
        undefined=StrictUndefined,
        trim_blocks=True,
        lstrip_blocks=True,
        keep_trailing_newline=True,
    )
    env.filters.update(
        anchor_id=anchor_id, reference_href=reference_href, schema_href=schema_href
    )
    reference = json.loads(reference_path.read_text(encoding="utf-8"))
    docs = output / "doc" / "idl"

    def render(destination: Path, template: str, **context) -> None:
        html = env.get_template(template).render(**context)
        destination.write_text(html, encoding="utf-8")

    render(output / "index.html", "home.html")
    render(docs / "index.html", "idl-index.html", entries=reference["schemas"])
    for entry in reference["schemas"]:
        render(
            docs / schema_filename(entry["name"]), "schema.html", schema=entry["schema"]
        )


if __name__ == "__main__":
    render_site(*(Path(arg) for arg in sys.argv[1:]))
