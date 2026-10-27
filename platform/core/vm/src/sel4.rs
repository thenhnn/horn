//! Reclaimable Untyped trees. Managed objects and delegated leases use the same RAM.
use crate::{Error, Frames, account::Account};
use ::sel4::{
    Cap, CapTypeForObjectOfFixedSize, CapTypeForObjectOfVariableSize, ObjectBlueprint, Word, cap,
    cap_type,
};
use alloc::{collections::BTreeMap, rc::Rc, vec::Vec};
use core::{alloc::Layout, cell::RefCell, ops::Range};
use sel4_abstract_allocator::{ByRange, basic::BasicAllocator};

pub const PAGE_SIZE: usize = ::sel4::FrameObjectType::GRANULE.bytes();
const MIN_BITS: usize = PAGE_SIZE.trailing_zeros() as usize;

#[derive(Clone)]
pub struct KernelAllocator {
    inner: Rc<RefCell<Allocator>>,
    account: Account,
}
struct Allocator {
    root: cap::CNode,
    first_slot: usize,
    slots: ByRange<BasicAllocator>,
    nodes: Vec<Node>,
    objects: BTreeMap<usize, Lease>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Split,
    Used,
    Dormant,
    Quarantined,
}
struct Node {
    cap: cap::Untyped,
    bits: usize,
    parent: Option<usize>,
    children: Option<[usize; 2]>,
    state: State,
    owner: Option<Account>,
}
struct Lease {
    node: usize,
    account: Account,
}

/// The retained root capability is never moved to the client. Export a copy.
/// Dropping a lease does not return its RAM or remove its charge.
#[must_use]
pub struct UntypedLease {
    node: usize,
    cap: cap::Untyped,
    bits: usize,
    account: Account,
    allocator: Rc<RefCell<Allocator>>,
}
impl UntypedLease {
    pub fn cap(&self) -> cap::Untyped {
        self.cap
    }
    pub fn size_bits(&self) -> usize {
        self.bits
    }
}

impl Allocator {
    fn reserve_slot(&mut self) -> Result<usize, Error> {
        self.slots
            .allocate(Layout::new::<u8>())
            .map(|range| self.first_slot + range.start)
            .map_err(|_| Error::Exhausted)
    }
    fn clear_slot(&self, slot: usize) -> Result<(), Error> {
        self.root
            .absolute_cptr(::sel4::CPtr::from_bits(slot as Word))
            .delete()
            .map_err(|_| Error::Backend)
    }
    fn free_slot(&mut self, slot: usize) -> Result<(), Error> {
        self.clear_slot(slot)?;
        let offset = slot.checked_sub(self.first_slot).ok_or(Error::Backend)?;
        self.slots.deallocate(offset..offset + 1);
        Ok(())
    }
    fn revoke(&self, node: usize) -> Result<(), Error> {
        self.root
            .absolute_cptr(self.nodes[node].cap.cptr())
            .revoke()
            .map_err(|_| Error::Backend)
    }
    fn acquire(&mut self, bits: usize) -> Result<usize, Error> {
        let mut node = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.state == State::Free && n.bits >= bits)
            .min_by_key(|(_, n)| n.bits)
            .map(|(i, _)| i)
            .ok_or(Error::Exhausted)?;
        while self.nodes[node].bits > bits {
            let child_bits = self.nodes[node].bits - 1;
            let first = match self.reserve_slot() {
                Ok(slot) => slot,
                Err(error) => {
                    self.release(node)?;
                    return Err(error);
                }
            };
            let second = match self.reserve_slot() {
                Ok(slot) => slot,
                Err(error) => {
                    self.free_slot(first)?;
                    self.release(node)?;
                    return Err(error);
                }
            };
            let blueprint = ObjectBlueprint::Untyped {
                size_bits: child_bits,
            };
            let result = [first, second].into_iter().try_for_each(|slot| {
                self.nodes[node].cap.untyped_retype(
                    &blueprint,
                    &self.root.absolute_cptr_for_self(),
                    slot,
                    1,
                )
            });
            if result.is_err() {
                if self.revoke(node).is_err() {
                    self.nodes[node].state = State::Quarantined;
                    return Err(Error::Backend);
                }
                self.free_slot(first)?;
                self.free_slot(second)?;
                self.release(node)?;
                return Err(Error::Exhausted);
            }
            let children = self.nodes[node].children.unwrap_or_else(|| {
                let start = self.nodes.len();
                for slot in [first, second] {
                    self.nodes.push(Node {
                        cap: Cap::from_bits(slot as Word),
                        bits: child_bits,
                        parent: Some(node),
                        children: None,
                        state: State::Free,
                        owner: None,
                    });
                }
                [start, start + 1]
            });
            for (index, slot) in children.into_iter().zip([first, second]) {
                self.nodes[index].cap = Cap::from_bits(slot as Word);
                self.nodes[index].state = State::Free;
            }
            self.nodes[node].children = Some(children);
            self.nodes[node].state = State::Split;
            node = children[0];
        }
        self.nodes[node].state = State::Used;
        Ok(node)
    }
    fn release(&mut self, mut node: usize) -> Result<(), Error> {
        self.revoke(node)?;
        self.nodes[node].state = State::Free;
        self.nodes[node].owner = None;
        while let Some(parent) = self.nodes[node].parent {
            let children = self.nodes[parent].children.unwrap();
            if !children.iter().all(|&i| self.nodes[i].state == State::Free) {
                break;
            }
            // Coalescing revokes the two child Untypeds before recycling their slots.
            if self.revoke(parent).is_err() {
                // A failed revoke gives no assurance that child caps survived.
                self.nodes[parent].state = State::Quarantined;
                for child in children {
                    self.nodes[child].state = State::Dormant;
                }
                return Err(Error::Backend);
            }
            self.nodes[parent].state = State::Quarantined;
            for child in children {
                let slot = self.nodes[child].cap.bits() as usize;
                self.nodes[child].state = State::Dormant;
                self.free_slot(slot)?;
            }
            self.nodes[parent].state = State::Free;
            node = parent;
        }
        Ok(())
    }
}

