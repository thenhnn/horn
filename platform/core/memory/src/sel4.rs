//! seL4 resident-page provider. Each pool page has its own Untyped ancestor,
//! so revocation can reclaim it without touching unrelated allocations.

use alloc::vec::Vec;
use core::ops::Range;

use ::sel4::{cap, cap_type, Cap, CapTypeForObjectOfFixedSize, ObjectBlueprint, Word};
use crate::{Error, Frames};

pub const PAGE_SIZE: usize = ::sel4::FrameObjectType::GRANULE.bytes();

/// Bootstrap allocator. Slots and physical authority are supplied explicitly.
/// It is deliberately monotonic; runtime page reuse belongs to FramePool.
pub struct KernelAllocator {
    root: cap::CNode,
    untyped: cap::Untyped,
    slots: Range<usize>,
}

impl KernelAllocator {
    pub fn new(root: cap::CNode, untyped: cap::Untyped, slots: Range<usize>) -> Self {
        Self { root, untyped, slots }
    }

    pub fn reserve_slot(&mut self) -> Result<usize, Error> {
        self.slots.next().ok_or(Error::Exhausted)
    }

    pub fn allocate(&mut self, blueprint: ObjectBlueprint) -> Result<cap::Unspecified, Error> {
        let slot = self.slots.clone().next().ok_or(Error::Exhausted)?;
        self.untyped.untyped_retype(&blueprint, &self.root.absolute_cptr_for_self(), slot, 1)
            .map_err(|_| Error::Backend)?;
        self.slots.next();
        Ok(Cap::from_bits(slot as Word))
    }

    pub fn allocate_fixed_sized<T: CapTypeForObjectOfFixedSize>(&mut self) -> Result<Cap<T>, Error> {
        self.allocate(T::object_blueprint()).map(|cap| cap.cast())
    }
}

struct Page {
    untyped: cap::Untyped,
    slot: usize,
    generation: u64,
    leased: bool,
    quarantined: bool,
}

#[must_use]
pub struct Frame {
    pool: Word,
    index: usize,
    generation: u64,
}

/// Fixed-capacity pool. This initial backend never exports frame capabilities.
/// The scratch address must remain exclusively owned by the pool.
pub struct FramePool {
    root: cap::CNode,
    vspace: cap::VSpace,
    scratch: usize,
    pages: Vec<Page>,
    identity: Word,
    scratch_healthy: bool,
}

impl FramePool {
    /// Provision a resident page budget at bootstrap.
    ///
    /// # Safety
    /// `scratch` must name one reserved, currently unmapped base page in
    /// `vspace`, accessible in the calling thread's address space. No other
    /// code may map or access that range while this pool exists.
    pub unsafe fn new(
        allocator: &mut KernelAllocator,
        root: cap::CNode,
        vspace: cap::VSpace,
        scratch: usize,
        count: usize,
    ) -> Result<Self, Error> {
        if count == 0 || scratch % PAGE_SIZE != 0 { return Err(Error::InvalidSize); }
        let mut pages = Vec::new();
        pages.try_reserve_exact(count).map_err(|_| Error::MetadataExhausted)?;
        for _ in 0..count {
            let untyped = allocator.allocate(ObjectBlueprint::Untyped {
                size_bits: PAGE_SIZE.trailing_zeros() as usize,
            })?.cast();
            let slot = allocator.reserve_slot()?;
            pages.push(Page { untyped, slot, generation: 0, leased: false, quarantined: false });
        }
        let identity = pages[0].untyped.bits();
        Ok(Self { root, vspace, scratch, pages, identity, scratch_healthy: true })
    }

    pub fn capacity(&self) -> usize { self.pages.len() }
    pub fn available(&self) -> usize {
        self.pages.iter().filter(|page| !page.leased && !page.quarantined).count()
    }

    /// Trusted diagnostic: write a byte while no external mappings exist.
    pub fn fill(&mut self, frame: &Frame, value: u8) -> Result<(), Error> {
        let index = self.validate(frame)?;
        self.with_page(index, |address| unsafe { core::ptr::write_bytes(address, value, PAGE_SIZE) })
    }

    pub fn is_zeroed(&mut self, frame: &Frame) -> Result<bool, Error> {
        let index = self.validate(frame)?;
        self.with_page(index, |address| unsafe {
            core::slice::from_raw_parts(address, PAGE_SIZE).iter().all(|byte| *byte == 0)
        })
    }

    fn validate(&self, frame: &Frame) -> Result<usize, Error> {
        let page = self.pages.get(frame.index).ok_or(Error::Backend)?;
        if frame.pool != self.identity || !page.leased || page.generation != frame.generation || page.quarantined {
            return Err(Error::Backend);
        }
        Ok(frame.index)
    }

    fn with_page<T>(&mut self, index: usize, operation: impl FnOnce(*mut u8) -> T) -> Result<T, Error> {
        if !self.scratch_healthy { return Err(Error::Backend); }
        let frame = Cap::<cap_type::Granule>::from_bits(self.pages[index].slot as Word);
        frame.frame_map(self.vspace, self.scratch, ::sel4::CapRights::read_write(), ::sel4::VmAttributes::default())
            .map_err(|_| Error::Backend)?;
        let result = operation(self.scratch as *mut u8);
        if frame.frame_unmap().is_err() {
            self.scratch_healthy = false;
            self.pages[index].quarantined = true;
            return Err(Error::Backend);
        }
        Ok(result)
    }
}

impl Frames for FramePool {
    type Frame = Frame;
    fn page_size(&self) -> usize { PAGE_SIZE }

    fn allocate_zeroed(&mut self) -> Result<Frame, Error> {
        if !self.scratch_healthy { return Err(Error::Backend); }
        let index = self.pages.iter().position(|page| !page.leased && !page.quarantined).ok_or(Error::Exhausted)?;
        let page = &mut self.pages[index];
        let generation = page.generation.checked_add(1).ok_or(Error::Exhausted)?;
        page.untyped.untyped_retype(&::sel4::FrameObjectType::GRANULE.blueprint(),
            &self.root.absolute_cptr_for_self(), page.slot, 1).map_err(|_| Error::Backend)?;
        page.leased = true;
        page.generation = generation;
        let frame = Frame { pool: self.identity, index, generation };
        if let Err(error) = self.fill(&frame, 0) {
            // Revocation failures quarantine backing; never reuse it blindly.
            let _ = self.release(frame);
            return Err(error);
        }
        Ok(frame)
    }

    fn release(&mut self, frame: Frame) -> Result<(), Error> {
        let index = self.validate(&frame)?;
        let page = &mut self.pages[index];
        if self.root.absolute_cptr(::sel4::CPtr::from_bits(page.untyped.bits())).revoke().is_err() {
            page.quarantined = true;
            return Err(Error::Backend);
        }
        page.leased = false;
        Ok(())
    }
}
