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
    let memory_idl = env::var_os("HORN_MEMORY_IDL").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../platform/core/memory/idl"));
    println!("cargo:rerun-if-env-changed=HORN_MEMORY_IDL");
    println!("cargo:rerun-if-changed={}", memory_idl.display());
    let status = Command::new(python)
        .arg("-m")
        .arg("horn_idl")
        .arg(schema)
        .arg("--rust")
        .arg(output)
        .env("PYTHONPATH", format!("{}:{}", idl.display(), memory_idl.display()))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .status()
        .expect("run Python IDL generator");
    assert!(status.success(), "IDL generation failed for {}", schema.display());
}

fn main() {
    generate_protocol();
}
