use std::{env, fs, path::PathBuf};

fn main() {
    horn_idl_build::rust("idl_demo", "protocol.rs");
    for (variable, name) in [
        ("CHILD_ELF", "child.elf"),
        ("LAUNCH_ELF", "launched.elf"),
        ("SMP_ELF", "smp_worker.elf"),
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        let path = env::var(variable).expect("ELF input is not set");
        println!("cargo:rerun-if-changed={path}");
        fs::copy(path, PathBuf::from(env::var("OUT_DIR").unwrap()).join(name))
            .expect("copy ELF input");
    }
}