impl KernelAllocator {
    pub fn new(
        root: cap::CNode,
        untyped: cap::Untyped,
        size_bits: usize,
        slots: Range<usize>,
    ) -> Self {
        Self {
            account: Account::new(1usize << size_bits),
            inner: Rc::new(RefCell::new(Allocator {
                root,
                first_slot: slots.start,
                slots: ByRange::new(BasicAllocator::with_granule_size(slots.len(), 1)),
                nodes: alloc::vec![Node {
                    cap: untyped,
                    bits: size_bits,
                    parent: None,
                    children: None,
                    state: State::Free,
                    owner: None
                }],
                objects: BTreeMap::new(),
            })),
        }
    }
    /// Add another, previously unused non-device BootInfo Untyped.
    pub fn add_untyped(&mut self, cap: cap::Untyped, bits: usize) {
        self.account
            .set_limit(self.account.usage().limit + (1usize << bits))
            .expect("RAM capacity");
        self.inner.borrow_mut().nodes.push(Node {
            cap,
            bits,
            parent: None,
            children: None,
            state: State::Free,
            owner: None,
        });
    }
    pub fn for_account(&self, account: Account) -> Self {
        Self {
            inner: self.inner.clone(),
            account,
        }
    }
    pub fn account(&self) -> &Account {
        &self.account
    }
    pub fn root(&self) -> cap::CNode {
        self.inner.borrow().root
    }
    pub fn reserve_slot(&mut self) -> Result<usize, Error> {
        self.inner.borrow_mut().reserve_slot()
    }
    pub fn clear_slot(&self, slot: usize) -> Result<(), Error> {
        self.inner.borrow().clear_slot(slot)
    }
    pub fn release_slot(&mut self, slot: usize) -> Result<(), Error> {
        let mut inner = self.inner.borrow_mut();
        if inner
            .objects
            .get(&slot)
            .is_some_and(|lease| inner.nodes[lease.node].state == State::Quarantined)
        {
            return Err(Error::Backend);
        }
        if let Some(lease) = inner.objects.remove(&slot) {
            let bytes = 1usize << inner.nodes[lease.node].bits;
            if inner.release(lease.node).is_err() {
                inner.nodes[lease.node].state = State::Quarantined;
                lease.account.quarantine(bytes);
                inner.objects.insert(slot, lease);
                return Err(Error::Backend);
            }
            lease.account.release(bytes);
        }
        inner.free_slot(slot)
    }
    pub fn delegate(&mut self, bits: usize) -> Result<UntypedLease, Error> {
        if bits < MIN_BITS || bits >= usize::BITS as usize {
            return Err(Error::InvalidSize);
        }
        let bytes = 1usize << bits;
        self.account.charge(bytes)?;
        let mut inner = self.inner.borrow_mut();
        let node = match inner.acquire(bits) {
            Ok(node) => node,
            Err(error) => {
                if error == Error::Backend {
                    self.account.quarantine(bytes);
                } else {
                    self.account.release(bytes);
                }
                return Err(error);
            }
        };
        inner.nodes[node].owner = Some(self.account.clone());
        Ok(UntypedLease {
            node,
            cap: inner.nodes[node].cap,
            bits,
            account: self.account.clone(),
            allocator: self.inner.clone(),
        })
    }
    pub fn reclaim(&mut self, lease: UntypedLease) -> Result<(), Error> {
        if !Rc::ptr_eq(&self.inner, &lease.allocator) {
            return Err(Error::Backend);
        }
        let mut inner = self.inner.borrow_mut();
        // A lease may only be returned to its original allocator.
        if inner.nodes.get(lease.node).is_none_or(|n| {
            n.cap.bits() != lease.cap.bits()
                || n.state != State::Used
                || n.owner.as_ref().is_none_or(|a| !a.same(&lease.account))
        }) {
            return Err(Error::Backend);
        }
        if inner.release(lease.node).is_err() {
            inner.nodes[lease.node].state = State::Quarantined;
            lease.account.quarantine(1usize << lease.bits);
            return Err(Error::Backend);
        }
        lease.account.release(1usize << lease.bits);
        Ok(())
    }
    pub fn allocate(&mut self, blueprint: ObjectBlueprint) -> Result<cap::Unspecified, Error> {
        // Dedicated revocable units: small kernel objects are charged a full page.
        let bits = blueprint.physical_size_bits().max(MIN_BITS);
        let slot = self.reserve_slot()?;
        let lease = match self.delegate(bits) {
            Ok(lease) => lease,
            Err(error) => {
                self.release_slot(slot)?;
                return Err(error);
            }
        };
        let root = self.root();
        if lease
            .cap
            .untyped_retype(&blueprint, &root.absolute_cptr_for_self(), slot, 1)
            .is_err()
        {
            self.reclaim(lease)?;
            self.release_slot(slot)?;
            return Err(Error::Backend);
        }
        self.inner.borrow_mut().objects.insert(
            slot,
            Lease {
                node: lease.node,
                account: lease.account,
            },
        );
        Ok(Cap::from_bits(slot as Word))
    }
    /// Called only after stopping every thread and clearing VM bookkeeping.
    pub fn destroy_account(&mut self) -> Result<(), Error> {
        self.account.close();
        let slots: Vec<_> = self
            .inner
            .borrow()
            .objects
            .iter()
            .filter(|(_, lease)| lease.account.same(&self.account))
            .map(|(&slot, _)| slot)
            .collect();
        let mut failed = false;
        for slot in slots {
            failed |= self.release_slot(slot).is_err();
        }
        let mut inner = self.inner.borrow_mut();
        let nodes: Vec<_> = inner
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.owner
                    .as_ref()
                    .is_some_and(|owner| owner.same(&self.account))
            })
            .map(|(i, _)| i)
            .collect();
        for node in nodes {
            if inner.nodes[node].state == State::Quarantined {
                failed = true;
                continue;
            }
            let bytes = 1usize << inner.nodes[node].bits;
            if inner.release(node).is_err() {
                inner.nodes[node].state = State::Quarantined;
                self.account.quarantine(bytes);
                failed = true;
            } else {
                self.account.release(bytes);
            }
        }
        if failed || self.account.usage().charged != 0 {
            Err(Error::Backend)
        } else {
            Ok(())
        }
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

