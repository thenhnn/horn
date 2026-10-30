//! Legacy VT-d backend owned by devmgr, not by seL4/root.
//! No passthrough domains, device TLBs, ATS/PASID, RMRRs or compatibility MSI.
//! Hardware operations are deliberately independent of bootstrap RPC/VM transport.
pub mod dmar;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unsupported,
    Exhausted,
    Backend,
    Timeout,
    Poisoned,
}
pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub cookie: u32,
    pub physical: u64,
}
/// # Safety
/// Pages are zeroed, aligned, exclusive resident RAM and remain pinned
/// until reboot (including on error/drop). MMIO and page accesses are volatile.
/// No driver can access these pages or register authority. MMIO writes publish
/// preceding page writes; completed page reads acquire hardware status writes.
/// Cache maintenance must make zeroing/writes visible to a non-snooping IOMMU
/// and invalidate cached hardware completion words before reads.
pub unsafe trait Hardware {
    fn read32(&mut self, address: u64) -> Result<u32>;
    fn read64(&mut self, address: u64) -> Result<u64>;
    fn write32(&mut self, address: u64, value: u32) -> Result<()>;
    fn write64(&mut self, address: u64, value: u64) -> Result<()>;
    fn allocate_page(&mut self) -> Result<Page>;
    fn read_page(&mut self, page: Page, word: usize) -> Result<u64>;
    fn write_page(&mut self, page: Page, word: usize, value: u64) -> Result<()>;
    fn now(&self) -> u64;
}
const TE: u32 = 1 << 31;
const SRTP: u32 = 1 << 30;
const QIE: u32 = 1 << 26;
const IRE: u32 = 1 << 25;
const SIRTP: u32 = 1 << 24;
const CFI: u32 = 1 << 23;
const ADDR: u64 = 0x000f_ffff_ffff_f000;

