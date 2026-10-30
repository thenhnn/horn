// SPDX-License-Identifier: GPL-2.0-only
//! Small byte-layout/bounds checks; no GPU/RPC peer or readiness simulation.
use crate::{Error, abi, boot::*, firmware::GSP_BYTES, wire::*};
use alloc::vec;
#[test]
fn real_image_radix_geometry_and_grant_envelopes() {
    let bytes = radix_bytes(GSP_BYTES).unwrap();
    assert_eq!(bytes, 33 * PAGE);
    let mut tables = vec![0; bytes];
    let image = Span::new(0x100000, GSP_BYTES).unwrap();
    let table = Span::new(0x5000000, bytes).unwrap();
    radix3(image, table, &mut tables).unwrap();
    let word = |offset| u64::from_le_bytes(tables[offset..offset + 8].try_into().unwrap());
    assert_eq!(word(0), table.address() + PAGE as u64);
    assert_eq!(word(PAGE + 30 * 8), table.address() + 32 * PAGE as u64);
    assert_eq!(
        word(2 * PAGE + (GSP_BYTES / PAGE - 1) * 8),
        image.address() + (GSP_BYTES - PAGE) as u64
    );
    assert!(
        tables[2 * PAGE + (GSP_BYTES / PAGE) * 8..]
            .iter()
            .all(|v| *v == 0)
    );
    assert!(Span::new((1 << 47) - 4096, 8192).is_err());
    assert_eq!(
        radix3(
            image,
            Span::new(image.address(), bytes).unwrap(),
            &mut tables
        ),
        Err(Error::Range)
    );
}
#[test]
fn queue_self_map_stops_at_committed_pages() {
    let mut bytes = vec![0; QUEUE_BYTES];
    queues(Span::new(0x100000, QUEUE_BYTES).unwrap(), &mut bytes).unwrap();
    assert!(bytes[QUEUE_PAGES * 8..PAGE].iter().all(|v| *v == 0));
    assert_eq!(
        u32::from_le_bytes(bytes[CPU_HEADER + 12..CPU_HEADER + 16].try_into().unwrap()),
        63
    );
    let mut args = [0; PAGE];
    rmargs(Span::new(0x100000, QUEUE_BYTES).unwrap(), &mut args).unwrap();
    assert_eq!(
        u64::from_le_bytes(args[24..32].try_into().unwrap()),
        (GSP_DATA - PAGE) as u64
    );
    assert_eq!(core::mem::size_of::<abi::GspFwWprMeta>(), 256);
    let layout = Framebuffer::from_registers(8192, 0, 0).unwrap();
    assert_eq!(layout.frts().end, layout.bytes() - (1 << 20));
    assert!(layout.wpr().start < layout.frts().start);
    assert!(Framebuffer::from_registers(u32::MAX, 0, 0).is_err());
}
#[test]
fn frame_bounds_checksum_and_distinct_sequences() {
    let mut out = vec![0; HEADER + 3];
    let n = encode(&mut out, STATIC_INFO, 17, 3, [b"abc", &[]]).unwrap();
    let packet = Packet::decode(&out[..n]).unwrap();
    assert_eq!(packet.header.transport_sequence, 17);
    assert_eq!(packet.header.rpc_sequence, 3);
    assert_eq!(packet.payload, b"abc");
    assert_eq!(packet.success(), Err(Error::FirmwareStatus(crate::FirmwareStatus(u32::MAX))));
    out[HEADER] ^= 1;
    assert!(matches!(Packet::decode(&out), Err(Error::Checksum)));
    let mut out = vec![0; HEADER];
    encode(&mut out, INIT_DONE, 0, 0, [&[], &[]]).unwrap();
    out[40..44].copy_from_slice(&2u32.to_le_bytes());
    assert!(matches!(Header::parse(&out), Err(Error::Malformed)));
}
#[test]
fn continuations_do_not_split_fixed_fields() {
    let variable = vec![0; MAX_PAYLOAD];
    let mut chunks = Chunks::new(STATIC_INFO, b"fixed", &variable).unwrap();
    let first = chunks.next().unwrap();
    assert_eq!(first.parts[0], b"fixed");
    assert_eq!(first.parts[1].len(), MAX_PAYLOAD - 5);
    let second = chunks.next().unwrap();
    assert_eq!(second.function, CONTINUATION);
    assert_eq!(second.parts[1].len(), 5);
    assert!(chunks.next().is_none());
    assert!(Chunks::new(STATIC_INFO, &vec![0; MAX_PAYLOAD + 1], &[]).is_err());
}

/// Bytes captured from the physical AD106 PROM: the PCI data structure length
/// is 0x18 (24), not 26+, and NOVA never validates that field.
#[test]
fn real_ad106_prom_header_with_24_byte_pcir_is_accepted() {
    let mut rom = vec![0u8; 0x1000];
    rom[0] = 0x55;
    rom[1] = 0xaa;
    rom[2] = 0x08; // 8 * 512 = 4096 bytes
    rom[0x18..0x1a].copy_from_slice(&0x170u16.to_le_bytes());
    let pcir = 0x170;
    rom[pcir..pcir + 4].copy_from_slice(b"PCIR");
    rom[pcir + 4..pcir + 6].copy_from_slice(&0x10deu16.to_le_bytes());
    rom[pcir + 6..pcir + 8].copy_from_slice(&0x2803u16.to_le_bytes());
    rom[pcir + 10..pcir + 12].copy_from_slice(&0x18u16.to_le_bytes());
    rom[pcir + 16..pcir + 18].copy_from_slice(&0x0008u16.to_le_bytes());
    rom[pcir + 21] = 0x80; // last image
    let npde = (pcir + 0x18 + 0xf) & !0xf;
    assert_eq!(npde, 0x190);
    rom[npde..npde + 4].copy_from_slice(b"NPDE");
    rom[npde + 8..npde + 10].copy_from_slice(&0x0008u16.to_le_bytes());
    rom[npde + 10] = 0x80;

    let layout = crate::vbios::layout(&rom).unwrap();
    assert_eq!(layout.rom, 0);
    assert_eq!(layout.images, 1);
    assert_eq!(layout.pci, Some((0, 4096)));
    assert_eq!(layout.end, Some(4096));
}
