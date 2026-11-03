"""Build plumbing: named schemas and real Cargo transitive invalidation."""

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_named_schema_without_consumer_import_paths(tmp_path):
    env = dict(os.environ, PYTHONPATH=str(ROOT / "tools/idl"))
    env.pop("HORN_IDL_SOURCES", None)
    for name in ("horn_shell", "horn_device"):
        subprocess.run(
            [
                "python3",
                "-B",
                "-m",
                "horn_idl",
                name,
                "--rust",
                str(tmp_path / f"{name}.rs"),
                "--capsets",
                str(tmp_path / f"{name}.json"),
            ],
            cwd=tmp_path,
            env=env,
            check=True,
            capture_output=True,
        )
        assert (tmp_path / f"{name}.rs").stat().st_size > 0
        # RPC-only entry schemas need not declare a bootstrap capset.
        assert isinstance(json.loads((tmp_path / f"{name}.json").read_text()), dict)


def test_cargo_rebuilds_for_transitive_schema_edit(tmp_path):
    schemas = tmp_path / "schemas"
    schemas.mkdir()
    dependency = schemas / "dependency.py"
    template = "from horn_idl import record, U32, U64\n@record\nclass Payload:\n    value: {}\n"
    dependency.write_text(template.format("U32"))
    (schemas / "entry.py").write_text("from dependency import Payload\n")
    (schemas / "unused.py").write_text("raise AssertionError('not a dependency')\n")
    catalog = schemas / "sources.json"
    catalog.write_text(
        json.dumps(
            {
                "root": ".",
                "schemas": {
                    "entry": "entry.py",
                    "unused": "unused.py",
                },
            }
        )
    )
    (tmp_path / "src").mkdir()
    (tmp_path / "src/lib.rs").write_text("#![no_std]\n")
    (tmp_path / "Cargo.toml").write_text(f'''[package]
name = "idl-rebuild-check"
version = "0.1.0"
edition = "2024"
[build-dependencies]
horn-idl-build = {{path = "{ROOT / "tools/idl/build"}"}}
[workspace]
''')
    (tmp_path / "build.rs").write_text("""fn main() {
    horn_idl_build::rust("entry", "protocol.rs");
    std::fs::copy(std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("protocol.rs"), "snapshot.rs").unwrap();
}
""")
    env = dict(
        os.environ,
        HORN_IDL=str(ROOT / "tools/idl"),
        HORN_IDL_SOURCES=str(catalog),
        CARGO_TARGET_DIR=str(tmp_path / "target"),
    )
    command = [
        "cargo",
        "check",
        "-vv",
        "--offline",
        "--manifest-path",
        str(tmp_path / "Cargo.toml"),
    ]
    first = subprocess.run(
        command, cwd=tmp_path, env=env, check=True, capture_output=True
    )
    before = (tmp_path / "snapshot.rs").read_bytes()
    # A second invocation must actually stay fresh, not rebuild unconditionally.
    stamp = (tmp_path / "snapshot.rs").stat().st_mtime_ns
    subprocess.run(command, cwd=tmp_path, env=env, check=True, capture_output=True)
    assert (tmp_path / "snapshot.rs").stat().st_mtime_ns == stamp
    dependency.write_text(template.format("U64"))
    subprocess.run(command, cwd=tmp_path, env=env, check=True, capture_output=True)
    assert (tmp_path / "snapshot.rs").read_bytes() != before
    output = (first.stdout + first.stderr).decode()
    assert f"cargo:rerun-if-changed={dependency}" in output
    assert f"cargo:rerun-if-changed={schemas / 'unused.py'}" not in output
    # Invalid imported input fails the build rather than retaining stale bindings.
    dependency.write_text("raise RuntimeError('broken dependency')\n")
    failed = subprocess.run(
        command, cwd=tmp_path, env=env, capture_output=True, check=False
    )
    assert failed.returncode != 0
    assert b"broken dependency" in failed.stderr
