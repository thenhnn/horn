use std::{env, fs, path::PathBuf};
fn main() {
    for key in ["SUPERVISOR_ELF", "DEVMGR_TEST_VTD", "DEVMGR_TEST_STALL", "ROOT_STARTUP_TIMEOUT_US"] { println!("cargo:rerun-if-env-changed={key}"); }
    let source = env::var("SUPERVISOR_ELF").expect("SUPERVISOR_ELF");
    println!("cargo:rerun-if-changed={source}");
    fs::copy(source, PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("supervisor.elf")).unwrap();
}
