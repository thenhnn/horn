// SPDX-License-Identifier: GPL-2.0-only
//! Bounded read-only adoption of an existing RM BAR1 hierarchy. Follow only
//! ordinary VRAM PDEs. Never dereference foreign system-memory IOVAs, huge PTEs,
//! WPR, cycles, or client allocations. Install only a revalidated zero entry.
use super::*;
pub(super) struct Observed {
    pub page: u64,
    pub offset: u64,
    pub value: u64,
}
pub(super) struct Slot {
    pub path: Vec<Observed>,
    pub parent_offset: usize,
    pub va: u64,
}
impl Native {
    /// Read-only binding/instance inspection before table publication or BAR1
    /// data writes. PCI aperture size alone is not the active GPU VA limit.
    pub(super) fn bar1_bound_aperture(
        &mut self,
        pci_bytes: u64,
        expected_root: u64,
        wpr: u64,
    ) -> Result<u64> {
        use horn_nvidia_gsp::raw;
        let block = self.read32(raw::HORN_BAR1_BLOCK)?;
        let target = (block >> raw::HORN_BAR1_TARGET_SHIFT) & raw::HORN_BAR1_TARGET_MASK;
        let instance = u64::from(block & raw::HORN_BAR1_PTR_MASK) << raw::HORN_BAR1_PTR_SHIFT;
        sel4::debug_println!(
            "NVIDIA_BAR1_BIND block={:#010x} virtual={} target={} instance={:#x} published_root={:#x} pci_bytes={}",
            block,
            block & raw::HORN_BAR1_VIRTUAL != 0,
            target,
            instance,
            expected_root,
            pci_bytes
        );
        if block == u32::MAX || block & raw::HORN_BAR1_VIRTUAL == 0 {
            return Err(Error::State);
        }
        // No authority to dereference foreign sysmem. An uninspectable binding
        // stops here rather than blindly writing through an assumed GPU VMM.
        if target != 0 {
            return Err(Error::Unsupported);
        }
        if instance == 0 || instance.checked_add(4096).is_none_or(|end| end > wpr) {
            return Err(Error::Range);
        }
        let deadline = self.now_us()?.checked_add(2_000_000).ok_or(Error::Range)?;
        let mut context = [0; 4];
        Region::new(instance, 4096)?.read(
            self,
            raw::HORN_BAR1_CONTEXT_OFFSET as u64,
            &mut context,
            deadline,
        )?;
        let root = u64::from(context[0] & !0xfff) | (u64::from(context[1]) << 32);
        let address_limit = u64::from(context[2] & !0xfff) | (u64::from(context[3]) << 32) | 0xfff;
        let bytes = pci_bytes.min(address_limit.checked_add(1).ok_or(Error::Range)?);
        sel4::debug_println!(
            "NVIDIA_BAR1_CONTEXT root={:#x} root_target={} flags={:#x} va_limit={:#x} pci_bytes={} effective_bytes={}",
            root,
            context[0] & 3,
            context[0] & 0xfff,
            address_limit,
            pci_bytes,
            bytes
        );
        if root != expected_root
            || context[0] & 3 != 0
            || context[0] & raw::HORN_BAR1_NEW_FORMAT == 0
        {
            return Err(Error::State);
        }
        if bytes < LARGE {
            return Err(Error::Range);
        }
        Ok(bytes)
    }
    pub(super) fn shared_entry(&mut self, page: u64, offset: u64, deadline: u64) -> Result<u64> {
        let mut words = [0; 2];
        Region::new(page, 4096)?.read(self, offset, &mut words, deadline)?;
        Ok(u64::from(words[0]) | (u64::from(words[1]) << 32))
    }
    fn shared_child(&mut self, entry: u64, image_base: u64, image_end: u64) -> Result<u64> {
        let page = decode_vid_pde(entry)?;
        let limit = self.bar1.info.ok_or(Error::State)?.3;
        if page == 0
            || page.checked_add(4096).is_none_or(|end| end > limit)
            || self.bar1.shared_pages.contains(&page)
            || (page < image_end && image_base < page + 4096)
            || self
                .bar1
                .mappings
                .iter()
                .any(|m| page < m.region.base + m.region.bytes && m.region.base < page + 4096)
        {
            return Err(Error::State);
        }
        self.bar1.shared_pages.push(page);
        Ok(page)
    }
    pub(super) fn shared_bar1_slot(
        &mut self,
        image_base: u64,
        image_end: u64,
        span: u64,
        deadline: u64,
    ) -> Result<Slot> {
        let (_, aperture, root, _) = self.bar1.info.ok_or(Error::State)?;
        self.bar1
            .shared_pages
            .try_reserve_exact(3)
            .map_err(|_| Error::Exhausted)?;
        self.bar1.shared_pages.push(root);
        let mut path = Vec::new();
        path.try_reserve_exact(3).map_err(|_| Error::Exhausted)?;
        let mut page = root;
        // BAR1 scope is at most PD2[0]'s256GiB. No other top-level entry
        // is modified or assumed empty.
        for level in 0..2 {
            let value = self.shared_entry(page, 0, deadline)?;
            sel4::debug_println!(
                "NVIDIA_BAR1_SHARED_ENTRY level={} table={:#x} index=0 value={:#018x}",
                level,
                page,
                value
            );
            path.push(Observed {
                page,
                offset: 0,
                value,
            });
            if value == 0 {
                return Ok(Slot {
                    path,
                    parent_offset: level * 4096,
                    va: 0,
                });
            }
            page = self.shared_child(value, image_base, image_end)?;
        }
        // PD1 entry covers512MiB. Search only complete/sufficient segments
        // inside the actual BAR1 aperture, highest first. All occupied entries,
        // including huge PTEs, remain untouched.
        let count = aperture.div_ceil(1 << 29).min(512);
        for index in (0..count).rev() {
            let va = index << 29;
            if aperture - va < span {
                continue;
            }
            let offset = index * 8;
            let value = self.shared_entry(page, offset, deadline)?;
            sel4::debug_println!(
                "NVIDIA_BAR1_SHARED_ENTRY level=2 table={:#x} index={} value={:#018x}",
                page,
                index,
                value
            );
            if value == 0 {
                path.push(Observed {
                    page,
                    offset,
                    value,
                });
                return Ok(Slot {
                    path,
                    parent_offset: 8192,
                    va,
                });
            }
        }
        Err(Error::Exhausted) // no lower-tree edits, allocation retries or fallback
    }
}
// GP100 common VER2 ordinary PDE: VID address32:8, aperture2:1,
// optional VOL3. Bit0 (huge PTE) and all other bits must be clear.
fn decode_vid_pde(entry: u64) -> Result<u64> {
    if entry >> 33 != 0
        || entry & 0xff != gmmu::PDE_APERTURE_VID
            && entry & 0xff != (gmmu::PDE_APERTURE_VID | gmmu::PDE_VOL)
    {
        return Err(Error::Unsupported);
    }
    let page = (entry & !0xff) << 4;
    if page & 4095 != 0 {
        return Err(Error::Range);
    }
    Ok(page)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_rm_pde_is_a_vram_directory() {
        assert_eq!(decode_vid_pde(0x1f382c02).unwrap(), 0x1f382c000);
        assert_eq!(decode_vid_pde(0x1f382c0a).unwrap(), 0x1f382c000);
        for entry in [0, 1, 3, 4, 6, 0x1f382c03, 1 << 33 | 2, 0x12] {
            assert!(decode_vid_pde(entry).is_err());
        }
    }
}
