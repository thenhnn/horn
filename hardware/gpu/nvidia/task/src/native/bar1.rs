// SPDX-License-Identifier: GPL-2.0-only
//! Append-only BAR1 VMM. Preserve occupied RM PDEs; bootstrap only a zero entry
//! below RM's published PD3 via PRAMIN. Private parents append PD0 slots; sealed leaf
//! tables and existing mappings are never overwritten, recycled or freed.
//! CPU aliases are UC, as required by current Horn VM/devmgr. No WB/WC alias.
use super::*;
mod bootstrap;
use horn_nvidia_gsp::{display::pramin::Region, gmmu};
const LARGE: u64 = 2 * 1024 * 1024;
const WINDOW: u64 = 65536;

#[derive(Clone, Copy)]
pub(crate) struct VramRegion {
    pub base: u64,
    pub bytes: u64,
}
impl VramRegion {
    pub fn write(self, p: &mut Native, offset: u64, words: &[u32], deadline: u64) -> Result<()> {
        p.vram_write(self, offset, words, deadline)
    }
    pub fn read(self, p: &mut Native, offset: u64, words: &mut [u32], deadline: u64) -> Result<()> {
        p.vram_read(self, offset, words, deadline)
    }
}
struct Mapping {
    region: VramRegion,
    va: u64,
    _leaves: DmaBlock,
}
#[derive(Default)]
pub(super) struct Bar1 {
    info: Option<(u32, u64, u64, u64)>, // PCI index, aperture length, shared PD3, WPR limit
    parents: Option<DmaBlock>,          // PD2, PD1, dual PD0; coherent system memory
    mappings: Vec<Mapping>,
    next: u64,
    arena_base: u64,
    shared_pages: Vec<u64>,
    failed: bool,
    phase: &'static str,
    root_publication_started: bool,
    window: Option<(u64, MmioWindow)>,
}
impl Native {
    pub fn configure_bar1(
        &mut self,
        resources: &PciResources,
        root: u64,
        limit: u64,
    ) -> Result<()> {
        if self.bar1.info.is_some()
            || root == 0
            || root & 4095 != 0
            || root.checked_add(4096).is_none_or(|v| v > limit)
        {
            return Err(Error::Range);
        }
        // GPU BAR1 means the second memory BAR, not PCI slot1 if BAR0 is 64-bit.
        let mut number = 0;
        let mut index = 0;
        while index < 6 {
            let low = resources.configuration.data[4 + index];
            if low & 1 == 0 && resources.bar_lengths.data[index] != 0 {
                if number == 1 {
                    if low & 8 == 0 {
                        return Err(Error::State);
                    }
                    sel4::debug_println!(
                        "NVIDIA_BAR1_CONFIG index={} aperture_bytes={} root={:#x} writable_fb_limit={:#x}",
                        index,
                        resources.bar_lengths.data[index],
                        root,
                        limit
                    );
                    let aperture =
                        self.bar1_bound_aperture(resources.bar_lengths.data[index], root, limit)?;
                    self.bar1.info = Some((index as u32, aperture, root, limit));
                    return Ok(());
                }
                number += 1;
            }
            index += if low & 7 == 4 { 2 } else { 1 };
        }
        Err(Error::Unavailable)
    }
    pub fn map_vram(&mut self, base: u64, bytes: u64, deadline: u64) -> Result<VramRegion> {
        self.healthy()?;
        if self.bar1.failed {
            return Err(Error::Quarantined);
        }
        // Latch before any possibly published store, including uncertain MMIO.
        self.bar1.failed = true;
        self.bar1.phase = "validate";
        sel4::debug_println!(
            "NVIDIA_BAR1_MAP_BEGIN vram={:#x} bytes={} aperture_va={:#x}",
            base,
            bytes,
            self.bar1.next
        );
        let result = self.map_vram_inner(base, bytes, deadline);
        if let Err(error) = &result {
            sel4::debug_println!(
                "NVIDIA_BAR1_MAP_FAILED phase={} error={:?} vram={:#x} bytes={} root_publication_started={} backing=retained retry=false",
                self.bar1.phase,
                error,
                base,
                bytes,
                self.bar1.root_publication_started
            );
        } else {
            self.bar1.failed = false;
            self.bar1.phase = "ready";
        }
        result
    }
    fn map_vram_inner(&mut self, base: u64, bytes: u64, deadline: u64) -> Result<VramRegion> {
        let (_, aperture, root, limit) = self.bar1.info.ok_or(Error::State)?;
        let end = base.checked_add(bytes).ok_or(Error::Range)?;
        let span = bytes
            .div_ceil(LARGE)
            .checked_mul(LARGE)
            .ok_or(Error::Range)?;
        if bytes == 0
            || (base | bytes) & 4095 != 0
            || end > limit
            || (base < root + 4096 && root < end)
            || self
                .bar1
                .shared_pages
                .iter()
                .any(|page| base < page + 4096 && *page < end)
            || self.bar1.mappings.len() >= 16
            || self
                .bar1
                .mappings
                .iter()
                .any(|m| base < m.region.base + m.region.bytes && m.region.base < end)
            || span > (1 << 29)
            || span > aperture
        {
            return Err(Error::Range);
        }
        self.bar1
            .mappings
            .try_reserve(1)
            .map_err(|_| Error::Exhausted)?;
        if self.bar1.parents.is_none() {
            self.bar1.phase = "find-shared-slot";
            let slot = self.shared_bar1_slot(base, end, span, deadline)?;
            self.bar1.phase = "allocate-parents";
            let mut parents = self.allocate(3 * 4096, DeviceAccess::Read)?;
            if parents.iova + parents.bytes as u64 > 1 << 32 {
                return Err(Error::Range);
            }
            unsafe {
                parents.zero()?;
                parents.write32(0, gmmu::pde(parents.iova + 4096)? as u32)?;
                parents.write32(4096, gmmu::pde(parents.iova + 8192)? as u32)?;
            }
            let entry = gmmu::pde(parents.iova + slot.parent_offset as u64)?;
            self.bar1.parents = Some(parents);
            // Revalidate the complete observed path after allocations and before
            // any shared-table write. A changed path is terminal, not retried.
            self.bar1.phase = "revalidate-shared-path";
            for observed in &slot.path {
                if self.shared_entry(observed.page, observed.offset, deadline)? != observed.value {
                    return Err(Error::State);
                }
            }
            let last = slot.path.last().ok_or(Error::State)?;
            if last.value != 0 {
                return Err(Error::State);
            }
            let shared = Region::new(last.page, 4096)?;
            self.dma_write_barrier();
            self.bar1.phase = "publish-shared-high";
            self.bar1.root_publication_started = true; // shared slot, including uncertain writes
            shared.write(self, last.offset + 4, &[(entry >> 32) as u32], deadline)?;
            self.bar1.phase = "publish-shared-valid";
            shared.write(self, last.offset, &[entry as u32], deadline)?;
            self.bar1.phase = "invalidate-shared-slot";
            self.bar1_flush(deadline)?;
            self.bar1.arena_base = slot.va;
            self.bar1.next = slot.va;
            sel4::debug_println!(
                "NVIDIA_BAR1_SHARED_SLOT table={:#x} offset={:#x} value={:#x} aperture_va={:#x} existing_pdes=preserved",
                last.page,
                last.offset,
                entry,
                slot.va
            );
        }
        let va = self.bar1.next;
        if va
            .checked_add(span)
            .is_none_or(|v| v > aperture.min(self.bar1.arena_base + (1 << 29)))
        {
            return Err(Error::Range);
        }
        self.bar1.phase = "build-leaves";
        let count = (span / LARGE) as usize;
        let mut leaves = self.allocate(count * 4096, DeviceAccess::Read)?;
        if leaves.iova + leaves.bytes as u64 > 1 << 32 {
            return Err(Error::Range);
        }
        unsafe {
            leaves.zero()?;
            for page in 0..bytes / 4096 {
                let pte = gmmu::pte_vid(base + page * 4096)?;
                leaves.write32(page as usize * 8, pte as u32)?;
                leaves.write32(page as usize * 8 + 4, (pte >> 32) as u32)?;
            }
        }
        let leaf_iova = leaves.iova;
        let region = VramRegion { base, bytes };
        self.bar1.mappings.push(Mapping {
            region,
            va,
            _leaves: leaves,
        });
        self.bar1.phase = "append-parent-slots";
        let parents = self.bar1.parents.as_mut().ok_or(Error::State)?;
        for leaf in 0..count {
            let at = 8192 + ((va - self.bar1.arena_base) as usize / LARGE as usize + leaf) * 16 + 8;
            let pde = gmmu::pde(leaf_iova + leaf as u64 * 4096)?;
            unsafe {
                if parents.read32(at)? != 0 || parents.read32(at + 4)? != 0 {
                    return Err(Error::State);
                }
                parents.write32(at + 4, (pde >> 32) as u32)?;
                fence(Ordering::Release);
                parents.write32(at, pde as u32)?;
            }
        }
        self.dma_write_barrier();
        self.bar1.phase = "invalidate-mapping";
        self.bar1_flush(deadline)?;
        self.bar1.next = va + span;
        sel4::debug_println!(
            "NVIDIA_BAR1_MAP vram={:#x} bytes={} aperture_va={:#x} cache=uc leaves=sealed parents=append-only backing=retained",
            base,
            bytes,
            va
        );
        Ok(region)
    }
    /// TU102/AD10x path: nouveau tu102_vmm_flush, BAR engref => PAGE_ALL |
    /// HUB_ONLY | ALL_PDB. Wait for invalidate trigger to clear (not GP_GET).
    fn bar1_flush(&mut self, deadline: u64) -> Result<()> {
        let root = self.bar1.info.ok_or(Error::State)?.2;
        let deadline = self
            .now_us()?
            .checked_add(2_000_000)
            .ok_or(Error::Range)?
            .min(deadline);
        // Do not overwrite a still-active invalidation request.
        loop {
            if self.now_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let value = self.read32(0xb830b0)?;
            if value == u32::MAX {
                sel4::debug_println!(
                    "NVIDIA_BAR1_INVALIDATE_UNAVAILABLE stage=before-request status={:#x}",
                    value
                );
                return Err(Error::State);
            }
            if value & 0x80000000 == 0 {
                break;
            }
        }
        self.write32(0xb830a0, (root >> 8) as u32)?;
        self.write32(0xb830a4, 0)?;
        self.write32(0xb830b0, 0x80000007)?;
        loop {
            if self.now_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let value = self.read32(0xb830b0)?;
            if value == u32::MAX {
                sel4::debug_println!(
                    "NVIDIA_BAR1_INVALIDATE_UNAVAILABLE stage=wait-completion status={:#x}",
                    value
                );
                return Err(Error::State);
            }
            if value & 0x80000000 == 0 {
                return Ok(());
            }
        }
    }
    fn vram_va(&self, region: VramRegion, offset: u64, words: usize) -> Result<u64> {
        if self.bar1.failed {
            return Err(Error::Quarantined);
        }
        if offset & 3 != 0
            || offset
                .checked_add((words as u64).checked_mul(4).ok_or(Error::Range)?)
                .is_none_or(|end| end > region.bytes)
        {
            return Err(Error::Range);
        }
        let map = self
            .bar1
            .mappings
            .iter()
            .find(|m| m.region.base == region.base && m.region.bytes == region.bytes)
            .ok_or(Error::State)?;
        map.va.checked_add(offset).ok_or(Error::Range)
    }
    fn bar1_pointer(&mut self, va: u64) -> Result<(*mut u32, usize)> {
        let (index, aperture, _, _) = self.bar1.info.ok_or(Error::State)?;
        let base = va & !(WINDOW - 1);
        // Map page-granular, never exporting the unowned guard after an allocation.
        let map = self
            .bar1
            .mappings
            .iter()
            .find(|m| va >= m.va && va < m.va + m.region.bytes)
            .ok_or(Error::Range)?;
        let length = (map.va + map.region.bytes - base)
            .min(WINDOW)
            .min(aperture - base) as usize;
        if self
            .bar1
            .window
            .as_ref()
            .is_none_or(|(old, _)| *old != base)
        {
            if let Some((_, old)) = self.bar1.window.take() {
                self.session.close_window(old);
            }
            let window = self.session.mmio_window(index, base, length);
            self.bar1.window = Some((base, window));
        }
        let window = &self.bar1.window.as_ref().ok_or(Error::State)?.1;
        let within = (va - base) as usize;
        Ok((
            unsafe { window.pointer().as_ptr().add(within).cast() },
            (window.length() - within) / 4,
        ))
    }
    fn vram_write(
        &mut self,
        region: VramRegion,
        offset: u64,
        words: &[u32],
        deadline: u64,
    ) -> Result<()> {
        self.healthy()?;
        let mut va = self.vram_va(region, offset, words.len())?;
        let mut at = 0;
        while at < words.len() {
            if self.now_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let (pointer, available) = self.bar1_pointer(va)?;
            let n = available.min(words.len() - at);
            // UC posted stores, no read-after-each-pixel. A final UC read drains
            // this batch; it is ordering, not full data/visible-pixel verification.
            unsafe {
                let mut i = 0;
                if va & 7 != 0 {
                    pointer.write_volatile(words[at].to_le());
                    i = 1;
                }
                while i + 1 < n {
                    let value = u64::from(words[at + i]) | (u64::from(words[at + i + 1]) << 32);
                    pointer.add(i).cast::<u64>().write_volatile(value.to_le());
                    i += 2;
                }
                if i < n {
                    pointer.add(i).write_volatile(words[at + i].to_le());
                }
                fence(Ordering::SeqCst);
                let last = u32::from_le(pointer.add(n - 1).read_volatile());
                if last != words[at + n - 1] {
                    sel4::debug_println!(
                        "NVIDIA_BAR1_WRITE_MISMATCH vram={:#x} offset={:#x} aperture_va={:#x} batch_bytes={} expected={:#010x} actual={:#010x} store=uc64-with-u32-edges retry=false",
                        region.base,
                        offset + (at + n - 1) as u64 * 4,
                        va + (n - 1) as u64 * 4,
                        n * 4,
                        words[at + n - 1],
                        last
                    );
                    // CPU-owned read-only DMA table metadata only: no further
                    // GPU/MMIO accesses, writes, alternate path or retry.
                    if let Some(map) =
                        self.bar1.mappings.iter().find(|m| {
                            m.region.base == region.base && m.region.bytes == region.bytes
                        })
                    {
                        let local = va - map.va + (n - 1) as u64 * 4;
                        let pte_at = (local / 4096) as usize * 8;
                        let pte = u64::from(map._leaves.read32(pte_at)?)
                            | (u64::from(map._leaves.read32(pte_at + 4)?) << 32);
                        let pd0_index = ((va - self.bar1.arena_base) / LARGE) as usize;
                        if let Some(parents) = &self.bar1.parents {
                            let at = 8192 + pd0_index * 16 + 8;
                            let pde = u64::from(parents.read32(at)?)
                                | (u64::from(parents.read32(at + 4)?) << 32);
                            sel4::debug_println!(
                                "NVIDIA_BAR1_WRITE_TABLES pd0_index={} pde={:#018x} leaf_iova={:#x} pte_index={} pte={:#018x} observation=cpu-table-view-not-gpu-fetch-proof",
                                pd0_index,
                                pde,
                                map._leaves.iova,
                                local / 4096,
                                pte
                            );
                        }
                    }
                    return Err(Error::Checksum);
                }
            }
            va += n as u64 * 4;
            at += n;
        }
        if self.now_us()? >= deadline {
            return Err(Error::Timeout);
        }
        Ok(())
    }
    fn vram_read(
        &mut self,
        region: VramRegion,
        offset: u64,
        words: &mut [u32],
        deadline: u64,
    ) -> Result<()> {
        self.healthy()?;
        let mut va = self.vram_va(region, offset, words.len())?;
        let mut at = 0;
        while at < words.len() {
            if self.now_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let (pointer, available) = self.bar1_pointer(va)?;
            let n = available.min(words.len() - at);
            for i in 0..n {
                words[at + i] = unsafe { u32::from_le(pointer.add(i).read_volatile()) };
            }
            fence(Ordering::Acquire);
            va += n as u64 * 4;
            at += n;
        }
        Ok(())
    }
}
