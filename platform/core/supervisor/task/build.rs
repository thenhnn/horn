use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=HORN_SERVICES");
    println!("cargo:rerun-if-env-changed=SUPERVISOR_STALL");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("services.rs");
    if let Some(path) = env::var_os("HORN_SERVICES") {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, out).unwrap();
    } else {
        fs::write(out, "pub static SERVICES: &[ServiceDefinition] = &[];").unwrap();
    }
}