/// Only this backend can construct a domain. IDs/SIDs are never recycled.
#[derive(Clone, Copy, Debug)]
pub struct Domain {
    sid: u16,
    id: u16,
    root: Page,
}
#[derive(Clone, Copy, Debug)]
pub struct Interrupt {
    index: u16,
    sid: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub source: u16,
    pub address: u64,
    pub reason: u8,
    pub read: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultReport {
    pub fault: Option<Fault>,
    pub overflow: bool,
}
/// Private, non-copyable authority for one monotonic IOVA reservation.
/// Only ordered prefixes may be populated; no unmap permits address reuse.
pub struct ReservedDma {
    domain: Domain,
    base: u64,
    pages: usize,
    populated: usize,
    // Entries added since the last invalidation. The device is not bus-mastering
    // while a reservation is being populated, so the flush is deferred to commit
    // and performed exactly once.
    dirty: bool,
}
impl ReservedDma {
    pub fn base(&self) -> u64 {
        self.base
    }
    pub fn populated(&self) -> usize {
        self.populated
    }
    pub fn complete(&self) -> bool {
        self.populated == self.pages
    }
    pub fn dirty(&self) -> bool {
        self.dirty
    }
}
impl Interrupt {
    /// Remappable MSI format: interrupt index in address, SHV=0, data=0.
    pub fn address(self) -> u64 {
        0xfee0_0010 | ((self.index as u64) << 5)
    }
    pub fn data(self) -> u32 {
        0
    }
}
pub struct Vtd<H> {
    io: H,
    base: u64,
    control: u32,
    width: u8,
    requesters: [u64; 4],
    hpet_sid: Option<u16>,
    levels: u8,
    drain: u64,
    fault_offset: u64,
    fault_count: usize,
    root: Page,
    context: Page,
    queue: Page,
    completion: Page,
    interrupts: Page,
    pages: Vec<Page>,
    assigned: Vec<u16>,
    domains: Vec<(Domain, u64)>,
    routes: Vec<Option<u16>>,
    next_domain: u16,
    domain_limit: u16,
    next_interrupt: u16,
    tail: u16,
    sequence: u32,
    timeout: u64,
    poisoned: bool,
}
impl<H: Hardware> Vtd<H> {
    /// # Safety
    /// Covered PCI functions must have bus mastering disabled and stay disabled
    /// until a domain/interrupt route is installed. No prior IOMMU owner, ATS,
    /// device translation caches or firmware-required reserved mappings may exist.
    /// On any error, keep backing pinned, stop normal operation and force reboot.
    pub unsafe fn enable(mut io: H, dmar: dmar::Dmar, timeout_ticks: u64) -> Result<Self> {
        if timeout_ticks == 0
            || timeout_ticks >= (1 << 63)
            || !(32..=52).contains(&dmar.host_width)
            || dmar.registers == 0
            || dmar.registers & 4095 != 0
            || dmar
                .registers
                .checked_add(65536)
                .is_none_or(|end| end > (1u64 << dmar.host_width))
        {
            return Err(Error::Invalid);
        }
        let base = dmar.registers;
        let version = io.read32(base)?;
        let cap = io.read64(base + 8)?;
        let ecap = io.read64(base + 16)?;
        // Legacy tables with backend cache maintenance, QI and interrupt remap.
        // Reject extended interrupt mode requirements and active firmware state.
        if cap == u64::MAX
            || ecap == u64::MAX
            || cap & 7 == 7
            || cap & (1 << 4) != 0
            || ecap & 0x0a != 0x0a
            || ecap & (1 << 61) != 0
            || io.read32(base + 0x1c)? & (TE | QIE | IRE | CFI) != 0
        {
            return Err(Error::Unsupported);
        }
        let levels = if cap & (1 << 10) != 0 {
            4
        } else if cap & (1 << 9) != 0 {
            3
        } else {
            return Err(Error::Unsupported);
        };
        let width = (12 + levels * 9)
            .min((((cap >> 16) & 63) + 1) as u8)
            .min(dmar.host_width);
        if width < 32 {
            return Err(Error::Unsupported);
        }
        let drain = if version >> 4 >= 2 {
            0
        } else if cap & (3 << 54) == 3 << 54 {
            (1 << 6) | (1 << 7)
        } else {
            return Err(Error::Unsupported);
        };
        let fault_offset = ((cap >> 24) & 1023) * 16;
        let fault_count = ((cap >> 40) & 255) as usize + 1;
        if fault_offset < 0xc0 || fault_offset + fault_count as u64 * 16 > 65536 {
            return Err(Error::Unsupported);
        }
        let mut pages = Vec::new();
        for _ in 0..5 {
            let page = io.allocate_page()?;
            Self::check_page(page, width)?;
            if pages
                .iter()
                .any(|p: &Page| p.cookie == page.cookie || p.physical == page.physical)
            {
                return Err(Error::Backend);
            }
            pages.push(page);
        }
        let mut this = Self {
            io,
            base,
            requesters: dmar.requesters,
            hpet_sid: dmar.hpet_sid,
            control: 0,
            width,
            levels,
            drain,
            fault_offset,
            fault_count,
            root: pages[0],
            context: pages[1],
            queue: pages[2],
            completion: pages[3],
            interrupts: pages[4],
            pages,
            assigned: Vec::new(),
            domains: Vec::new(),
            routes: Vec::new(),
            next_domain: 1,
            domain_limit: (1u32 << (4 + 2 * (cap as u32 & 7))).min(64) as u16,
            next_interrupt: 0,
            tail: 0,
            sequence: 0,
            timeout: timeout_ticks,
            poisoned: false,
        };
        // Keep fault events masked until a trusted, bound kernel IRQ is armed.
        this.io.write32(base + 0x38, 1 << 31)?;
        this.io.write64(base + 0x20, this.root.physical)?;
        this.command(SRTP)?;
        if this.io.read64(base + 0x80)? != 0 {
            return Err(Error::Unsupported);
        }
        this.io.write64(base + 0x90, this.queue.physical)?;
        this.io.write64(base + 0x88, 0)?;
        this.control |= QIE;
        this.command(0)?;
        this.invalidate()?;
        this.io.write64(base + 0xb8, this.interrupts.physical | 7)?;
        this.command(SIRTP)?;
        this.submit(4, 0)?; // global interrupt-entry-cache invalidation
        this.sync()?;
        // CFI stays clear: compatibility-format MSI/IOAPIC interrupts are blocked.
        this.control |= IRE;
        this.command(0)?;
        this.control |= TE;
        this.command(0)?;
        Ok(this)
    }
    pub fn hardware(&mut self) -> &mut H {
        &mut self.io
    }
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }
    fn check_page(page: Page, width: u8) -> Result<()> {
        if page.physical == 0 || page.physical & !ADDR != 0 || page.physical >= (1u64 << width) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn allocate(&mut self) -> Result<Page> {
        // A hard bound on table RAM, independent of malicious mapping requests.
        if self.pages.len() >= 256 {
            return Err(Error::Exhausted);
        }
        self.pages.try_reserve(1).map_err(|_| Error::Exhausted)?;
        let page = self.io.allocate_page()?;
        Self::check_page(page, self.width)?;
        if self
            .pages
            .iter()
            .any(|p| p.cookie == page.cookie || p.physical == page.physical)
        {
            return Err(Error::Backend);
        }
        self.pages.push(page);
        Ok(page)
    }
    fn wait(&mut self, mut predicate: impl FnMut(&mut Self) -> Result<bool>) -> Result<()> {
        let started = self.io.now();
        loop {
            if predicate(self)? {
                return Ok(());
            }
            if self.io.read32(self.base + 0x34)? & 0x70 != 0 {
                return Err(Error::Backend);
            }
            if self.io.now().wrapping_sub(started) >= self.timeout {
                return Err(Error::Timeout);
            }
            core::hint::spin_loop();
        }
    }
    fn command(&mut self, one_shot: u32) -> Result<()> {
        fence(Ordering::SeqCst);
        self.io.write32(self.base + 0x18, self.control | one_shot)?;
        let expected = self.control | one_shot;
        self.wait(|s| {
            Ok(s.io.read32(s.base + 0x1c)? & (TE | QIE | IRE | CFI | one_shot) == expected)
        })
    }
    fn submit(&mut self, lo: u64, hi: u64) -> Result<()> {
        let next = (self.tail + 16) & 4095;
        self.wait(|s| Ok(s.io.read64(s.base + 0x80)? & 4095 != next as u64))?;
        let at = self.tail as usize / 8;
        self.io.write_page(self.queue, at, lo)?;
        self.io.write_page(self.queue, at + 1, hi)?;
        fence(Ordering::SeqCst);
        self.tail = next;
        self.io.write64(self.base + 0x88, next as u64)
    }
    fn sync(&mut self) -> Result<()> {
        self.sequence = self.sequence.wrapping_add(1);
        let sequence = self.sequence;
        self.submit(
            5 | (1 << 5) | (1 << 6) | ((sequence as u64) << 32),
            self.completion.physical,
        )?;
        self.wait(|s| Ok(s.io.read_page(s.completion, 0)? as u32 == sequence))?;
        fence(Ordering::SeqCst);
        Ok(())
    }
    fn invalidate(&mut self) -> Result<()> {
        self.submit(1 | (1 << 4), 0)?; // global context cache
        self.submit(2 | (1 << 4) | self.drain, 0)?; // global IOTLB, drain reads AND writes
        self.sync()
    }
    fn transaction<T>(&mut self, operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let result = operation(self);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn domain(&self, domain: Domain) -> Result<()> {
        if self
            .domains
            .iter()
            .any(|(d, _)| d.id == domain.id && d.sid == domain.sid && d.root == domain.root)
        {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
    /// Flat root-bus endpoints only. Bridges/aliases need requester-group support.
    /// The caller must validate topology; a numeric SID is not public authority.
    pub fn attach(&mut self, sid: u16) -> Result<Domain> {
        if sid >> 8 != 0 || self.requesters[sid as usize / 64] & (1 << (sid % 64)) == 0 {
            return Err(Error::Unsupported);
        }
        if self.assigned.contains(&sid) {
            return Err(Error::Invalid);
        }
        if self.next_domain >= self.domain_limit {
            return Err(Error::Exhausted);
        }
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        self.assigned.try_reserve(1).map_err(|_| Error::Exhausted)?;
        self.domains.try_reserve(1).map_err(|_| Error::Exhausted)?;
        let root = match self.allocate() {
            Ok(root) => root,
            Err(Error::Exhausted) => return Err(Error::Exhausted),
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        self.transaction(|s| {
            let domain = Domain {
                sid,
                id: s.next_domain,
                root,
            };
            s.next_domain += 1;
            s.io.write_page(
                s.context,
                sid as usize * 2 + 1,
                ((domain.id as u64) << 8) | (s.levels as u64 - 2),
            )?;
            s.io.write_page(s.context, sid as usize * 2, root.physical | 1)?;
            s.io.write_page(s.root, 0, s.context.physical | 1)?;
            s.invalidate()?;
            s.assigned.push(sid);
            s.domains.push((domain, 4096));
            Ok(domain)
        })
    }
    fn leaf(&mut self, domain: Domain, iova: u64, create: bool) -> Result<Option<(Page, usize)>> {
        let mut page = domain.root;
        for level in (1..self.levels).rev() {
            let index = ((iova >> (12 + level * 9)) & 511) as usize;
            let entry = self.io.read_page(page, index)?;
            page = if entry & 3 == 0 {
                if !create {
                    return Ok(None);
                }
                let child = self.allocate()?;
                self.io.write_page(page, index, child.physical | 3)?;
                child
            } else {
                *self
                    .pages
                    .iter()
                    .find(|p| p.physical == entry & ADDR)
                    .ok_or(Error::Backend)?
            };
        }
        Ok(Some((page, ((iova >> 12) & 511) as usize)))
    }
    fn range(&self, iova: u64, pages: usize) -> Result<()> {
        if pages == 0
            || pages > 16
            || iova < 4096
            || iova & 4095 != 0
            || iova
                .checked_add((pages as u64) * 4096)
                .is_none_or(|end| end > 0x8000_0000)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    /// Trusted broker only: physical pages must belong to a retained VM pin.
    /// IOVAs are monotonic and cannot be recycled even after unmap.
    pub fn reserve(&mut self, domain: Domain, iova: u64, pages: usize) -> Result<ReservedDma> {
        self.domain(domain)?;
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if pages == 0
            || pages > 16384
            || iova < 4096
            || iova & 4095 != 0
            || iova
                .checked_add(pages as u64 * 4096)
                .is_none_or(|end| end > 0x8000_0000)
        {
            return Err(Error::Invalid);
        }
        let high = &mut self
            .domains
            .iter_mut()
            .find(|(d, _)| d.id == domain.id)
            .unwrap()
            .1;
        if iova < *high {
            return Err(Error::Invalid);
        }
        *high = iova + pages as u64 * 4096;
        Ok(ReservedDma {
            domain,
            base: iova,
            pages,
            populated: 0,
            dirty: false,
        })
    }
    pub fn map_reserved(
        &mut self,
        r: &mut ReservedDma,
        physical: &[u64],
        access: u8,
    ) -> Result<()> {
        if physical.is_empty() || physical.len() > r.pages - r.populated {
            return Err(Error::Invalid);
        }
        self.map_chunk(
            r.domain,
            r.base + r.populated as u64 * 4096,
            physical,
            access,
            false,
        )?;
        r.populated += physical.len();
        r.dirty = true;
        Ok(())
    }
    pub fn map(
        &mut self,
        domain: Domain,
        iova: u64,
        physical: &[u64],
        device_access: u8,
    ) -> Result<()> {
        self.domain(domain)?;
        if iova
            < self
                .domains
                .iter()
                .find(|(d, _)| d.id == domain.id)
                .unwrap()
                .1
        {
            return Err(Error::Invalid);
        }
        self.map_chunk(domain, iova, physical, device_access, true)?;
        self.domains
            .iter_mut()
            .find(|(d, _)| d.id == domain.id)
            .unwrap()
            .1 = iova + physical.len() as u64 * 4096;
        Ok(())
    }
    /// Publish every deferred translation of a populated reservation with one
    /// global invalidation. Callers must keep the device from bus-mastering
    /// until this returns.
    pub fn commit_reserved(&mut self, r: &mut ReservedDma) -> Result<()> {
        if !r.dirty {
            return Ok(());
        }
        self.transaction(|s| s.invalidate())?;
        r.dirty = false;
        Ok(())
    }
    fn map_chunk(
        &mut self,
        domain: Domain,
        iova: u64,
        physical: &[u64],
        device_access: u8,
        flush: bool,
    ) -> Result<()> {
        self.domain(domain)?;
        self.range(iova, physical.len())?;
        if device_access == 0
            || device_access & !3 != 0
            || physical
                .iter()
                .any(|p| *p == 0 || *p & !ADDR != 0 || *p >= (1u64 << self.width))
        {
            return Err(Error::Invalid);
        }
        // Check every leaf before changing ANY of them, including overlap at the
        // end of a batch. A conflicting map is a caller error, not fatal hardware.
        let check = self.transaction(|s| {
            for n in 0..physical.len() {
                if let Some((page, index)) = s.leaf(domain, iova + n as u64 * 4096, false)? {
                    if s.io.read_page(page, index)? & 3 != 0 {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })?;
        if !check {
            return Err(Error::Invalid);
        }
        // Allocate ALL paths before publishing ANY source page. Ordinary table
        // exhaustion cannot become a partial DMA map requiring a system reboot.
        let mut leaves = Vec::new();
        leaves
            .try_reserve_exact(physical.len())
            .map_err(|_| Error::Exhausted)?;
        for n in 0..physical.len() {
            match self.leaf(domain, iova + n as u64 * 4096, true) {
                Ok(Some(leaf)) => leaves.push(leaf),
                Err(Error::Exhausted) => return Err(Error::Exhausted),
                _ => {
                    self.poisoned = true;
                    return Err(Error::Backend);
                }
            }
        }
        self.transaction(|s| {
            for ((page, index), paddr) in leaves.into_iter().zip(physical) {
                s.io.write_page(page, index, paddr | device_access as u64)?;
            }
            if flush {
                s.invalidate()?;
            }
            Ok(())
        })
    }
    /// Success acknowledges translation removal, read/write drain and completion.
    /// On error do not release the VM pin: the only recovery is forced reboot.
    pub fn unmap(&mut self, domain: Domain, iova: u64, pages: usize) -> Result<()> {
        self.domain(domain)?;
        self.range(iova, pages)?;
        self.transaction(|s| {
            for n in 0..pages {
                if let Some((page, index)) = s.leaf(domain, iova + n as u64 * 4096, false)? {
                    s.io.write_page(page, index, 0)?;
                }
            }
            s.invalidate()
        })
    }
    /// Remove several mappings and invalidate once. Used when tearing down a
    /// chunked reservation: the device is already stopped, and one global flush
    /// covers every removed leaf (a per-chunk flush made teardown as slow as
    /// the unbatched map path was).
    pub fn unmap_many(&mut self, domain: Domain, ranges: &[(u64, usize)]) -> Result<()> {
        self.domain(domain)?;
        for (iova, pages) in ranges {
            self.range(*iova, *pages)?;
        }
        self.transaction(|s| {
            for (iova, pages) in ranges {
                for n in 0..*pages {
                    if let Some((page, index)) = s.leaf(domain, *iova + n as u64 * 4096, false)? {
                        s.io.write_page(page, index, 0)?;
                    }
                }
            }
            s.invalidate()
        })
    }
    pub fn interrupt(&mut self, domain: Domain, vector: u8, destination: u8) -> Result<Interrupt> {
        self.domain(domain)?;
        self.interrupt_source(domain.sid, vector, destination)
    }
    /// Firmware-validated interrupt-only source, not a DMA domain/assignment.
    pub fn timer_interrupt(&mut self, vector: u8, destination: u8) -> Result<Interrupt> {
        let sid = self.hpet_sid.ok_or(Error::Unsupported)?;
        self.interrupt_source(sid, vector, destination)
    }
    fn interrupt_source(&mut self, sid: u16, vector: u8, destination: u8) -> Result<Interrupt> {
        if !(0x30..=0xdf).contains(&vector) {
            return Err(Error::Invalid);
        }
        if self.next_interrupt == 256 {
            return Err(Error::Exhausted);
        }
        self.routes.try_reserve(1).map_err(|_| Error::Exhausted)?;
        self.transaction(|s| {
            let index = s.next_interrupt;
            s.next_interrupt += 1;
            s.routes.push(Some(sid));
            // SVT=01: validate the full requester SID, no function masking.
            s.io.write_page(s.interrupts, index as usize * 2 + 1, (1 << 18) | sid as u64)?;
            s.io.write_page(
                s.interrupts,
                index as usize * 2,
                1 | ((vector as u64) << 16) | ((destination as u64) << 40),
            )?;
            s.submit(4, 0)?;
            s.sync()?;
            Ok(Interrupt { index, sid })
        })
    }
    pub fn remove_interrupt(&mut self, domain: Domain, interrupt: Interrupt) -> Result<()> {
        self.domain(domain)?;
        if interrupt.sid != domain.sid
            || self.routes.get(interrupt.index as usize).copied().flatten() != Some(domain.sid)
        {
            return Err(Error::Invalid);
        }
        self.transaction(|s| {
            s.io.write_page(s.interrupts, interrupt.index as usize * 2, 0)?;
            s.submit(4, 0)?;
            s.sync()?;
            s.routes[interrupt.index as usize] = None;
            Ok(())
        })
    }
    /// Route the remapping unit's OWN fault-event MSI, not a PCI device MSI.
    /// These unit-generated messages bypass interrupt remapping; compatibility
    /// interrupts from PCI devices remain blocked. Caller owns the kernel vector.
    pub fn fault_interrupt(&mut self, vector: u8, destination: u8) -> Result<()> {
        if vector < 32 {
            return Err(Error::Invalid);
        }
        self.transaction(|s| {
            s.io.write32(s.base + 0x38, 1 << 31)?;
            s.io.write32(s.base + 0x3c, vector as u32)?;
            s.io.write32(s.base + 0x40, 0xfee0_0000 | ((destination as u32) << 12))?;
            s.io.write32(s.base + 0x44, 0)?;
            s.io.write32(s.base + 0x38, 0)
        })
    }
    /// Mask while draining records; unmask only after the kernel IRQ is acked.
    /// A fault arriving while masked remains pending and is sent upon unmask.
    pub fn mask_fault_interrupt(&mut self, masked: bool) -> Result<()> {
        self.transaction(|s| {
            s.io.write32(s.base + 0x38, if masked { 1 << 31 } else { 0 })
        })
    }
    /// Ordinary device faults do not poison the IOMMU. Queue/timeout failures
    /// do. Overflow is reported, never mistaken for a complete fault inventory.
    pub fn poll_fault(&mut self) -> Result<FaultReport> {
        self.transaction(|s| {
            let status = s.io.read32(s.base + 0x34)?;
            if status & 0x70 != 0 {
                return Err(Error::Backend);
            }
            let overflow = status & 1 != 0;
            let mut fault = None;
            for index in 0..s.fault_count {
                let register = s.base + s.fault_offset + index as u64 * 16;
                let high = s.io.read64(register + 8)?;
                if high & (1 << 63) != 0 {
                    let address = s.io.read64(register)? & !4095;
                    fault = Some(Fault {
                        source: high as u16,
                        address,
                        reason: (high >> 32) as u8,
                        read: high & (1 << 62) != 0,
                    });
                    s.io.write64(register + 8, 1 << 63)?;
                    break;
                }
            }
            if overflow {
                s.io.write32(s.base + 0x34, 1)?;
            }
            Ok(FaultReport { fault, overflow })
        })
    }
    /// First revoke/acknowledge all MMIO mappings and prevent further config
    /// writes, then stop/reset the function and disable bus mastering. This does
    /// not recycle the requester ID/domain/IOVAs; reassignment is unsupported.
    pub fn detach(&mut self, domain: Domain) -> Result<()> {
        self.domain(domain)?;
        self.transaction(|s| {
            s.io.write_page(s.context, domain.sid as usize * 2, 0)?;
            for index in 0..s.routes.len() {
                if s.routes[index] == Some(domain.sid) {
                    s.io.write_page(s.interrupts, index * 2, 0)?;
                    s.routes[index] = None;
                }
            }
            s.submit(4, 0)?;
            s.invalidate()?;
            s.domains.retain(|(d, _)| d.id != domain.id);
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests;
