//! seL4 retyping and revocation; bookkeeping uses rust-sel4's range allocator.

use alloc::vec::Vec;
use core::{alloc::Layout, ops::Range};

use crate::{Error, Frames};
use ::sel4::{
    Cap, CapTypeForObjectOfFixedSize, CapTypeForObjectOfVariableSize, ObjectBlueprint, Word, cap,
    cap_type,
};
use sel4_abstract_allocator::{
    AbstractAllocator, AbstractAllocatorAllocation, ByRange,
    basic::{Allocation, BasicAllocator},
};

pub const PAGE_SIZE: usize = ::sel4::FrameObjectType::GRANULE.bytes();

pub struct KernelAllocator {
    root: cap::CNode,
    untyped: cap::Untyped,
    first_slot: usize,
    slots: ByRange<BasicAllocator>,
}

impl KernelAllocator {
    pub fn new(root: cap::CNode, untyped: cap::Untyped, slots: Range<usize>) -> Self {
        Self {
            root,
            untyped,
            first_slot: slots.start,
            slots: ByRange::new(BasicAllocator::with_granule_size(slots.len(), 1)),
        }
    }

    pub fn root(&self) -> cap::CNode {
        self.root
    }

    pub fn reserve_slot(&mut self) -> Result<usize, Error> {
        self.slots
            .allocate(Layout::new::<u8>())
            .map(|range| self.first_slot + range.start)
            .map_err(|_| Error::Exhausted)
    }

    pub fn release_slot(&mut self, slot: usize) -> Result<(), Error> {
        self.root
            .absolute_cptr(::sel4::CPtr::from_bits(slot as Word))
            .delete()
            .map_err(|_| Error::Backend)?;
        let offset = slot - self.first_slot;
        self.slots.deallocate(offset..offset + 1);
        Ok(())
    }

    pub fn allocate(&mut self, blueprint: ObjectBlueprint) -> Result<cap::Unspecified, Error> {
        let slot = self.reserve_slot()?;
        if self
            .untyped
            .untyped_retype(&blueprint, &self.root.absolute_cptr_for_self(), slot, 1)
            .is_err()
        {
            self.release_slot(slot)?;
            return Err(Error::Backend);
        }
        Ok(Cap::from_bits(slot as Word))
    }

    pub fn allocate_fixed_sized<T: CapTypeForObjectOfFixedSize>(
        &mut self,
    ) -> Result<Cap<T>, Error> {
        self.allocate(T::object_blueprint()).map(|cap| cap.cast())
    }

    pub fn allocate_variable_sized<T: CapTypeForObjectOfVariableSize>(
        &mut self,
        bits: usize,
    ) -> Result<Cap<T>, Error> {
        self.allocate(T::object_blueprint(bits))
            .map(|cap| cap.cast())
    }
}

struct Page {
    untyped: cap::Untyped,
    slot: usize,
}

#[must_use]
pub struct Frame {
    pool: Word,
    allocation: Allocation,
}

pub struct FramePool {
    root: cap::CNode,
    pages: Vec<Page>,
    free: BasicAllocator,
    available: usize,
}

impl FramePool {
    pub fn new(allocator: &mut KernelAllocator, count: usize) -> Result<Self, Error> {
        if count == 0 {
            return Err(Error::InvalidSize);
        }
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(count)
            .map_err(|_| Error::MetadataExhausted)?;
        for _ in 0..count {
            let untyped = allocator
                .allocate(ObjectBlueprint::Untyped {
                    size_bits: PAGE_SIZE.trailing_zeros() as usize,
                })?
                .cast();
            let slot = allocator.reserve_slot()?;
            pages.push(Page { untyped, slot });
        }
        Ok(Self {
            root: allocator.root,
            pages,
            free: BasicAllocator::with_granule_size(count, 1),
            available: count,
        })
    }

    pub fn capacity(&self) -> usize {
        self.pages.len()
    }
    pub fn available(&self) -> usize {
        self.available
    }

    /// Trusted kernel access; raw frame caps are never exported by memory RPC.
    pub fn cap(&self, frame: &Frame) -> Result<cap::Granule, Error> {
        let index = self.index(frame)?;
        Ok(Cap::<cap_type::Granule>::from_bits(
            self.pages[index].slot as Word,
        ))
    }

    fn index(&self, frame: &Frame) -> Result<usize, Error> {
        if frame.pool != self.pages[0].untyped.bits() {
            return Err(Error::Backend);
        }
        let index = frame.allocation.range().start;
        if index >= self.pages.len() {
            return Err(Error::Backend);
        }
        Ok(index)
    }
}

impl Frames for FramePool {
    type Frame = Frame;
    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    fn allocate_zeroed(&mut self) -> Result<Frame, Error> {
        let allocation = self
            .free
            .allocate(Layout::new::<u8>())
            .map_err(|_| Error::Exhausted)?;
        self.available -= 1;
        let page = &self.pages[allocation.range().start];
        // Retyping clears frame contents. Failed operations quarantine the allocation.
        page.untyped
            .untyped_retype(
                &::sel4::FrameObjectType::GRANULE.blueprint(),
                &self.root.absolute_cptr_for_self(),
                page.slot,
                1,
            )
            .map_err(|_| Error::Backend)?;
        Ok(Frame {
            pool: self.pages[0].untyped.bits(),
            allocation,
        })
    }

    fn release(&mut self, frame: Frame) -> Result<(), Error> {
        let index = self.index(&frame)?;
        self.root
            .absolute_cptr(::sel4::CPtr::from_bits(self.pages[index].untyped.bits()))
            .revoke()
            .map_err(|_| Error::Backend)?;
        self.free.deallocate(frame.allocation);
        self.available += 1;
        Ok(())
    }
}
