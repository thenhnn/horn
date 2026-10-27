use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=MIMALLOC_LIB");
    let library = PathBuf::from(
        env::var_os("MIMALLOC_LIB")
            .expect("MIMALLOC_LIB must name the vendored native archive directory"),
    );
    println!("cargo:rustc-link-search=native={}", library.display());
    println!("cargo:rustc-link-lib=static=horn-mimalloc");
}
