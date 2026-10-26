use std::{env, fs, path::PathBuf};

use object::{Object, ObjectSection, ObjectSegment, SegmentFlags};

fn main() {
    println!("cargo:rerun-if-env-changed=CHILD_ELF");
    let child_path = env::var("CHILD_ELF").expect("CHILD_ELF is not set");
    let data = fs::read(&child_path).expect("read child ELF");
    let image = object::File::parse(&*data).expect("parse child ELF");

    let horn = image.section_by_name(".horn").expect("child has no .horn tag");
    let (tag_offset, tag_size) = horn.file_range().expect(".horn has no file range");
    let tag = horn.data().expect(".horn has no data");
    assert!(tag.len() >= 4 && &tag[..4] == b"HORN", "bad HornTag magic");

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
