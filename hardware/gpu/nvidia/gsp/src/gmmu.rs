// SPDX-License-Identifier: GPL-2.0-only
//! AD10x VER2 4-KiB GMMU: GA10X/GP10X geometry and pinned GP100 dev_mmu.h
//! VER2 fields, also used by nouveau gp100_vmm_desc_12. PD3/2/1 are 8-byte;
//! PD0 is dual 16-byte (big table first, small table second); PTE is 8-byte.
//! These are not the obsolete NV_MMU_PTE/PDE pre-VER2 formats.
use crate::{Error, Result, raw};
pub const PAGE: usize = 4096;
pub const PAGE_BYTES: u64 = PAGE as u64;
pub const LEVELS: [(u32, u32); 5] = [(47, 2), (38, 9), (29, 9), (21, 8), (12, 9)];
pub const PTE_VALID: u64 = raw::HORN_GMMU_VALID as u64;
pub const PTE_READ_ONLY: u64 = raw::HORN_GMMU_READ_ONLY as u64;
pub const PTE_VOL: u64 = raw::HORN_GMMU_VOL as u64;
pub const PTE_APERTURE_SYS_COH: u64 = raw::HORN_GMMU_SYS_COH as u64;
pub const PTE_APERTURE_VID: u64 = 0;
pub const PDE_APERTURE_SYS_COH: u64 = PTE_APERTURE_SYS_COH;
pub const PDE_APERTURE_VID: u64 = raw::HORN_GMMU_PDE_VID as u64;
pub const PDE_VOL: u64 = PTE_VOL;

pub fn index(address: u64, level: usize) -> Result<usize> {
    if address >= 1 << 49 {
        return Err(Error::Range);
    }
    let (first, bits) = LEVELS.get(level).copied().ok_or(Error::Range)?;
    Ok(((address >> first) & ((1u64 << bits) - 1)) as usize)
}
pub fn entries(level: usize) -> Result<usize> {
    Ok(1 << LEVELS.get(level).ok_or(Error::Range)?.1)
}
pub fn entry_bytes(level: usize) -> usize {
    if level == 3 {
        raw::HORN_GMMU_DUAL_BYTES as usize
    } else {
        raw::HORN_GMMU_PDE_BYTES as usize
    }
}
pub fn table_bytes(levels: usize) -> Result<usize> {
    if levels == 0 || levels > LEVELS.len() {
        return Err(Error::Range);
    }
    Ok(levels * PAGE)
}
fn address(address: u64, limit: u64) -> Result<u64> {
    if address & (PAGE_BYTES - 1) != 0 || address >= limit {
        return Err(Error::Range);
    }
    Ok(address >> 4) // VER2 address bits53:8 (sys), bits32:8 (vid), page shift12
}
pub fn pte_sys(iova: u64) -> Result<u64> {
    Ok(address(iova, 1 << 47)? | PTE_VALID | PTE_APERTURE_SYS_COH | PTE_VOL)
}
pub fn pte_sys_read_only(iova: u64) -> Result<u64> {
    Ok(pte_sys(iova)? | PTE_READ_ONLY)
}
pub fn pte_vid(vram: u64) -> Result<u64> {
    Ok(address(vram, 1 << 37)? | PTE_VALID | PTE_VOL)
}
pub fn pde(table: u64) -> Result<u64> {
    Ok(address(table, 1 << 47)? | PDE_APERTURE_SYS_COH | PDE_VOL)
}
pub fn pde_vid(table: u64) -> Result<u64> {
    Ok(address(table, 1 << 37)? | PDE_APERTURE_VID)
}

/// One page per level for a single 2-MiB leaf span. Writes are CPU-owned only;
/// publish once after every PTE is finalized. No live-table mutation authority.
pub struct Tables<'a> {
    bytes: &'a mut [u8],
}
impl<'a> Tables<'a> {
    pub fn new(bytes: &'a mut [u8], levels: usize) -> Result<Self> {
        let out = Self::existing(bytes, levels)?;
        out.bytes.fill(0);
        Ok(out)
    }
    pub fn existing(bytes: &'a mut [u8], levels: usize) -> Result<Self> {
        if bytes.len() < table_bytes(levels)? || !bytes.len().is_multiple_of(PAGE) {
            return Err(Error::Range);
        }
        Ok(Self { bytes })
    }
    /// PD0 writes target the small-page half; big-page half remains invalid.
    pub fn write(&mut self, index: usize, level: usize, value: u64) -> Result<()> {
        if index >= entries(level)? {
            return Err(Error::Range);
        }
        let at = level * PAGE + index * entry_bytes(level) + if level == 3 { 8 } else { 0 };
        self.bytes
            .get_mut(at..at + 8)
            .ok_or(Error::Range)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }
    pub fn write_root_big(&mut self, index: usize, value: u64) -> Result<()> {
        self.write(index, 0, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ver2_fields_and_geometry() {
        assert_eq!(pte_sys(0x1000).unwrap(), 0x10d);
        assert_eq!(pte_vid(0x1000).unwrap(), 0x109);
        assert_eq!(pde(0x1000).unwrap(), 0x10c);
        assert_eq!(pde_vid(0x1000).unwrap(), 0x102);
        assert_eq!(PTE_READ_ONLY, 1 << 6);
        assert_eq!(entry_bytes(0), 8);
        assert_eq!(entry_bytes(3), 16);
    }
    #[test]
    fn adding_pte_does_not_clear_hierarchy() {
        let mut bytes = alloc::vec![0; 5 * PAGE];
        Tables::new(&mut bytes, 5)
            .unwrap()
            .write(0, 0, 0x10c)
            .unwrap();
        Tables::existing(&mut bytes, 5)
            .unwrap()
            .write(2, 4, 0x109)
            .unwrap();
        assert_eq!(&bytes[..8], &0x10cu64.to_le_bytes());
        Tables::existing(&mut bytes, 5)
            .unwrap()
            .write(0, 3, 0x20c)
            .unwrap();
        assert_eq!(&bytes[3 * PAGE..3 * PAGE + 8], &[0; 8]);
        assert_eq!(&bytes[3 * PAGE + 8..3 * PAGE + 16], &0x20cu64.to_le_bytes());
    }
}
