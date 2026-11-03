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


def test_owned_interface_lifetimes_compile(tmp_path):
    """Compile positive and compile-fail fixtures; never execute a Rust binary."""
    env = dict(
        os.environ,
        PYTHONPATH=str(ROOT / "tools/idl"),
        CARGO_TARGET_DIR=str(tmp_path / "target"),
    )
    src = tmp_path / "src"
    src.mkdir()
    (src / "lib.rs").write_text((ROOT / "tests/idl/owned.rs").read_text())
    subprocess.run(
        [
            "python3",
            "-B",
            "-m",
            "horn_idl",
            str(ROOT / "examples/idl/tagged.py"),
            "--rust",
            str(src / "protocol.rs"),
        ],
        env=env,
        check=True,
        capture_output=True,
    )
    failures = {
        "reject_outlive": "E0505",
        "reject_copy": "E0382",
        "reject_send": "E0277",
        "reject_borrowed_send": "E0277",
        "reject_erase": "E0599",
    }
    dependency = json.dumps(str(ROOT / "platform/runtime/rust/horn-ipc"))
    features = "\n".join(f"{name} = []" for name in failures)
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "owned-compile"\nversion = "0.1.0"\nedition = "2024"\n'
        f"[dependencies]\nhorn-ipc = {{ path = {dependency} }}\n"
        f"[features]\n{features}\n[workspace]\n"
    )
    command = [
        "cargo",
        "check",
        "--offline",
        "--manifest-path",
        str(tmp_path / "Cargo.toml"),
    ]
    subprocess.run(command, env=env, check=True, capture_output=True)
    for feature, code in failures.items():
        result = subprocess.run(
            command + ["--features", feature], env=env, check=False, capture_output=True
        )
        assert result.returncode != 0
        assert code.encode() in result.stderr, result.stderr.decode()
