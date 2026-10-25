use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=blobs");
    println!("cargo:rerun-if-changed=chips.json");
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("blobs");
    let mut paths: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "fw"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no firmware blobs in {}", root.display());
    let blobs: Vec<_> = paths
        .iter()
        .map(|path| {
            rtl_compiler::parser::parse_fw_blob(
                path.file_stem().unwrap().to_str().unwrap().into(),
                &std::fs::read(path).unwrap(),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect();
    let generated = rtl_compiler::emit::emit_module(&blobs).unwrap();
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("firmware.rs");
    std::fs::write(output, generated.to_string()).unwrap();
    let metadata = root.parent().unwrap().join("chips.json");
    let chips =
        rtl_compiler::chips::emit_module(&std::fs::read_to_string(metadata).unwrap(), &blobs)
            .unwrap();
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("chips.rs");
    std::fs::write(output, chips.to_string()).unwrap();
}
