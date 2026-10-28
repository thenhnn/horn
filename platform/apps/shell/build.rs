use std::{env, path::PathBuf, process::Command};
fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = [
        ("HORN_IDL", "../../../tools/idl"),
        ("HORN_VM_IDL", "../../core/vm/idl"),
        ("HORN_PROCESS_IDL", "../../core/procd/idl"),
        ("HORN_DEVICE_IDL", "../../core/devmgr/idl"),
        ("HORN_CLOCK_IDL", "../../core/clock/idl"),
        ("HORN_CONSOLE_IDL", "../../core/console/idl"),
    ]
    .map(|(key, fallback)| {
        println!("cargo:rerun-if-env-changed={key}");
        let path = env::var_os(key)
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest.join(fallback));
        println!("cargo:rerun-if-changed={}", path.display());
        path
    });
    println!("cargo:rerun-if-changed=idl/horn_shell.py");
    assert!(
        Command::new("python3")
            .args(["-B", "-m", "horn_idl"])
            .arg(manifest.join("idl/horn_shell.py"))
            .arg("--rust")
            .arg(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("protocol.rs"))
            .env("PYTHONPATH", env::join_paths(paths).unwrap())
            .status()
            .unwrap()
            .success()
    );
}