#[must_use]
pub struct Frame {
    cap: cap::Granule,
    allocator: Rc<RefCell<Allocator>>,
}
pub struct FramePool {
    allocator: KernelAllocator,
}
impl FramePool {
    /// Compatibility constructor: a quota, not a preallocated pool.
    pub fn new(allocator: &mut KernelAllocator, count: usize) -> Result<Self, Error> {
        let bytes = count
            .checked_mul(PAGE_SIZE)
            .filter(|&n| n != 0)
            .ok_or(Error::InvalidSize)?;
        Ok(Self::for_account(
            allocator.for_account(allocator.account.child(bytes)?),
        ))
    }
    pub fn for_account(allocator: KernelAllocator) -> Self {
        Self { allocator }
    }
    pub fn capacity(&self) -> usize {
        self.allocator.account.usage().limit / PAGE_SIZE
    }
    pub fn available(&self) -> usize {
        let usage = self.allocator.account.usage();
        (usage.limit - usage.charged) / PAGE_SIZE
    }
    pub fn quarantined_bytes(&self) -> usize {
        self.allocator.account.usage().quarantined
    }
    pub fn cap(&self, frame: &Frame) -> Result<cap::Granule, Error> {
        if !Rc::ptr_eq(&self.allocator.inner, &frame.allocator) {
            return Err(Error::Backend);
        }
        let inner = self.allocator.inner.borrow();
        let lease = inner
            .objects
            .get(&(frame.cap.bits() as usize))
            .ok_or(Error::Backend)?;
        if !lease.account.same(&self.allocator.account)
            || inner.nodes[lease.node].state != State::Used
        {
            return Err(Error::Backend);
        }
        Ok(frame.cap)
    }
    pub fn account(&self) -> &Account {
        &self.allocator.account
    }
}
impl Frames for FramePool {
    type Frame = Frame;
    fn page_size(&self) -> usize {
        PAGE_SIZE
    }
    fn allocate_zeroed(&mut self) -> Result<Frame, Error> {
        self.allocator
            .allocate_fixed_sized::<cap_type::Granule>()
            .map(|cap| Frame {
                cap,
                allocator: self.allocator.inner.clone(),
            })
    }
    fn release(&mut self, frame: Frame) -> Result<(), Error> {
        let cap = self.cap(&frame)?;
        self.allocator.release_slot(cap.bits() as usize)
    }
}
