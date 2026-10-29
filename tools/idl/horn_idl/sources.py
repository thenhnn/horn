"""Shared schema locations for local Cargo, CLI tools and sandboxed Nix builds."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def configure_sources() -> tuple[Path, dict[str, Path]]:
    catalog = Path(
        os.environ.get("HORN_IDL_SOURCES", Path(__file__).parents[1] / "sources.json")
    ).resolve()
    data = json.loads(catalog.read_text())
    root = catalog.parent / data["root"]
    sources = {name: (root / path).resolve() for name, path in data["schemas"].items()}
    # Only schema directories, not the repository or a consumer-maintained
    # transitive dependency list. Normal Python imports remain normal imports.
    directories = sorted({str(path.parent) for path in sources.values()})
    sys.path[:0] = directories
    return catalog, sources


def cargo_dependencies(catalog: Path, sources: dict[str, Path], schema: Path) -> None:
    directories = {path.parent for path in sources.values()} | {schema.resolve().parent}
    dependencies = {catalog, schema.resolve(), Path(__file__).parent}
    for module in tuple(sys.modules.values()):
        filename = getattr(module, "__file__", None)
        if filename:
            path = Path(filename).resolve()
            if any(path.is_relative_to(directory) for directory in directories):
                dependencies.add(path)
    for path in sorted(dependencies):
        print(f"cargo:rerun-if-changed={path}")
