//! Shared device-Untyped tree. Temporary hardware access and retained MMIO
//! backing must use the SAME bank: revoking a temporary source wholesale would
//! otherwise silently destroy another process's BAR mappings.
use super::*;
use alloc::{rc::Rc, vec::Vec};
use core::{cell::RefCell, ops::Range};

#[derive(Clone, Copy)]
struct Node {
    address: usize,
    bits: usize,
    cap: cap::Untyped,
    parent: Option<usize>,
    children: Option<[usize; 2]>,
    frame: Option<(cap::Granule, usize)>,
}
struct Inner {
    root: cap::CNode,
    capacity: usize,
    peak: usize,
    roots: Vec<usize>,
    nodes: Vec<Option<Node>>,
    free: Vec<usize>,
    poisoned: bool,
}
/// Exclusive authority to the supplied device Untypeds and CSlot interval.
#[derive(Clone)]
pub struct DeviceBank {
    inner: Rc<RefCell<Inner>>,
}
/// Explicitly released provider token. Drop intentionally leaks its reference;
/// uncertain VM cleanup must never free/retype an MMIO frame behind a CPU alias.
#[must_use]
pub struct DevicePage {
    inner: Rc<RefCell<Inner>>,
    node: usize,
    cap: cap::Granule,
    bits: usize,
}
impl DeviceBank {
    /// # Safety
    /// All supplied device Untypeds must be unused and exclusive to this bank.
    /// `slots` must be empty and excluded from every other allocator, including
    /// after the bank is dropped. All CPU maps must be acknowledged unmapped
    /// before a DevicePage is explicitly released.
    pub unsafe fn new(
        root: cap::CNode,
        sources: impl IntoIterator<Item = (usize, usize, cap::Untyped)>,
        slots: Range<usize>,
    ) -> Result<Self, Error> {
        let mut nodes = Vec::new();
        let mut roots = Vec::new();
        for (address, bits, cap) in sources {
            // seL4 aligns Untypeds in kernel-pointer space, NOT necessarily
            // physical space (the x86 direct-map offset wraps at 512GiB).
            if bits < 12
                || bits >= usize::BITS as usize
                || address & (PAGE_SIZE - 1) != 0
                || address.checked_add(1usize << bits).is_none()
            {
                return Err(Error::InvalidSize);
            }
            if nodes.iter().flatten().any(|n: &Node| {
                address < n.address + (1usize << n.bits) && n.address < address + (1usize << bits)
            }) {
                return Err(Error::InvalidSize);
            }
            roots.push(nodes.len());
            nodes.push(Some(Node {
                address,
                bits,
                cap,
                parent: None,
                children: None,
                frame: None,
            }));
        }
        let capacity = slots.len();
        let free = slots.rev().collect();
        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                root,
                capacity,
                peak: 0,
                roots,
                nodes,
                free,
                poisoned: false,
            })),
        })
    }
    pub fn page(&mut self, address: usize) -> Result<DevicePage, Error> {
        self.frame(address, 12)
    }
    /// Optional large-frame optimization for temporary bootstrap access. Fails
    /// without changing live claims if a smaller retained BAR page overlaps it.
    pub fn large_page(&mut self, address: usize) -> Result<DevicePage, Error> {
        self.frame(address, 21)
    }
    fn frame(&mut self, address: usize, bits: usize) -> Result<DevicePage, Error> {
        if address & ((1usize << bits) - 1) != 0 {
            return Err(Error::InvalidSize);
        }
        let mut inner = self.inner.borrow_mut();
        if inner.poisoned {
            return Err(Error::Backend);
        }
        let mut at = inner
            .roots
            .iter()
            .copied()
            .find(|i| {
                let n = inner.nodes[*i].unwrap();
                address >= n.address && address - n.address < 1usize << n.bits
            })
            .ok_or(Error::InvalidSize)?;
        // Ordinary capacity/metadata failures do not poison the tree. Partial
        // kernel operations do: retain every uncertain slot until reboot.
        if inner.nodes[at].unwrap().bits < bits {
            return Err(Error::InvalidSize);
        }
        while inner.nodes[at].unwrap().bits > bits {
            let node = inner.nodes[at].unwrap();
            if node.frame.is_some() {
                inner.report(address, bits, "claimed");
                return Err(Error::Exhausted);
            }
            let children = if let Some(children) = node.children {
                children
            } else {
                if inner.free.len() < 2 {
                    inner.prune(at)?;
                    return Err(Error::Exhausted);
                }
                if inner.nodes.try_reserve(2).is_err() {
                    inner.prune(at)?;
                    return Err(Error::MetadataExhausted);
                }
                let slots = [inner.free.pop().unwrap(), inner.free.pop().unwrap()];
                inner.peak = inner.peak.max(inner.capacity - inner.free.len());
                let mut ids = [0; 2];
                for n in 0..2 {
                    if node
                        .cap
                        .untyped_retype(
                            &sel4::ObjectBlueprint::Untyped {
                                size_bits: node.bits - 1,
                            },
                            &inner.root.absolute_cptr_for_self(),
                            slots[n],
                            1,
                        )
                        .is_err()
                    {
                        inner.poisoned = true;
                        return Err(Error::Backend);
                    }
                    let child = Node {
                        address: node.address + (n << (node.bits - 1)),
                        bits: node.bits - 1,
                        cap: cap::Untyped::from_bits(slots[n] as _),
                        parent: Some(at),
                        children: None,
                        frame: None,
                    };
                    ids[n] = if let Some(index) = inner.nodes.iter().position(Option::is_none) {
                        inner.nodes[index] = Some(child);
                        index
                    } else {
                        inner.nodes.push(Some(child));
                        inner.nodes.len() - 1
                    };
                }
                inner.nodes[at].as_mut().unwrap().children = Some(ids);
                ids
            };
            at = children[usize::from(address >= node.address + (1usize << (node.bits - 1)))];
        }
        let node = inner.nodes[at].unwrap();
        if node.children.is_some() {
            inner.report(address, bits, "split");
            return Err(Error::Exhausted);
        }
        let cap = if let Some((cap, references)) = node.frame {
            inner.nodes[at].as_mut().unwrap().frame =
                Some((cap, references.checked_add(1).ok_or(Error::Exhausted)?));
            cap
        } else {
            let Some(slot) = inner.free.pop() else {
                inner.report(address, bits, "slots");
                inner.prune(at)?;
                return Err(Error::Exhausted);
            };
            inner.peak = inner.peak.max(inner.capacity - inner.free.len());
            let blueprint = if bits == 21 {
                cap_type::LargePage::object_blueprint()
            } else {
                cap_type::Granule::object_blueprint()
            };
            if node
                .cap
                .untyped_retype(&blueprint, &inner.root.absolute_cptr_for_self(), slot, 1)
                .is_err()
            {
                inner.poisoned = true;
                return Err(Error::Backend);
            }
            let cap = cap::Granule::from_bits(slot as _);
            inner.nodes[at].as_mut().unwrap().frame = Some((cap, 1));
            cap
        };
        Ok(DevicePage {
            inner: self.inner.clone(),
            node: at,
            cap,
            bits,
        })
    }
    pub fn memory(&mut self, address: usize, length: usize) -> Result<MemoryObject<Frame>, Error> {
        if length == 0 || length & (PAGE_SIZE - 1) != 0 || address.checked_add(length).is_none() {
            return Err(Error::InvalidSize);
        }
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(length / PAGE_SIZE)
            .map_err(|_| Error::MetadataExhausted)?;
        for offset in (0..length).step_by(PAGE_SIZE) {
            match self.page(address + offset) {
                Ok(page) => frames.push(Frame {
                    kind: FrameKind::Device(page),
                }),
                Err(error) => {
                    let mut failed = false;
                    for frame in frames {
                        if let FrameKind::Device(page) = frame.kind {
                            failed |= page.release().is_err();
                        }
                    }
                    return Err(if failed { Error::Backend } else { error });
                }
            }
        }
        Ok(MemoryObject {
            size: length as u64,
            resident_size: 0,
            frames,
        })
    }
    pub fn poisoned(&self) -> bool {
        self.inner.borrow().poisoned
    }
    pub fn available_slots(&self) -> usize {
        self.inner.borrow().free.len()
    }
    pub fn peak_slots(&self) -> usize {
        self.inner.borrow().peak
    }
}
impl DevicePage {
    pub fn cap(&self) -> cap::Granule {
        self.cap
    }
    pub fn size_bits(&self) -> usize {
        self.bits
    }
    pub fn release(self) -> Result<(), Error> {
        let mut inner = self.inner.borrow_mut();
        if inner.poisoned {
            return Err(Error::Backend);
        }
        let node = inner.nodes[self.node].as_mut().unwrap();
        let references = &mut node.frame.as_mut().unwrap().1;
        assert!(*references != 0);
        *references -= 1;
        if *references != 0 {
            return Ok(());
        }
        let node = *node;
        if inner.root.absolute_cptr(node.cap.cptr()).revoke().is_err() {
            inner.poisoned = true;
            return Err(Error::Backend);
        }
        inner.free.push(self.cap.bits() as usize);
        inner.nodes[self.node].as_mut().unwrap().frame = None;
        inner.prune(self.node)
    }
}
impl Inner {
    fn report(&self, address: usize, bits: usize, reason: &str) {
        ::sel4::debug_println!(
            "DEVICE_BANK_EXHAUSTED reason={} address={:#x} bits={} capacity={} free={} peak={} nodes={}",
            reason,
            address,
            bits,
            self.capacity,
            self.free.len(),
            self.peak,
            self.nodes.len()
        );
    }
    fn prune(&mut self, mut at: usize) -> Result<(), Error> {
        while let Some(parent) = self.nodes[at].unwrap().parent {
            let children = self.nodes[parent].unwrap().children.unwrap();
            if children.iter().any(|c| {
                let node = self.nodes[*c].unwrap();
                node.children.is_some() || node.frame.is_some()
            }) {
                break;
            }
            let node = self.nodes[parent].unwrap();
            if self.root.absolute_cptr(node.cap.cptr()).revoke().is_err() {
                self.poisoned = true;
                return Err(Error::Backend);
            }
            for child in children {
                let child = self.nodes[child].take().unwrap();
                self.free.push(child.cap.bits() as usize);
            }
            self.nodes[parent].as_mut().unwrap().children = None;
            at = parent;
        }
        Ok(())
    }
}
