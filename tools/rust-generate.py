#!/usr/bin/env python3
"""Regenerate crate2nix graphs and compiler cfg data; run inside nix-shell."""

import hashlib
import json
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUST = ROOT / "nix/rust"
TARGETS = ROOT / "vendor/rust-sel4/src/support/targets"
HOSTS = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
]
SEL4 = [
    f"{arch}-sel4{suffix}"
    for arch in ["x86_64", "aarch64", "riscv64imac"]
    for suffix in ["", "-roottask"]
]


def run(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def generate(manifest, output):
    subprocess.run(
        ["crate2nix", "generate", "-f", str(manifest), "-o", str(output)],
        cwd=ROOT,
        check=True,
    )
    text = output.read_text()
    # crate2nix 0.15 compares most cfg values as scalars. Rust cfg can have
    # several values (e.g. target_has_atomic), and target features differ
    # between native proc macros and the bare-metal target.
    quoted = r'"(?:[^"\\]|\\.)*"'
    text = re.sub(
        rf"\(({quoted}) == target\.({quoted}) or null\)",
        r"(builtins.elem \1 (lib.toList (target.\2 or [])))",
        text,
    )
    text = re.sub(
        rf"\(builtins.elem ({quoted}) targetFeatures\)",
        r"(builtins.elem \1 (target.feature or []))",
        text,
    )
    text = re.sub(
        rf"\(target\.({quoted}) or false\)",
        r"(builtins.elem \1 target.__flags)",
        text,
    )
    output.write_text(text)


def compiler_cfg(target, release):
    spec = TARGETS / f"{target}.json"
    lines = run(
        "rustc",
        "-Zunstable-options",
        "--print=cfg",
        "--target",
        str(spec) if spec.exists() else target,
        "-C",
        f"opt-level={3 if release else 0}",
    ).splitlines()
    cfg = {"flags": [], "values": {}}
    for line in lines:
        key, sep, value = line.partition("=")
        if sep:
            cfg["values"].setdefault(key, []).append(json.loads(value))
        else:
            cfg["flags"].append(key)
    return cfg


def main():
    pinned = json.loads(
        run(
            "nix-instantiate",
            "--eval",
            "--strict",
            "--json",
            "--expr",
            "let r = (import ./nix {}).toolchain.rust; in { path = r.outPath; version = r.version; }",
        )
    )
    sysroot = Path(run("rustc", "--print=sysroot")).resolve()
    if sysroot != Path(pinned["path"]).resolve():
        raise SystemExit(
            "Run just rust-generate inside nix-shell with the pinned Rust toolchain."
        )
    library = (sysroot / "lib/rustlib/src/rust/library").resolve()
    scratch = ROOT / "work/rust-sysroot"
    (scratch / "src").mkdir(parents=True, exist_ok=True)
    dependencies = {
        "core": {"path": "core"},
        "alloc": {"path": "alloc", "features": ["compiler-builtins-mem"]},
        "compiler_builtins": {
            "path": "compiler-builtins/compiler-builtins",
            "features": ["compiler-builtins", "mem"],
        },
    }
    manifest = '[package]\nname = "horn-sysroot"\nversion = "0.1.0"\nedition = "2024"\n'
    manifest += '\n[workspace]\nresolver = "3"\n\n[dependencies]\n'
    for name, dependency in dependencies.items():
        fields = [f"path = {json.dumps(str(library / dependency['path']))}"]
        if "features" in dependency:
            fields.append(f"features = {json.dumps(dependency['features'])}")
        manifest += name + " = { " + ", ".join(fields) + " }\n"
    (scratch / "Cargo.toml").write_text(manifest)
    (scratch / "src/lib.rs").write_text("#![no_std]\n")
    subprocess.run(
        ["cargo", "generate-lockfile", "--manifest-path", str(scratch / "Cargo.toml")],
        cwd=ROOT,
        check=True,
    )

    generate(Path("Cargo.toml"), Path("Cargo.nix"))
    output = RUST / "sysroot.nix"
    generate(scratch.relative_to(ROOT) / "Cargo.toml", output.relative_to(ROOT))
    text = output.read_text().replace(
        "{ nixpkgs ? <nixpkgs>", "{ rustLibrary\n, nixpkgs ? <nixpkgs>"
    )
    for dependency in dependencies.values():
        path = dependency["path"]
        relative = os.path.relpath(library / path, output.parent)
        text = text.replace(f"src = {relative};", f'src = (rustLibrary + "/{path}");')
    # This package only seeds dependency resolution; it is never compiled.
    text = text.replace(
        f"src = {os.path.relpath(scratch, output.parent)};", "src = ./.;"
    )
    # The invocation comment may mention the scratch manifest, but sources must not.
    body = "\n".join(line for line in text.splitlines() if not line.startswith("#"))
    if "nix/store/" in body or "work/rust-sysroot" in body:
        raise ValueError("Unnormalized sysroot source paths")
    output.write_text(text)

    cfg = {
        "toolchainVersion": pinned["version"],
        "compiler": run("rustc", "--version"),
        "targets": {},
    }
    for target in HOSTS + SEL4:
        spec = TARGETS / f"{target}.json"
        cfg["targets"][target] = {
            "specHash": hashlib.sha256(spec.read_bytes()).hexdigest()
            if spec.exists()
            else None,
            "dev": compiler_cfg(target, False),
            "release": compiler_cfg(target, True),
        }
    (RUST / "cfg.json").write_text(json.dumps(cfg, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
