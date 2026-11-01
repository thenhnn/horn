// SPDX-License-Identifier: GPL-2.0-only
//! GA10x/AD10x GMMU page tables for a driver-owned VA space.
//!
//! Geometry is taken from NVIDIA 570.144 `kern_gmmu_fmt_ga10x.c`:
//!
//! ```text
//! PD3 [48:47] 2   PD2 [46:38] 9   PD1 [37:29] 9
//! PD0 [28:21] 8   PT_SMALL [20:12] 9
//! ```
//!
//! Entry fields follow `swref/published/ampere/ga100/dev_mmu.h`:
//! PTE valid/privilege/read-only/encrypted in bits 3:0, sysmem address in
//! bits 31:4, VOL at 32, aperture at 34:33, kind at 43:36. PDE entries keep
//! the big-page fields in bits 31:4 and the small-page fields in 63:36; the
//! top-level entry is 16 bytes wide (big part, then small part).
//!
//! Only what is needed for a first mapping is implemented: 4 KiB sysmem
//! pages, read/write, aperture SYSTEM_COHERENT with VOL. Every table is a
//! single 4 KiB page and the caller supplies the backing memory.
use crate::{Error, Result};

pub const PAGE: usize = 4096;
pub const PAGE_BYTES: u64 = PAGE as u64;

/// Levels from the root down, as (first VA bit, index bits).
pub const LEVELS: [(u32, u32); 5] = [(47, 2), (38, 9), (29, 9), (21, 8), (12, 9)];

/// PTE/PDE flag fields.
pub const PTE_VALID: u64 = 1 << 0;
pub const PTE_READ_ONLY: u64 = 1 << 2;
pub const PTE_VOL: u64 = 1 << 32;
pub const PTE_APERTURE_SYS_COH: u64 = 2 << 33;
pub const PTE_APERTURE_VID: u64 = 0;

/// PDE big-page fields (8-byte entries and the first word of the top entry).
pub const PDE_APERTURE_SYS_COH: u64 = 2;
pub const PDE_APERTURE_VID: u64 = 0;
pub const PDE_VOL: u64 = 1 << 3;
/// PDE small-page fields live in the second word of the top-level entry.
pub const PDE_SMALL_APERTURE_SHIFT: u32 = 32;
pub const PDE_SMALL_VOL: u64 = 1 << 34;

const _: () = assert!(LEVELS[0].0 == 47 && LEVELS[0].1 == 2);
const _: () = assert!(LEVELS[4].0 == 12 && LEVELS[4].1 == 9);
const _: () = assert!(PAGE == 4096);

/// Index of `address` at `level` (0 = root).
pub fn index(address: u64, level: usize) -> Result<usize> {
    let (first, bits) = LEVELS.get(level).copied().ok_or(Error::Range)?;
    let value = (address >> first) & ((1u64 << bits) - 1);
    usize::try_from(value).map_err(|_| Error::Range)
}

/// Entries in one table at `level`.
pub fn entries(level: usize) -> Result<usize> {
    let (_, bits) = LEVELS.get(level).copied().ok_or(Error::Range)?;
    Ok(1usize << bits)
}

/// Bytes per entry: the root is 16-byte wide, the rest are 8.
pub fn entry_bytes(level: usize) -> usize {
    if level == 0 { 16 } else { 8 }
}

/// Arena size for `levels` levels: one 4 KiB page per table, level 0 first.
pub fn table_bytes(levels: usize) -> Result<usize> {
    if levels == 0 || levels > LEVELS.len() {
        return Err(Error::Range);
    }
    Ok(levels * PAGE)
}

/// 4 KiB sysmem PTE. The address occupies bits 31:4, so the device-visible
/// address must stay below 4 GiB (our IOVA reservations are).
pub fn pte_sys(address: u64) -> Result<u64> {
    if address & (PAGE_BYTES - 1) != 0 || address >= 1 << 32 {
        return Err(Error::Range);
    }
    Ok((address >> 4) | PTE_VALID | PTE_APERTURE_SYS_COH | PTE_VOL)
}

/// 4 KiB video-memory PTE: same address encoding, aperture 0. The instance,
/// USERD and RAMFC of a channel live in VRAM and must be reachable through the
/// channel's page directory, which RM cannot populate for an externally owned
/// vspace.
pub fn pte_vid(address: u64) -> Result<u64> {
    // A VRAM offset can exceed 4 GiB on a large framebuffer (the first
    // allocation on this board lands at 0x1_ee5c0000), so the address field is
    // wider here than for system memory.
    if address & (PAGE_BYTES - 1) != 0 || address >= 1 << 40 {
        return Err(Error::Range);
    }
    Ok((address >> 4) | PTE_VALID | PTE_APERTURE_VID | PTE_VOL)
}

/// Read-only variant, used for mappings the GPU must not write.
pub fn pte_sys_read_only(address: u64) -> Result<u64> {
    Ok(pte_sys(address)? | PTE_READ_ONLY)
}

/// Pointer to the next-level table, stored in the big-page fields.
pub fn pde(table: u64) -> Result<u64> {
    if table & (PAGE_BYTES - 1) != 0 || table >= 1 << 32 {
        return Err(Error::Range);
    }
    Ok((table >> 4) | PDE_APERTURE_SYS_COH | PDE_VOL)
}

/// Pointer to the next-level table in the small-page half of the top entry.
pub fn pde_small(table: u64) -> Result<u64> {
    if table & (PAGE_BYTES - 1) != 0 || table >= 1 << 36 {
        return Err(Error::Range);
    }
    Ok((table >> 4) << PDE_SMALL_APERTURE_SHIFT
        | PDE_APERTURE_SYS_COH << PDE_SMALL_APERTURE_SHIFT
        | PDE_SMALL_VOL)
}

