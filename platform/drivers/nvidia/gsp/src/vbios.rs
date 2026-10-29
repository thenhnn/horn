// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//! Adapted from NOVA 3497a403, vbios.rs and firmware/fwsec.rs.
//! Parse a bounded PROM snapshot, without executing x86/EFI option-ROM code.
//! The hardware adapter uses BAR0 PROM reads; it does not require a ROM BAR.
use crate::{Error, Result};
use core::ops::Range;

pub const PROM_BYTES: usize = 1024 * 1024;
fn range(data: &[u8], at: usize, length: usize) -> Result<&[u8]> {
    data.get(at..at.checked_add(length).ok_or(Error::Range)?)
        .ok_or(Error::Range)
}
fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(range(data, at, 2)?.try_into().unwrap()))
}
fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(range(data, at, 4)?.try_into().unwrap()))
}
fn put(data: &mut [u8], at: usize, value: u32) -> Result<()> {
    data.get_mut(at..at.checked_add(4).ok_or(Error::Range)?)
        .ok_or(Error::Range)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn array(
    data: &[u8],
    at: usize,
    header: usize,
    stride: usize,
    count: usize,
    minimum: usize,
) -> Result<Range<usize>> {
    if stride < minimum {
        return Err(Error::Malformed);
    }
    let start = at.checked_add(header).ok_or(Error::Range)?;
    let length = stride.checked_mul(count).ok_or(Error::Range)?;
    range(data, start, length)?;
    Ok(start..start + length)
}
/// IFR v1/v2/v3 PROM preamble. Non-IFR PROMs begin directly with PCI ROM images.
fn rom_offset(data: &[u8]) -> Result<usize> {
    if range(data, 0, 4)? != b"NVGI" {
        return Ok(0);
    }
    let fixed = u32_at(data, 4)?;
    let offset = match (fixed >> 8) & 255 {
        1 | 2 => u32_at(data, ((fixed >> 16) & 0x7fff) as usize + 4)?,
        3 => {
            let total = u32_at(data, 8)? & 0xfffff;
            let status = u32_at(data, total as usize)? as usize;
            let directory = status.checked_add(4096).ok_or(Error::Range)?;
            if range(data, directory, 4)? != b"RFRD" {
                return Err(Error::Malformed);
            }
            u32_at(data, directory + 8)?
        }
        _ => return Err(Error::Unsupported),
    } as usize;
    range(data, offset, 26)?;
    Ok(offset)
}
struct Image<'a> {
    bytes: &'a [u8],
    kind: u8,
    last: bool,
}
fn image(data: &[u8]) -> Result<Image<'_>> {
    if !matches!(u16_at(data, 0)?, 0xaa55 | 0x4e56) {
        return Err(Error::Malformed);
    }
    let pcir = u16_at(data, 24)? as usize;
    let header = range(data, pcir, 26)?;
    if &header[..4] != b"PCIR" && &header[..4] != b"NPDS" {
        return Err(Error::Malformed);
    }
    let pcir_bytes = u16_at(header, 10)? as usize;
    if pcir_bytes < 26 {
        return Err(Error::Malformed);
    }
    range(data, pcir, pcir_bytes)?;
    let mut length = u16_at(header, 16)? as usize * 512;
    let kind = header[20];
    let mut last = header[21] & 0x80 != 0 || kind == 0x70;
    let npde = pcir
        .checked_add(pcir_bytes)
        .and_then(|n| n.checked_add(15))
        .ok_or(Error::Range)?
        & !15;
    // Optional extension overrides both size and last-image status.
    if let Ok(extension) = range(data, npde, 11)
        && &extension[..4] == b"NPDE"
    {
        let n = u16_at(extension, 8)? as usize;
        if n == 0 {
            return Err(Error::Malformed);
        }
        length = n * 512;
        last = extension[10] & 0x80 != 0 || kind == 0x70;
    }
    if length == 0 || pcir.checked_add(pcir_bytes).is_none_or(|n| n > length) {
        return Err(Error::Malformed);
    }
    if range(data, npde, 4).is_ok_and(|v| v == b"NPDE") && npde + 11 > length {
        return Err(Error::Range);
    }
    Ok(Image {
        bytes: range(data, 0, length)?,
        kind,
        last,
    })
}
fn falcon_pointer(pci: &[u8]) -> Result<usize> {
    let at = pci
        .windows(6)
        .position(|b| b == b"\xff\xb8BIT\0")
        .ok_or(Error::Malformed)?;
    let header = range(pci, at, 12)?;
    let header_size = header[8] as usize;
    if header_size < 12 {
        return Err(Error::Malformed);
    }
    // BIT header checksum is independent of the PCI ROM's executable contents.
    if range(pci, at, header_size)?
        .iter()
        .fold(0u8, |a, b| a.wrapping_add(*b))
        != 0
    {
        return Err(Error::Checksum);
    }
    let stride = header[9] as usize;
    let entries = array(pci, at, header_size, stride, header[10] as usize, 6)?;
    let mut pointer = None;
    for entry in (entries.start..entries.end).step_by(stride) {
        let token = range(pci, entry, 6)?;
        if token[0] != 0x70 {
            continue;
        }
        if pointer.is_some() || u16_at(token, 2)? < 4 {
            return Err(Error::Malformed);
        }
        pointer = Some(u32_at(pci, u16_at(token, 4)? as usize)? as usize);
    }
    pointer.ok_or(Error::Malformed)
}
/// AD106 production FWSEC descriptor, payload and original signature table.
/// Borrows a CPU-owned snapshot, never device-writable memory.
pub struct Fwsec<'a> {
    pub ucode: &'a [u8],
    signatures: &'a [u8],
    pub imem_size: u32,
    pub imem_destination: u32,
    pub imem_tag: u32,
    pub dmem_size: u32,
    pub dmem_destination: u32,
    pub interface_offset: u32,
    pub pkc_data_offset: u32,
    pub engine_mask: u16,
    pub ucode_id: u8,
    pub signature_versions: u16,
}
impl<'a> Fwsec<'a> {
    pub fn from_prom(snapshot: &'a [u8]) -> Result<Self> {
        if snapshot.len() > PROM_BYTES {
            return Err(Error::Range);
        }
        let mut at = rom_offset(snapshot)?;
        let mut pci = None;
        let mut fwsec = None;
        let mut end = None;
        for _ in 0..64 {
            let img = image(snapshot.get(at..).ok_or(Error::Range)?)?;
            if fwsec.is_none() {
                if img.kind == 0 && pci.is_none() {
                    pci = Some(img.bytes);
                }
                if img.kind == 0xe0 {
                    fwsec = Some(at);
                }
            }
            at = at.checked_add(img.bytes.len()).ok_or(Error::Range)?;
            if img.last {
                end = Some(at);
                break;
            }
        }
        let pci = pci.ok_or(Error::Malformed)?;
        let section = snapshot
            .get(fwsec.ok_or(Error::Malformed)?..end.ok_or(Error::Malformed)?)
            .ok_or(Error::Range)?;
        // BIT treats PCI-AT and FWSEC as logically adjacent, even if EFI lies
        // between them physically. Never subtract the physical PROM offset.
        let table = falcon_pointer(pci)?
            .checked_sub(pci.len())
            .ok_or(Error::Range)?;
        let header = range(section, table, 4)?;
        if header[1] < 4 {
            return Err(Error::Malformed);
        }
        let stride = header[2] as usize;
        let entries = array(
            section,
            table,
            header[1] as usize,
            stride,
            header[3] as usize,
            6,
        )?;
        let mut offset = None;
        for entry in (entries.start..entries.end).step_by(stride) {
            if section[entry] != 0x85 {
                continue;
            } // production only, never debug 0x45
            if offset.is_some() {
                return Err(Error::Malformed);
            }
            offset = Some(
                (u32_at(section, entry + 2)? as usize)
                    .checked_sub(pci.len())
                    .ok_or(Error::Range)?,
            );
        }
        Self::descriptor(
            section
                .get(offset.ok_or(Error::Malformed)?..)
                .ok_or(Error::Range)?,
        )
    }
    fn descriptor(data: &'a [u8]) -> Result<Self> {
        let hdr = u32_at(data, 0)?;
        if (hdr >> 8) & 255 != 3 {
            return Err(Error::Unsupported);
        } // AD106 V3 only
        range(data, 0, 44)?;
        let descriptor_size = (hdr >> 16) as usize;
        let imem_size = u32_at(data, 20)?;
        let dmem_size = u32_at(data, 32)?;
        let payload_size = imem_size.checked_add(dmem_size).ok_or(Error::Range)? as usize;
        // No guessed decompression: only uncompressed, block-aligned payloads.
        if u32_at(data, 4)? as usize != payload_size
            || imem_size == 0
            || dmem_size == 0
            || imem_size & 255 != 0
            || dmem_size & 255 != 0
        {
            return Err(Error::Unsupported);
        }
        let count = data[39] as usize;
        let versions = u16_at(data, 40)?;
        if count == 0
            || versions.count_ones() as usize != count
            || descriptor_size < 44 + count * 384
        {
            return Err(Error::Malformed);
        }
        let out = Self {
            ucode: range(data, descriptor_size, payload_size)?,
            signatures: range(data, 44, count * 384)?,
            imem_size,
            dmem_size,
            imem_destination: u32_at(data, 16)?,
            imem_tag: u32_at(data, 24)?,
            dmem_destination: u32_at(data, 28)?,
            interface_offset: u32_at(data, 12)?,
            pkc_data_offset: u32_at(data, 8)?,
            engine_mask: u16_at(data, 36)?,
            ucode_id: data[38],
            signature_versions: versions,
        };
        if out.engine_mask == 0
            || out.imem_destination & 255 != 0
            || out.dmem_destination & 255 != 0
            || out
                .pkc_data_offset
                .checked_add(384)
                .is_none_or(|n| n > out.dmem_size)
        {
            return Err(Error::Range);
        }
        Ok(out)
    }
    /// Prepare both FRTS and SB *before* starting either firmware. Production
    /// signature selection follows the descriptor bitmap, never the debug path.
    pub fn prepare<'s>(
        &self,
        fuse: u32,
        command: Command,
        staging: &'s mut [u8],
    ) -> Result<crate::falcon::Prepared<'s>> {
        if fuse >= 16 || self.signature_versions & (1 << fuse) == 0 {
            return Err(Error::Unsupported);
        }
        if staging.len() != self.ucode.len() {
            return Err(Error::Range);
        }
        let header_at = (self.imem_size as usize)
            .checked_add(self.interface_offset as usize)
            .ok_or(Error::Range)?;
        let header = range(self.ucode, header_at, 4)?;
        if header[0] != 1 || header[1] < 4 {
            return Err(Error::Unsupported);
        }
        let stride = header[2] as usize;
        let entries = array(
            self.ucode,
            header_at,
            header[1] as usize,
            stride,
            header[3] as usize,
            8,
        )?;
        if entries.end > self.ucode.len() {
            return Err(Error::Range);
        }
        let mut mapper = None;
        for entry in (entries.start..entries.end).step_by(stride) {
            if u32_at(self.ucode, entry)? != 4 {
                continue;
            }
            if mapper.is_some() {
                return Err(Error::Malformed);
            }
            mapper = Some(
                (self.imem_size as usize)
                    .checked_add(u32_at(self.ucode, entry + 4)? as usize)
                    .ok_or(Error::Range)?,
            );
        }
        let mapper = mapper.ok_or(Error::Unsupported)?;
        let data = range(self.ucode, mapper, 64)?;
        if mapper < self.imem_size as usize || u16_at(data, 4)? != 3 || u16_at(data, 6)? < 64 {
            return Err(Error::Unsupported);
        }
        let cmd = (self.imem_size as usize)
            .checked_add(u32_at(data, 8)? as usize)
            .ok_or(Error::Range)?;
        if u32_at(data, 12)? < 44 {
            return Err(Error::Range);
        }
        range(self.ucode, cmd, 44)?;
        if cmd < self.imem_size as usize {
            return Err(Error::Range);
        }
        let index = (self.signature_versions & ((1u16 << fuse) - 1)).count_ones() as usize;
        let signature = range(self.signatures, index * 384, 384)?;
        let patch = (self.imem_size as usize)
            .checked_add(self.pkc_data_offset as usize)
            .ok_or(Error::Range)?;
        let patch_end = patch.checked_add(384).ok_or(Error::Range)?;
        range(self.ucode, patch, 384)?;
        if [(mapper, 64), (cmd, 44), (entries.start, entries.len())]
            .iter()
            .any(|(at, len)| patch < at + len && *at < patch_end)
        {
            return Err(Error::Range);
        }
        staging.copy_from_slice(self.ucode);
        put(
            staging,
            mapper + 44,
            match command {
                Command::Frts { .. } => 0x15,
                Command::Sb => 0x19,
            },
        )?;
        // ReadVbios, packed 24-byte layout: request the GPU's own verified VBIOS.
        for (offset, value) in [(0, 1), (4, 24), (8, 0), (12, 0), (16, 0), (20, 2)] {
            put(staging, cmd + offset, value)?;
        }
        if let Command::Frts { address, bytes } = command {
            if address & 4095 != 0
                || bytes == 0
                || bytes & 4095 != 0
                || address
                    .checked_add(bytes)
                    .is_none_or(|end| end > 1u64 << 44)
            {
                return Err(Error::Range);
            }
            let address = u32::try_from(address >> 12).map_err(|_| Error::Range)?;
            let bytes = u32::try_from(bytes >> 12).map_err(|_| Error::Range)?;
            for (offset, value) in [(24, 1), (28, 20), (32, address), (36, bytes), (40, 2)] {
                put(staging, cmd + offset, value)?;
            }
        }
        staging
            .get_mut(patch..patch_end)
            .ok_or(Error::Range)?
            .copy_from_slice(signature);
        crate::falcon::Prepared::new(
            staging,
            crate::falcon::Engine::Gsp,
            crate::firmware::Segment {
                source: 0,
                destination: self.imem_destination,
                length: self.imem_size,
            },
            crate::firmware::Segment {
                source: self.imem_size,
                destination: self.dmem_destination,
                length: self.dmem_size,
            },
            crate::falcon::Brom {
                pkc: self.pkc_data_offset,
                engines: self.engine_mask,
                ucode: self.ucode_id,
            },
            0,
            Some(command),
        )
    }
}
#[derive(Clone, Copy)]
pub enum Command {
    /// Addresses refer to a separately checked/accounted VRAM FRTS reservation,
    /// not CPU physical addresses or caller-supplied firmware data.
    Frts {
        address: u64,
        bytes: u64,
    },
    Sb,
}
