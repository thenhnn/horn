use std::{env, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let idl = env::var_os("HORN_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../tools/idl"));
    let schema = manifest.join("idl/horn_process.py");
    let vm_idl = env::var_os("HORN_VM_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../vm/idl"));
    println!("cargo:rerun-if-env-changed=HORN_VM_IDL");
    println!("cargo:rerun-if-changed={}", vm_idl.display());
    println!("cargo:rerun-if-env-changed=HORN_IDL");
    println!("cargo:rerun-if-changed={}", idl.display());
    println!("cargo:rerun-if-changed={}", schema.display());
    let status = Command::new("python3")
        .args(["-B", "-m", "horn_idl"])
        .arg(schema)
        .arg("--rust")
        .arg(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("protocol.rs"))
        .env("PYTHONPATH", env::join_paths([idl, vm_idl]).unwrap())
        .status()
        .expect("run IDL compiler");
    assert!(status.success(), "process IDL compilation failed");
}
