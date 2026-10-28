use std::{env,fs,path::PathBuf};
fn main() {
    println!("cargo:rerun-if-env-changed=DEVMGR_ELF");
    println!("cargo:rerun-if-env-changed=SUPERVISOR_STALL");
    let out=PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("devmgr.elf");
    if let Some(path)=env::var_os("DEVMGR_ELF") {
        println!("cargo:rerun-if-changed={}",PathBuf::from(&path).display());
        fs::copy(path,out).unwrap();
    } else {fs::write(out,[]).unwrap();}
}
