use std::{env, path::PathBuf, process::Command};
fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let idl = env::var_os("HORN_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../tools/idl"));
    println!("cargo:rerun-if-env-changed=HORN_IDL");
    println!("cargo:rerun-if-changed={}", idl.display());
    let schema = manifest.join("idl/horn_net.py");
    println!("cargo:rerun-if-changed={}", schema.display());
    assert!(
        Command::new("python3")
            .args(["-B", "-m", "horn_idl"])
            .arg(schema)
            .arg("--rust")
            .arg(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("protocol.rs"))
            .env("PYTHONPATH", idl)
            .status()
            .unwrap()
            .success()
    );
}
