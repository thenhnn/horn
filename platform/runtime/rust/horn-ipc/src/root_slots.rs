//! Shared free-slot pool for a single flat root CNode.
extern crate alloc;
use alloc::{collections::BTreeSet, sync::Arc};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};
use sel4_abstract_allocator::{ByRange, basic::BasicAllocator};

struct Pool {
    range: Range<usize>,
    free: ByRange<BasicAllocator>,
}
/// Clones share free space; each client retains a separate ownership ledger.
/// A client must only release slots it allocated, after successful cap deletion.
#[derive(Clone)]
pub struct RootSlots(Arc<Locked<Pool>>);
pub(crate) struct Locked<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}
// Access is serialized by Guard; no kernel operations run under this lock.
unsafe impl<T: Send> Sync for Locked<T> {}
struct Guard<'a, T>(&'a Locked<T>);
impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}
impl<T> Locked<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        while self
            .busy
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        let _guard = Guard(self);
        f(unsafe { &mut *self.value.get() })
    }
}
impl RootSlots {
    pub fn new(range: Range<usize>) -> Self {
        assert!(range.start <= range.end);
        Self(Arc::new(Locked::new(Pool {
            free: ByRange::new(BasicAllocator::with_granule_size(range.len(), 1)),
            range,
        })))
    }
    pub fn range(&self) -> Range<usize> {
        self.0.with(|pool| pool.range.clone())
    }
    pub fn allocate(&self) -> Option<usize> {
        self.0.with(|pool| {
            let offset = pool.free.allocate(Layout::new::<u8>()).ok()?.start;
            Some(pool.range.start + offset)
        })
    }
    /// Caller guarantees this is its live allocation, not a borrowed cap.
    pub fn release(&self, slot: usize) {
        self.0.with(|pool| {
            assert!(pool.range.contains(&slot));
            let offset = slot - pool.range.start;
            pool.free.deallocate(offset..offset + 1);
        });
    }
}

/// Ownership ledger for IPC-local slots, distinct from kernel leases sharing
/// the same free pool. register() of a borrowed cap never acquires ownership.
pub(crate) struct LocalSlots {
    pool: RootSlots,
    owned: BTreeSet<usize>,
}
impl LocalSlots {
    pub fn new(pool: RootSlots) -> Self {
        Self {
            pool,
            owned: BTreeSet::new(),
        }
    }
    pub fn allocate(&mut self) -> Option<usize> {
        let slot = self.pool.allocate()?;
        assert!(self.owned.insert(slot));
        Some(slot)
    }
    /// Transfer ledger ownership to a local RAII handle, without freeing capacity.
    pub fn take(&mut self, slot: usize) -> bool {
        self.owned.remove(&slot)
    }
    /// The detached owner has successfully deleted its capability.
    pub fn release_taken(&self, slot: usize) {
        self.pool.release(slot);
    }
    pub fn release_if_owned(&mut self, slot: usize) {
        if self.owned.remove(&slot) {
            self.pool.release(slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_owner_retains_capacity_and_cannot_be_adopted_twice() {
        let pool = RootSlots::new(100..101);
        let mut ipc = LocalSlots::new(pool.clone());
        let slot = ipc.allocate().unwrap();
        assert!(ipc.take(slot));
        assert!(!ipc.take(slot));
        ipc.release_if_owned(slot); // transport no longer owns the slot
        assert!(pool.allocate().is_none());
        ipc.release_taken(slot); // only after successful kernel deletion
        assert_eq!(pool.allocate(), Some(slot));
    }

    #[test]
    fn kernel_and_ipc_share_capacity_without_freeing_each_others_slots() {
        let pool = RootSlots::new(100..104);
        let kernel = pool.clone();
        let anchor = kernel.allocate().unwrap();
        let mut ipc = LocalSlots::new(pool);
        ipc.release_if_owned(anchor); // borrowed/registered kernel cap
        let imports: alloc::vec::Vec<_> = (0..3).map(|_| ipc.allocate().unwrap()).collect();
        assert!(!imports.contains(&anchor));
        assert!(kernel.allocate().is_none());
        ipc.release_if_owned(imports[0]);
        ipc.release_if_owned(imports[0]); // must not double-free
        assert_eq!(kernel.allocate(), Some(imports[0]));
        assert!(ipc.allocate().is_none());
        kernel.release(anchor);
        assert_eq!(ipc.allocate(), Some(anchor));
        assert!(kernel.allocate().is_none());
    }
}
