use std::{env, fs, path::PathBuf};
fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for key in [
        "DEVMGR_DRIVER_CATALOG",
        "HORN_CLOCK_ROUTE",
        "DEVMGR_PROBE_ELF",
        "DEVMGR_TEST_FAULT",
        "DEVMGR_TEST_EARLY_FAULT",
        "DEVMGR_TEST_NO_READY",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    if let Some(path) = env::var_os("DEVMGR_DRIVER_CATALOG") {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, out.join("drivers.rs")).unwrap();
    } else {
        fs::write(
            out.join("drivers.rs"),
            "static DRIVERS: &[DriverConfig] = &[];",
        )
        .unwrap();
    }
    if env::var_os("CARGO_FEATURE_BOOT_TESTS").is_none() {
        return;
    }
    horn_idl_build::rust("devmgr_probe", "probe_layout.rs");
    if let Some(path) = env::var_os("DEVMGR_PROBE_ELF").filter(|p| !p.is_empty()) {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, out.join("probe.elf")).unwrap();
    } else {
        fs::write(out.join("probe.elf"), []).unwrap();
    }
}
