use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=DEVMGR_ELF");
    let path = env::var("DEVMGR_ELF").expect("DEVMGR_ELF");
    println!("cargo:rerun-if-changed={path}");
    fs::copy(
        path,
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("devmgr.elf"),
    )
    .unwrap();
}
