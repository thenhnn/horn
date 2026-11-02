use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=HORN_LARGE_IMAGES");
    println!("cargo:rustc-check-cfg=cfg(large_images)");
    if env::var("HORN_LARGE_IMAGES").as_deref() == Ok("1") {
        println!("cargo:rustc-cfg=large_images");
    }
    for key in [
        "SUPERVISOR_ELF",
        "DEVMGR_TEST_VTD",
        "DEVMGR_TEST_STALL",
        "ROOT_STARTUP_TIMEOUT_US",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let source = env::var("SUPERVISOR_ELF").expect("SUPERVISOR_ELF");
    println!("cargo:rerun-if-changed={source}");
    fs::copy(
        source,
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("supervisor.elf"),
    )
    .unwrap();
}
