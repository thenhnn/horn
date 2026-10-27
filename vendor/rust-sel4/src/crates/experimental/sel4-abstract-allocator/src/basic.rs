//
// Copyright 2023, Colias Group, LLC
//
// SPDX-License-Identifier: BSD-2-Clause
//

use alloc::collections::BTreeMap;
use core::alloc::Layout;
use core::ops::Range;

use crate::{AbstractAllocator, AbstractAllocatorAllocation};

// TODO
// This is basically just a free list. Should use a more efficient implementation.

// NOTE
// Once #![feature(allocator_api)] and #![feature(btreemap_alloc)] land, this should be
// parameterized with an allocator A, to enable this type to be used without a global allocator.

// NOTE
// #![feature(btree_cursors)] would make this stand-in implementation simpler and more efficient.
// See git history.

const DEFAULT_GRANULE_SIZE: usize = 512;

type Offset = usize;
type Size = usize;

pub struct BasicAllocator {
    granule_size: usize,
    holes: BTreeMap<Offset, Size>,
}

impl BasicAllocator {
    pub fn new(size: Size) -> Self {
        Self::with_granule_size(size, DEFAULT_GRANULE_SIZE)
    }

    pub fn with_granule_size(size: usize, granule_size: usize) -> Self {
        assert_eq!(size % granule_size, 0);
        let mut holes = BTreeMap::new();
        holes.insert(0, size);
        Self {
            granule_size,
            holes,
        }
    }

    fn granule_size(&self) -> usize {
        self.granule_size
    }
}

impl AbstractAllocator for BasicAllocator {
    type AllocationError = InsufficientResources;

    type Allocation = Allocation;

    fn allocate(&mut self, orig_layout: Layout) -> Result<Self::Allocation, Self::AllocationError> {
        let layout = Layout::from_size_align(
            orig_layout.size().next_multiple_of(self.granule_size()),
            orig_layout.align().max(self.granule_size()),
        )
        .unwrap();

        let (buffer_offset, hole_offset, hole_size) = self
            .holes
            .iter()
            .find_map(|(&hole_offset, &hole_size)| {
                let buffer_offset = hole_offset.next_multiple_of(layout.align());
                if buffer_offset + layout.size() <= hole_offset + hole_size {
                    Some((buffer_offset, hole_offset, hole_size))
                } else {
                    None
                }
            })
            .ok_or(InsufficientResources::new())?;

        self.holes.remove(&hole_offset).unwrap();

        if hole_offset < buffer_offset {
            self.holes.insert(hole_offset, buffer_offset - hole_offset);
        }

        let hole_end_offset = hole_offset + hole_size;
        let buffer_end_offset = buffer_offset + layout.size();

        if buffer_end_offset < hole_end_offset {
            self.holes
                .insert(buffer_end_offset, hole_end_offset - buffer_end_offset);
        }

        Ok(Allocation::new(
            buffer_offset..(buffer_offset + orig_layout.size()),
        ))
    }

    fn deallocate(&mut self, allocation: Self::Allocation) {
        let range = allocation.range();
        let offset = range.start;
        let size = range.len();

        assert_eq!(offset % self.granule_size(), 0);
        let size = size.next_multiple_of(self.granule_size());

        let previous = self
            .holes
            .range(..&offset)
            .next_back()
            .map(copy_typle_fields);
        let next = self.holes.range(&offset..).next().map(copy_typle_fields);
        let mut start = offset;
        let mut length = size;
        if let Some((hole_offset, hole_size)) = previous {
            assert!(hole_offset + hole_size <= offset);
            if hole_offset + hole_size == offset {
                self.holes.remove(&hole_offset);
                start = hole_offset;
                length += hole_size;
            }
        }
        if let Some((hole_offset, hole_size)) = next {
            assert!(offset + size <= hole_offset);
            if offset + size == hole_offset {
                self.holes.remove(&hole_offset);
                length += hole_size;
            }
        }
        self.holes.insert(start, length);
    }
}

pub struct Allocation(Range<usize>);

impl Allocation {
    fn new(range: Range<usize>) -> Self {
        Self(range)
    }
}

impl AbstractAllocatorAllocation for Allocation {
    fn range(&self) -> Range<usize> {
        self.0.clone()
    }
}

#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct InsufficientResources(());

impl InsufficientResources {
    fn new() -> Self {
        Self(())
    }
}

fn copy_typle_fields<T: Copy, U: Copy>((&t, &u): (&T, &U)) -> (T, U) {
    (t, u)
}