/// A page-table arena: one contiguous buffer, one 4 KiB page per table.
///
/// The arena is CPU memory the caller has already made device-visible; the
/// root page address is what `SET_PAGE_DIRECTORY` takes.
pub struct Tables<'a> {
    bytes: &'a mut [u8],
}

impl<'a> Tables<'a> {
    /// `bytes` must be page aligned in device address space and large enough
    /// for `levels` tables. Contents are zeroed: an entry is valid only after
    /// the caller writes it.
    pub fn new(bytes: &'a mut [u8], levels: usize) -> Result<Self> {
        if bytes.len() < table_bytes(levels)? || !bytes.len().is_multiple_of(PAGE) {
            return Err(Error::Range);
        }
        bytes.fill(0);
        Ok(Self { bytes })
    }

    fn table(&mut self, index: usize, level: usize) -> Result<&mut [u8]> {
        if index >= entries(level)? {
            return Err(Error::Range);
        }
        let offset = level
            .checked_mul(PAGE)
            .and_then(|base| {
                index
                    .checked_mul(entry_bytes(level))
                    .and_then(|n| n.checked_add(base))
            })
            .ok_or(Error::Range)?;
        self.bytes
            .get_mut(offset..offset + entry_bytes(level))
            .ok_or(Error::Range)
    }

    /// Write one 8-byte entry (levels 1..4 and leaf tables).
    pub fn write(&mut self, index: usize, level: usize, value: u64) -> Result<()> {
        if level == 0 {
            return Err(Error::Range);
        }
        let entry = self.table(index, level)?;
        entry[..8].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// Write the big half of the 16-byte top-level entry.
    pub fn write_root_big(&mut self, index: usize, value: u64) -> Result<()> {
        let entry = self.table(index, 0)?;
        entry[..8].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// Write the small half of the 16-byte top-level entry.
    pub fn write_root_small(&mut self, index: usize, value: u64) -> Result<()> {
        let entry = self.table(index, 0)?;
        entry[8..16].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_matches_ga10x() {
        assert_eq!(entries(0).unwrap(), 4);
        assert_eq!(entries(1).unwrap(), 512);
        assert_eq!(entries(3).unwrap(), 256);
        assert_eq!(entry_bytes(0), 16);
        assert_eq!(entry_bytes(1), 8);
        assert_eq!(table_bytes(5).unwrap(), 5 * PAGE);

        // 4 KiB page: index 0 of every level for VA 0, and the next page.
        assert_eq!(index(0, 4).unwrap(), 0);
        assert_eq!(index(PAGE_BYTES, 4).unwrap(), 1);
        assert_eq!(index(2 * 1024 * 1024, 3).unwrap(), 1);
        assert_eq!(index(512 * 1024 * 1024, 2).unwrap(), 1);
        assert_eq!(index(256 * 1024 * 1024 * 1024, 1).unwrap(), 1);
        // VA 4 GiB selects root index 0 (bit 32 is below bit 47).
        assert_eq!(index(4 * 1024 * 1024 * 1024, 0).unwrap(), 0);
    }

    #[test]
    fn pte_and_pde_encodings() {
        let pte = pte_sys(0x4a000).unwrap();
        assert_eq!(pte & 0xf, PTE_VALID);
        assert_eq!(pte & (0xffff_ffff << 32), PTE_APERTURE_SYS_COH | PTE_VOL);
        assert_eq!(pte & 0xffff_fffe, 0x4a000 >> 4);
        assert!(pte_sys(0x1001).is_err());
        let vid = pte_vid(0x2000).unwrap();
        assert_eq!(vid & (0xffff_ffff << 32), PTE_VOL);
        assert_eq!(vid & 0x3, PTE_VALID);
        assert_eq!(vid & 0xffe, 0x200);
        assert!(pte_vid(0x1001).is_err());
        assert!(pte_sys(1 << 32).is_err());
        assert_eq!(
            pte_sys_read_only(0x4a000).unwrap() & PTE_READ_ONLY,
            PTE_READ_ONLY
        );

        let pde = pde(0x1_0000).unwrap();
        assert_eq!(pde & 0xffff_fff0, 0x1_0000 >> 4);
        assert_eq!(pde & 0x3, PDE_APERTURE_SYS_COH);
        assert_eq!(pde & PDE_VOL, PDE_VOL);

        let small = pde_small(0x1_0000).unwrap();
        assert_eq!((small >> 32) & 0xffff_fff8, 0x1_0000 >> 4);
        assert_eq!(small & PDE_SMALL_VOL, PDE_SMALL_VOL);
    }

    #[test]
    fn arena_writes_entries_at_the_expected_offsets() {
        let mut bytes = alloc::vec![0xffu8; 5 * PAGE];
        {
            let mut tables = Tables::new(&mut bytes, 5).unwrap();
            tables.write_root_big(1, 0x1122).unwrap();
            tables.write_root_small(1, 0x3344).unwrap();
            tables.write(2, 4, 0x5566).unwrap();
            assert!(tables.write(0, 0, 1).is_err());
        }
        let root = 0x10;
        assert_eq!(&bytes[root..root + 8], &0x1122u64.to_le_bytes());
        assert_eq!(&bytes[root + 8..root + 16], &0x3344u64.to_le_bytes());
        let leaf = 4 * PAGE + 2 * 8;
        assert_eq!(&bytes[leaf..leaf + 8], &0x5566u64.to_le_bytes());
    }
}
