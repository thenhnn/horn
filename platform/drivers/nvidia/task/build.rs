fn main() {
    println!("cargo:rustc-check-cfg=cfg(gsp_boot)");
    println!("cargo:rerun-if-env-changed=NVIDIA_FIRMWARE_DIR");
    if let Ok(directory) = std::env::var("NVIDIA_FIRMWARE_DIR") {
        let mut generated = String::new();
        for (name, file) in [
            ("IMAGE", "gsp.bin"),
            ("META", "gsp.tlv"),
            ("BOOTLOADER", "gsp_bootloader.tlv"),
            ("LOAD", "booter_load.tlv"),
            ("UNLOAD", "booter_unload.tlv"),
        ] {
            let path = std::path::Path::new(&directory).join(file);
            println!("cargo:rerun-if-changed={}", path.display());
            generated.push_str(&format!(
                "pub static {name}: &[u8] = include_bytes!({:?});\n",
                path
            ));
        }
        std::fs::write(
            std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("images.rs"),
            generated,
        )
        .unwrap();
        println!("cargo:rustc-cfg=gsp_boot");
    }
    horn_idl_build::rust("nvidia_probe", "protocol.rs");
}
