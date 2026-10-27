use crate::{Error, elf::Image};
use alloc::vec;

#[test]
fn process_management_is_bounded_inline_and_preserves_exit_info() {
    use crate::protocol::*;
    use horn_ipc::{Decoder, Encoder, List, MAX_MESSAGE_BYTES, Wire, WireCaps};
    assert!(ProcessPage::SIZE + 8 <= MAX_MESSAGE_BYTES);
    assert_eq!(ProcessfactoryLaunchArgs::CAPS, 1);
    let mut name = List::default();
    name.len = 6;
    name.data[..6].copy_from_slice(b"driver");
    let info = ProcessInfo {
        id: 42,
        name,
        state: ProcessState::Exited,
        exit: Some(ProcessExit::Code(7)),
        ram: ProcessUsage {
            limit_bytes: 8192,
            charged_bytes: 0,
            quarantined_bytes: 0,
        },
    };
    let mut entries = List::default();
    entries.len = 4;
    entries.data.fill(info);
    let page = ProcessPage { entries, next: 42 };
    let mut bytes = vec![0; ProcessPage::SIZE];
    page.encode(&mut Encoder::new(&mut bytes));
    let decoded = ProcessPage::decode(&mut Decoder::new(&bytes, &mut WireCaps::default())).unwrap();
    assert_eq!(decoded, page);
}

// A static ELF64 with one RX page, one RW page, and the eight-byte Horn tag.
fn image() -> alloc::vec::Vec<u8> {
    let mut bytes = vec![0; 0x2c0];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (offset, value) in [
        (16, 2u16),
        (18, 62),
        (52, 64),
        (54, 56),
        (56, 2),
        (58, 64),
        (60, 3),
        (62, 1),
    ] {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    for (offset, value) in [(24, 0x400000u64), (32, 64), (40, 0x200)] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    for (header, address, flags, data) in
        [(64, 0x400000u64, 5u32, 0x100u64), (120, 0x401000, 6, 0x180)]
    {
        bytes[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
        bytes[header + 4..header + 8].copy_from_slice(&flags.to_le_bytes());
        for (offset, value) in [(8, data), (16, address), (32, 1), (40, 4096), (48, 1)] {
            bytes[header + offset..header + offset + 8].copy_from_slice(&value.to_le_bytes());
        }
    }
    bytes[0x188..0x190].copy_from_slice(b"HORN\x01\x00\x00\x00");
    bytes[0x190..0x1a1].copy_from_slice(b"\0.shstrtab\0.horn\0");
    for (header, name, kind, offset, size) in [
        (0x240, 1u32, 3u32, 0x190u64, 17u64),
        (0x280, 11, 1, 0x188, 8),
    ] {
        bytes[header..header + 4].copy_from_slice(&name.to_le_bytes());
        bytes[header + 4..header + 8].copy_from_slice(&kind.to_le_bytes());
        bytes[header + 24..header + 32].copy_from_slice(&offset.to_le_bytes());
        bytes[header + 32..header + 40].copy_from_slice(&size.to_le_bytes());
    }
    bytes
}

#[test]
fn elf_checks_abi_bounds_entry_and_page_level_wx() {
    let valid = image();
    let parsed = Image::parse(&valid, 4096..0x800000, 2).unwrap();
    assert_eq!(parsed.entry, 0x400000);
    assert_eq!(parsed.ipc_address, 0x402000);
    assert!(matches!(
        Image::parse(&valid, 4096..0x800000, 1),
        Err(Error::Resource(horn_vm::Error::QuotaExceeded))
    ));
    for (offset, data, expected) in [
        (0x18c, 2u64.to_le_bytes().to_vec(), Error::InvalidAbi),
        (68, 7u32.to_le_bytes().to_vec(), Error::Denied),
        (136, 0x400100u64.to_le_bytes().to_vec(), Error::Denied),
        (160, u64::MAX.to_le_bytes().to_vec(), Error::InvalidElf),
        (24, 0x401000u64.to_le_bytes().to_vec(), Error::InvalidElf),
    ] {
        let mut bad = valid.clone();
        bad[offset..offset + data.len()].copy_from_slice(&data);
        assert!(matches!(Image::parse(&bad, 4096..0x800000, 2), Err(error) if error == expected));
    }
}
