//! Local capability ownership, independent of RPC transport and remote lifetime.
extern crate alloc;
use crate::{Cap, CapRef, CapType, RpcError, RpcInterface};
use alloc::{rc::Rc, sync::Arc};
use core::marker::PhantomData;

/// Low-level access used by generated receivers. Extracting a raw selector does
/// not retain its owner; callers must not use it after deletion or transfer.
pub trait CapView<T: CapType> {
    fn cap_ref(&self) -> CapRef;
}
impl<T: CapType, V: CapView<T> + ?Sized> CapView<T> for &V {
    fn cap_ref(&self) -> CapRef {
        (**self).cap_ref()
    }
}
impl<T: CapType> CapView<T> for Cap<T> {
    fn cap_ref(&self) -> CapRef {
        self.as_ref()
    }
}

/// A capability borrowed from a local owner. Copying does not extend its lifetime.
pub struct BorrowedCap<'a, T: CapType> {
    cap: Cap<T>,
    lifetime: PhantomData<&'a Rc<()>>,
}
impl<T: CapType> Copy for BorrowedCap<'_, T> {}
impl<T: CapType> Clone for BorrowedCap<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: CapType> CapView<T> for BorrowedCap<'_, T> {
    fn cap_ref(&self) -> CapRef {
        self.cap.as_ref()
    }
}
impl<'a, T: CapType> BorrowedCap<'a, T> {
    /// Select a protocol view; this is not a remote type or authority check.
    pub fn cast<U: CapType>(self) -> BorrowedCap<'a, U> {
        BorrowedCap {
            cap: Cap::from_ref(self.cap.as_ref()),
            lifetime: PhantomData,
        }
    }
}

pub trait LocalCapSpace: Send + Sync {
    /// Restore raw ownership to the originating ledger, without freeing the slot.
    fn return_raw(&self, cap: CapRef);
    /// Delete first, then release capacity. On error capacity stays reserved.
    fn delete_owned(&self, cap: CapRef) -> Result<(), RpcError>;
}

/// Owns exactly one local slot, not the remote object behind the capability.
/// Drop deletes the local capability; it never performs a protocol close/cancel.
/// Like other allocator-backed values, releasing its context can run heap callbacks.
/// Not Send/Sync: cleanup uses the creating thread's IPC invocation context.
#[must_use = "dropping this handle deletes its local capability"]
pub struct OwnedCap<T: CapType> {
    cap: Cap<T>,
    space: Option<Arc<dyn LocalCapSpace>>,
    thread_local: PhantomData<Rc<()>>,
}
impl<T: CapType> OwnedCap<T> {
    pub(crate) fn new(cap: Cap<T>, space: Arc<dyn LocalCapSpace>) -> Self {
        Self {
            cap,
            space: Some(space),
            thread_local: PhantomData,
        }
    }
    /// # Safety
    /// The slot must be live and exclusively owned; space must delete it exactly
    /// once or restore it to its raw ledger. Abandon other raw aliases.
    pub unsafe fn from_raw_in(cap: Cap<T>, space: Arc<dyn LocalCapSpace>) -> Self {
        Self::new(cap, space)
    }
    /// Explicit low-level handoff. Keep the originating cap-space alive and
    /// resume its raw cleanup/transfer discipline; no kernel operation occurs.
    pub fn into_raw(mut self) -> Cap<T> {
        self.space.take().unwrap().return_raw(self.cap.as_ref());
        self.cap
    }
    pub(crate) fn cast<U: CapType>(mut self) -> OwnedCap<U> {
        OwnedCap {
            cap: Cap::from_ref(self.cap.as_ref()),
            space: self.space.take(),
            thread_local: PhantomData,
        }
    }
    pub fn decode(
        input: &mut crate::Decoder<'_>,
        mode: crate::CapMode,
        rights: crate::Rights,
    ) -> Result<Self, RpcError> {
        Ok(input.caps.pop_owned(mode, rights)?.cast())
    }
    pub fn borrow(&self) -> BorrowedCap<'_, T> {
        BorrowedCap {
            cap: self.cap,
            lifetime: PhantomData,
        }
    }
    pub fn interface<I: RpcInterface>(&self) -> I::Borrowed<'_> {
        I::from_borrowed(self.borrow().cast())
    }
}
impl<T: CapType> CapView<T> for OwnedCap<T> {
    fn cap_ref(&self) -> CapRef {
        self.cap.as_ref()
    }
}
impl<T: CapType> Drop for OwnedCap<T> {
    fn drop(&mut self) {
        // Failure is fail-stop, not a silent leak or reuse of an uncertain slot.
        // No allocator lock/RefCell borrow may be held across the kernel syscall.
        let Some(space) = &self.space else { return };
        if let Err(error) = space.delete_owned(self.cap.as_ref()) {
            panic!(
                "local capability deletion failed: {:?}: {:?}",
                self.cap.as_ref(),
                error
            );
        }
    }
}

