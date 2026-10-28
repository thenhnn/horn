use std::{env, path::PathBuf, process::Command};
fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = [
        ("HORN_IDL", "../../../tools/idl"),
        ("HORN_VM_IDL", "../../../platform/core/vm/idl"),
        ("HORN_DEVICE_IDL", "../../../platform/core/devmgr/idl"),
        ("HORN_CLOCK_IDL", "../../../platform/core/clock/idl"),
    ]
    .map(|(key, fallback)| {
        println!("cargo:rerun-if-env-changed={key}");
        let path = env::var_os(key)
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest.join(fallback));
        println!("cargo:rerun-if-changed={}", path.display());
        path
    });
    println!("cargo:rerun-if-changed=schema.py");
    assert!(
        Command::new("python3")
            .args(["-B", "-m", "horn_idl"])
            .arg(manifest.join("schema.py"))
            .arg("--rust")
            .arg(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("layout.rs"))
            .env("PYTHONPATH", env::join_paths(paths).unwrap())
            .status()
            .unwrap()
            .success()
    );
}
