use std::{env, fs, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=DEVMGR_ELF");
    for key in [
        "CONSOLE_ELF",
        "DEVMGR_TEST_VTD",
        "DEVMGR_TEST_STALL",
        "DEVMGR_TEST_FAULT",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let target = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("virtio_console.elf");
    if let Some(path) = env::var_os("CONSOLE_ELF") {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, target).unwrap();
    } else {
        fs::write(target, []).unwrap();
    }
    let path = env::var("DEVMGR_ELF").expect("DEVMGR_ELF");
    println!("cargo:rerun-if-changed={path}");
    fs::copy(
        path,
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("devmgr.elf"),
    )
    .unwrap();
}
