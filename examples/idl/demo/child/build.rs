use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn generate_protocol() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let schema = env::var_os("HORN_SCHEMA")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../schema.py"));
    let idl = env::var_os("HORN_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../../tools/idl"));
    let output = out.join("protocol.rs");
    run_python(&schema, &idl, &output);
    println!("cargo:rerun-if-changed={}", schema.display());
    println!("cargo:rerun-if-changed={}", idl.display());
}

fn run_python(schema: &Path, idl: &Path, output: &Path) {
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let status = Command::new(python)
        .arg("-m")
        .arg("horn_idl")
        .arg(schema)
        .arg("--rust")
        .arg(output)
        .env("PYTHONPATH", idl.display().to_string())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .status()
        .expect("run Python IDL generator");
    assert!(
        status.success(),
        "IDL generation failed for {}",
        schema.display()
    );
}

fn main() {
    generate_protocol();
}
