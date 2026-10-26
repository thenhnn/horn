use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use object::{Object, ObjectSection, ObjectSegment, SegmentFlags};

fn generate_protocol() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let schema = env::var_os("HORN_SCHEMA")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../schema.py"));
    let idl = env::var_os("HORN_IDL")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../../tools/idl"));
    let output = out.join("protocol.rs");
    run_python(&schema, &idl, &output);
    println!("cargo:rerun-if-changed={}", schema.display());
    println!("cargo:rerun-if-changed={}", idl.display());
}

fn run_python(schema: &Path, idl: &Path, output: &Path) {
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let memory_idl = env::var_os("HORN_MEMORY_IDL").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../platform/core/memory/idl"));
    println!("cargo:rerun-if-env-changed=HORN_MEMORY_IDL");
    println!("cargo:rerun-if-changed={}", memory_idl.display());
    let status = Command::new(python)
        .arg("-m")
        .arg("horn_idl")
        .arg(schema)
        .arg("--rust")
        .arg(output)
        .env("PYTHONPATH", format!("{}:{}", idl.display(), memory_idl.display()))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .status()
        .expect("run Python IDL generator");
    assert!(status.success(), "IDL generation failed for {}", schema.display());
}

fn main() {
    generate_protocol();
    println!("cargo:rerun-if-env-changed=CHILD_ELF");
    let child_path = env::var("CHILD_ELF").expect("CHILD_ELF is not set");
    let data = fs::read(&child_path).expect("read child ELF");
    let image = object::File::parse(&*data).expect("parse child ELF");

    let horn = image.section_by_name(".horn").expect("child has no .horn tag");
    let (tag_offset, tag_size) = horn.file_range().expect(".horn has no file range");
    let tag = horn.data().expect(".horn has no data");
    assert!(tag.len() >= 8 && &tag[..4] == b"HORN", "bad HornTag magic");
    assert_eq!(
        u32::from_le_bytes(tag[4..8].try_into().unwrap()),
        1,
        "unsupported HornTag version"
    );

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("child.elf"), &data).unwrap();

    let mut code = String::new();
    code.push_str("pub struct ChildSegment { pub address: usize, pub size: usize, pub data: &'static [u8], pub flags: u32 }\n");
    code.push_str("pub struct ChildImage { pub entry: usize, pub segments: &'static [ChildSegment] }\n");
    code.push_str("static CHILD_ELF: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/child.elf\"));\n");
    code.push_str(&format!(
        "pub static HORN_TAG: &[u8] = &CHILD_ELF[{tag_offset}..{}];\n",
        tag_offset + tag_size
    ));
    code.push_str(&format!(
        "pub static CHILD_IMAGE: ChildImage = ChildImage {{ entry: {}, segments: &[\n",
        image.entry()
    ));
    for segment in image.segments() {
        let (offset, _) = segment.file_range();
        let data = segment.data().expect("segment data");
        let flags = match segment.flags() {
            SegmentFlags::Elf { p_flags, .. } => p_flags.0,
            _ => 0,
        };
        code.push_str(&format!(
            "    ChildSegment {{ address: {}, size: {}, data: &CHILD_ELF[{offset}..{}], flags: {} }},\n",
            segment.address(),
            segment.size(),
            offset + data.len() as u64,
            flags
        ));
    }
    code.push_str("] };\n");
    fs::write(out.join("child_image.rs"), code).unwrap();
}
