// SPDX-License-Identifier: GPL-2.0-only
//! Validate the real, pinned bundle before it can be embedded into a native task.
use horn_nvidia_gsp::firmware::{Bundle, VERSION};
fn main() {
    let vbios = std::env::args_os().nth(2);
    let path = std::path::PathBuf::from(std::env::args_os().nth(1).expect("firmware directory"));
    let read = |name: &str| std::fs::read(path.join(name)).expect(name);
    let meta = read("gsp.tlv");
    let image = read("gsp.bin");
    let bl = read("gsp_bootloader.tlv");
    let load = read("booter_load.tlv");
    let unload = read("booter_unload.tlv");
    let bundle = Bundle::parse(&meta, &image, &bl, &load, &unload).expect("AD106 firmware bundle");
    let mut staging = vec![0u8; bundle.loader.blob.len()];
    bundle
        .loader
        .prepare(0, &mut staging)
        .expect("production loader signature");
    let mut staging = vec![0u8; bundle.unloader.blob.len()];
    bundle
        .unloader
        .prepare(0, &mut staging)
        .expect("production unloader signature");
    println!(
        "AD106_FIRMWARE_VALIDATED version={VERSION} image_bytes={} signatures={} bootloader={} load={} unload={}",
        bundle.image.len(),
        bundle.signatures.len(),
        bundle.bootloader.blob.len(),
        bundle.loader.blob.len(),
        bundle.unloader.blob.len()
    );
    if let Some(path) = vbios {
        let bytes = std::fs::read(path).expect("PROM snapshot");
        let fw =
            horn_nvidia_gsp::vbios::Fwsec::from_prom(&bytes).expect("production FWSEC descriptor");
        println!(
            "AD106_FWSEC_PARSED ucode_bytes={} signatures={:#x}",
            fw.ucode.len(),
            fw.signature_versions
        );
    }
}
