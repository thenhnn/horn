use core::alloc::Layout;
use sel4_abstract_allocator::{ByRange, basic::BasicAllocator};

#[test]
fn sdk_allocator_reclaims_lowest_hole_and_both_neighbors() {
    for order in [
        [0, 1, 2],
        [2, 1, 0],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
    ] {
        let mut allocator = ByRange::new(BasicAllocator::with_granule_size(16, 1));
        let allocations: [_; 3] = core::array::from_fn(|_| {
            allocator
                .allocate(Layout::from_size_align(4, 1).unwrap())
                .unwrap()
        });
        for index in order {
            allocator.deallocate(allocations[index].clone());
        }
        assert_eq!(
            allocator
                .allocate(Layout::from_size_align(16, 1).unwrap())
                .unwrap(),
            0..16
        );
    }
}

#[test]
fn sdk_slots_are_reused_over_more_than_cspace_capacity() {
    let mut allocator = ByRange::new(BasicAllocator::with_granule_size(8, 1));
    for _ in 0..1024 {
        let slot = allocator.allocate(Layout::new::<u8>()).unwrap();
        assert_eq!(slot, 0..1);
        allocator.deallocate(slot);
    }
}

#[test]
fn page_ranges_reject_overflow_alignment_and_empty_spans() {
    assert_eq!(super::page_range(4096, 8192, 12288, 4096), Ok(1..3));
    for (offset, length, limit, page) in [
        (0, 0, 4096, 4096),
        (1, 4096, 8192, 4096),
        (0, 1, 8192, 4096),
        (u64::MAX - 4095, 4096, u64::MAX, 4096),
        (0, 8192, 4096, 4096),
        (0, 4096, 4096, 0),
    ] {
        assert!(super::page_range(offset, length, limit, page).is_err());
    }
}
