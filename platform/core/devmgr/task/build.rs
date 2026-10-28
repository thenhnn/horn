use std::{env, fs, path::PathBuf};
fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for key in [
        "DEVMGR_DRIVER_ELF",
        "DEVMGR_PROBE_ELF",
        "DEVMGR_DRIVER_NAME",
        "DEVMGR_DRIVER_DEVICE",
        "DEVMGR_DRIVER_RAM",
        "DEVMGR_TEST_FAULT",
        "DEVMGR_TEST_EARLY_FAULT",
        "DEVMGR_TEST_NO_READY",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let mut drivers = String::from("static DRIVERS:&[DriverConfig]=&[");
    if let Some(path) = env::var_os("DEVMGR_DRIVER_ELF").filter(|p| !p.is_empty()) {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, out.join("driver.elf")).unwrap();
        let name = env::var("DEVMGR_DRIVER_NAME").unwrap();
        let device = env::var("DEVMGR_DRIVER_DEVICE")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let ram = env::var("DEVMGR_DRIVER_RAM")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        drivers.push_str(&format!("DriverConfig{{name:{name:?},device:{device},ram:{ram},elf:include_bytes!(concat!(env!(\"OUT_DIR\"),\"/driver.elf\"))}},"));
    }
    drivers.push_str("];");
    fs::write(out.join("drivers.rs"), drivers).unwrap();
    if env::var_os("CARGO_FEATURE_BOOT_TESTS").is_none() {
        return;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = [
        ("HORN_IDL", "../../../../tools/idl"),
        ("HORN_VM_IDL", "../../vm/idl"),
        ("HORN_DEVICE_IDL", "../idl"),
    ]
    .map(|(key, fallback)| {
        println!("cargo:rerun-if-env-changed={key}");
        let path = env::var_os(key)
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest.join(fallback));
        println!("cargo:rerun-if-changed={}", path.display());
        path
    });
    println!("cargo:rerun-if-env-changed=HORN_DEVICE_TEST_IDL");
    let schema = env::var_os("HORN_DEVICE_TEST_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../../tests/boot/devmgr-probe/schema.py"));
    println!("cargo:rerun-if-changed={}", schema.display());
    assert!(
        std::process::Command::new("python3")
            .args(["-B", "-m", "horn_idl"])
            .arg(schema)
            .arg("--rust")
            .arg(out.join("probe_layout.rs"))
            .env("PYTHONPATH", env::join_paths(paths).unwrap())
            .status()
            .unwrap()
            .success()
    );
    if let Some(path) = env::var_os("DEVMGR_PROBE_ELF").filter(|p| !p.is_empty()) {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, out.join("probe.elf")).unwrap();
    } else {
        fs::write(out.join("probe.elf"), []).unwrap();
    }
}