impl<T: CapType> core::fmt::Debug for OwnedCap<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.cap.fmt(f)
    }
}
impl<T: CapType> PartialEq for OwnedCap<T> {
    fn eq(&self, other: &Self) -> bool {
        self.cap == other.cap
    }
}
impl<T: CapType> Eq for OwnedCap<T> {}

/// Bounded decoded list without dummy/Copy capability owners in inactive cells.
#[derive(Debug, PartialEq)]
pub struct OwnedList<T, const N: usize> {
    pub len: u32,
    pub data: alloc::vec::Vec<T>,
}
impl<T, const N: usize> OwnedList<T, N> {
    pub fn into_raw<U: Copy>(self, empty: U, convert: impl Fn(T) -> U) -> crate::List<U, N> {
        assert_eq!(self.len as usize, self.data.len());
        assert!(self.data.len() <= N);
        let mut raw = crate::List {
            len: self.len,
            data: [empty; N],
        };
        for (i, value) in self.data.into_iter().enumerate() {
            raw.data[i] = convert(value);
        }
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CapMode, Endpoint, Rights};
    use core::sync::atomic::{AtomicUsize, Ordering};
    struct Space(AtomicUsize);
    impl LocalCapSpace for Space {
        fn return_raw(&self, _: CapRef) {}
        fn delete_owned(&self, _: CapRef) -> Result<(), RpcError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    #[test]
    #[should_panic(expected = "local capability deletion failed")]
    fn failed_local_deletion_is_fail_stop() {
        struct FailingSpace;
        impl LocalCapSpace for FailingSpace {
            fn return_raw(&self, _: CapRef) {}
            fn delete_owned(&self, _: CapRef) -> Result<(), RpcError> {
                Err(RpcError::Transport)
            }
        }
        drop(OwnedCap::<Endpoint>::new(
            Cap::default(),
            Arc::new(FailingSpace),
        ));
    }

    #[test]
    fn raw_handoff_restores_raw_ownership_without_deleting_the_capability() {
        #[derive(Default)]
        struct Tracking {
            returned: AtomicUsize,
            deleted: AtomicUsize,
        }
        impl LocalCapSpace for Tracking {
            fn return_raw(&self, _: CapRef) {
                self.returned.fetch_add(1, Ordering::Relaxed);
            }
            fn delete_owned(&self, _: CapRef) -> Result<(), RpcError> {
                self.deleted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
        let space = Arc::new(Tracking::default());
        let cap = Cap::<Endpoint>::new(0, 42, 64, 0, CapMode::COPY, Rights::ALL);
        let owner = OwnedCap::new(cap, space.clone());
        assert_eq!(owner.into_raw(), cap);
        assert_eq!(space.returned.load(Ordering::Relaxed), 1);
        assert_eq!(space.deleted.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn returning_an_rpc_error_still_drops_the_local_owner() {
        fn fail(owner: OwnedCap<Endpoint>) -> Result<(), RpcError> {
            let _ = owner.borrow().cap_ref();
            Err(RpcError::Transport)
        }
        let space = Arc::new(Space(AtomicUsize::new(0)));
        let owner = OwnedCap::new(Cap::default(), space.clone());
        assert_eq!(fail(owner), Err(RpcError::Transport));
        assert_eq!(space.0.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn moves_and_borrows_do_not_duplicate_cleanup_and_owner_retains_context() {
        let space = Arc::new(Space(AtomicUsize::new(0)));
        let observer = space.clone();
        let owned = OwnedCap::new(
            Cap::<Endpoint>::new(0, 42, 64, 0, CapMode::COPY, Rights::ALL),
            space,
        );
        assert_eq!(owned.borrow().cap_ref().slot, 42);
        let moved = owned;
        assert_eq!(observer.0.load(Ordering::Relaxed), 0);
        drop(moved);
        assert_eq!(observer.0.load(Ordering::Relaxed), 1);
        assert_eq!(Arc::strong_count(&observer), 1);
    }
}
