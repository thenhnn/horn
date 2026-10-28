//! Checked local CSpace partitioning, not capability authority or discovery.
use core::ops::Range;

/// Free slots follow the generated bootstrap prefix. Reserve one public receive
/// slot, then an application RPC receive/import pool in the lower half, and a
/// separate heap callback receive/import pool in the upper half. Each transport
/// consumes its pool's first slot for receive. All bounds are half-open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskSlots {
    first: u32,
    split: u32,
    end: u32,
}
impl TaskSlots {
    /// Panics on an invalid layout; usable in a const so bad static bootstrap
    /// configurations fail at compile time, before any capability is touched.
    pub const fn new(first_free: u32, cnode_bits: u8) -> Self {
        assert!(cnode_bits >= 3 && cnode_bits < 32, "unsupported CNode size");
        let end = 1u32 << cnode_bits;
        let split = end / 2;
        assert!(
            first_free != 0 && first_free <= split - 3,
            "bootstrap prefix leaves no application import pool"
        );
        Self {
            first: first_free,
            split,
            end,
        }
    }
    pub const fn public_receive(self) -> u32 {
        self.first
    }
    pub const fn application(self) -> Range<u32> {
        self.first + 1..self.split
    }
    pub const fn heap(self) -> Range<u32> {
        self.split..self.end
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn pools_partition_every_free_slot_without_overlap() {
        for bits in 3..12 {
            let end = 1 << bits;
            for first in 1..end / 2 - 2 {
                let slots = TaskSlots::new(first, bits);
                let app = slots.application();
                let heap = slots.heap();
                assert_eq!(slots.public_receive(), first);
                assert_eq!(app.start, first + 1);
                assert!(app.start + 1 < app.end);
                assert_eq!(app.end, heap.start);
                assert!(heap.start + 1 < heap.end);
                assert_eq!(heap.end, end);
            }
        }
    }
    #[test]
    fn minimum_pool_has_a_receive_and_one_import() {
        const SLOTS: TaskSlots = TaskSlots::new(1, 3);
        assert_eq!(SLOTS.public_receive(), 1);
        assert_eq!(SLOTS.application(), 2..4);
        assert_eq!(SLOTS.heap(), 4..8);
        assert_eq!(TaskSlots::new(125, 8).application(), 126..128);
    }
    #[test]
    fn invalid_sizes_and_prefixes_fail_before_allocation() {
        for (first, bits) in [
            (0, 8),
            (1, 0),
            (1, 2),
            (1, 32),
            (1, u8::MAX),
            (126, 8),
            (256, 8),
            (u32::MAX, 8),
        ] {
            assert!(std::panic::catch_unwind(|| TaskSlots::new(first, bits)).is_err());
        }
    }
}
