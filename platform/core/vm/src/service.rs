//! Root-hosted VM. Mapping authority stays here; private Untyped may be delegated.
use alloc::{rc::Rc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};
use horn_ipc::*;

use crate::{
    Error, MemoryObject,
    protocol::*,
    sel4::{Frame, FramePool, KernelAllocator, PAGE_SIZE, UntypedLease},
};

const FACTORY: usize = 0;
const SPACE: usize = 1;
// Never recycle a badge when a VM instance is destroyed. Root process lifecycle
// uses the upper namespace on the shared endpoint.
static NEXT_BADGE: AtomicU64 = AtomicU64::new(1);
fn new_badge() -> Result<::sel4::Word, VmError> {
    NEXT_BADGE
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1).filter(|&next| next < (1 << 32))
        })
        .map_err(|_| VmError::Exhausted)
}

fn error(value: Error) -> VmError {
    match value {
        Error::InvalidSize => VmError::Invalid,
        Error::QuotaExceeded => VmError::QuotaExceeded,
        Error::Exhausted | Error::MetadataExhausted => VmError::Exhausted,
        Error::Backend => VmError::Quarantined,
    }
}

fn mint(
    allocator: &mut KernelAllocator,
    endpoint: ::sel4::cap::Endpoint,
    badge: ::sel4::Word,
) -> Result<usize, VmError> {
    let slot = allocator.reserve_slot().map_err(error)?;
    let root = allocator.root();
    if root
        .absolute_cptr(::sel4::CPtr::from_bits(slot as _))
        .mint(
            &root.absolute_cptr(::sel4::CPtr::from_bits(endpoint.bits())),
            ::sel4::CapRights::new(true, true, false, true),
            badge,
        )
        .is_err()
    {
        allocator.release_slot(slot).map_err(error)?;
        return Err(VmError::Backend);
    }
    Ok(slot)
}

fn provision(
    allocator: &mut KernelAllocator,
    endpoint: ::sel4::cap::Endpoint,
    badges: [::sel4::Word; 2],
) -> Result<(::sel4::cap::Endpoint, ::sel4::cap::Endpoint), VmError> {
    let factory = mint(allocator, endpoint, badges[FACTORY])?;
    let space = match mint(allocator, endpoint, badges[SPACE]) {
        Ok(slot) => slot,
        Err(value) => {
            allocator.release_slot(factory).map_err(error)?;
            return Err(value);
        }
    };
    Ok((
        ::sel4::cap::Endpoint::from_bits(factory as _),
        ::sel4::cap::Endpoint::from_bits(space as _),
    ))
}

const HANDLE_DEPTH: usize = 32;
const HANDLE_CONTROL: u32 = VmMemoryRights::DELEGATE.0
    | VmMemoryRights::MAKE_IMMUTABLE.0
    | VmMemoryRights::REVOKE.0
    | VmMemoryRights::DMA.0
    | VmMemoryRights::CLOSE.0;
struct DerivedHandle {
    memory: usize,
    badge: ::sel4::Word,
    authority: usize,
    rights: VmMemoryRights,
    lineage: Rc<Lineage>,
}

struct Lineage {
    ancestors: [::sel4::Word; HANDLE_DEPTH],
    depth: usize,
}

struct MemoryHandle {
    badge: ::sel4::Word,
    authority: Option<usize>,
    ceiling: VmProtection,
}

struct Memory {
    owner: ::sel4::Word,
    allocator: KernelAllocator,
    handle: MemoryHandle,
    backing: MemoryObject<Frame>,
    // Frames must always be resolved and released through their allocating account.
    pool: FramePool,
    immutable: bool,
    references: usize,
    dma: crate::dma::Pins,
    mmio_lease: bool,
    poisoned: bool,
}

#[derive(Clone, Default)]
struct Page {
    slot: Option<usize>,
    memory: Option<usize>,
    handle: ::sel4::Word,
    ceiling: VmProtection,
    lineage: Option<Rc<Lineage>>,
    // Like VM_MAYWRITE: survives protect(READ), but not Unmap then Map.
    may_write: bool,
    mapped: bool,
    protection: VmProtection,
    previous: VmProtection,
    fresh: bool,
}

struct Region {
    owner: ::sel4::Word,
    allocator: KernelAllocator,
    vspace: ::sel4::cap::VSpace,
    address: usize,
    badge: ::sel4::Word,
    authority: usize,
    range: Range<usize>,
    ceiling: VmProtection,
    pages: Vec<Page>,
    poisoned: bool,
}

struct Delegation {
    badge: ::sel4::Word,
    authority: usize,
    lease: Option<UntypedLease>,
}

/// Root-held lifetime/control token for a device-backed memory object.
/// Drop retains the backing; it never silently revokes CPU mappings.
#[must_use]
pub struct MmioLease {
    memory: usize,
    badge: u64,
}

fn resolve_handle(
    memories: &[Option<Memory>],
    handles: &[Option<DerivedHandle>],
    badge: ::sel4::Word,
) -> Result<(usize, VmMemoryRights), VmError> {
    if let Some(index) = memories.iter().position(|m| {
        m.as_ref()
            .is_some_and(|m| m.handle.badge == badge && m.handle.authority.is_some())
    }) {
        let memory = memories[index].as_ref().unwrap();
        return Ok((
            index,
            VmMemoryRights(memory.handle.ceiling.0 | HANDLE_CONTROL),
        ));
    }
    handles
        .iter()
        .flatten()
        .find(|h| h.badge == badge)
        .map(|h| (h.memory, h.rights))
        .ok_or(VmError::Stale)
}

fn collect_backing(memories: &mut [Option<Memory>], index: usize) -> Result<(), VmError> {
    let memory = memories[index].as_ref().unwrap();
    if memory.poisoned {
        return Err(VmError::Quarantined);
    }
    if memory.references == 0 && memory.handle.authority.is_none() {
        let mut memory = memories[index].take().unwrap();
        memory
            .backing
            .release(&mut memory.pool)
            .map_err(|_| VmError::Quarantined)?;
    }
    Ok(())
}

/// Root-owned tables, exclusively loaned to one serialized VM operation at a time.
#[derive(Default)]
pub struct Registry {
    handles: Vec<Option<DerivedHandle>>,
    memories: Vec<Option<Memory>>,
    regions: Vec<Option<Region>>,
}
impl Registry {
    /// Trusted IOMMU broker only. The reference must carry a kernel-unwrapped
    /// memory badge; device access is bounded to whole resident pages (at most 16).
    /// This adds a backing reference, not a RAM charge or a raw capability export.
    pub fn pin_dma(
        &mut self,
        cap: CapRef,
        offset: u64,
        length: u64,
        access: VmProtection,
    ) -> Result<crate::dma::Pin, VmError> {
        let badge = cap.unwrapped_badge().ok_or(VmError::Denied)? as _;
        let (index, rights) = resolve_handle(&self.memories, &self.handles, badge)?;
        if rights.0 & VmMemoryRights::DMA.0 == 0 {
            return Err(VmError::Denied);
        }
        let ceiling = VmProtection(rights.0 & 7);
        let memory = self.memories[index].as_ref().unwrap();
        if memory.poisoned {
            return Err(VmError::Quarantined);
        }
        if memory.backing.resident_size() == 0 {
            return Err(VmError::Denied);
        }
        let range = crate::page_range(offset, length, memory.backing.size(), PAGE_SIZE as u64)
            .map_err(error)?;
        if range.len() > 16 {
            return Err(VmError::Invalid);
        }
        let pins = memory.dma.adding(access, ceiling, memory.immutable)?;
        let references = memory.references.checked_add(1).ok_or(VmError::Exhausted)?;
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(range.len())
            .map_err(|_| VmError::Exhausted)?;
        for frame in &memory.backing.frames()[range] {
            pages.push(
                memory
                    .pool
                    .cap(frame)
                    .map_err(error)?
                    .frame_get_address()
                    .map_err(|_| VmError::Backend)? as u64,
            );
        }
        let pin = crate::dma::Pin {
            memory: index,
            badge: memory.handle.badge as u64,
            writer: access.0 & VmProtection::WRITE.0 != 0,
            pages,
        };
        let memory = self.memories[index].as_mut().unwrap();
        memory.dma = pins;
        memory.references = references;
        Ok(pin)
    }
    /// # Safety
    /// Every device translation for this token must already be removed with
    /// acknowledged IOTLB invalidation AND read/write draining. No IOVA reuse
    /// may expose a new allocation to stale device requests.
    pub unsafe fn unpin_dma(&mut self, pin: crate::dma::Pin) -> Result<(), VmError> {
        let memory = self
            .memories
            .get_mut(pin.memory)
            .and_then(Option::as_mut)
            .ok_or(VmError::Stale)?;
        if memory.handle.badge as u64 != pin.badge {
            return Err(VmError::Stale);
        }
        memory.dma.remove(pin.writer);
        memory.references -= 1;
        collect_backing(&mut self.memories, pin.memory)
    }

    /// Attach before creating regions, delegations, or externally retained tokens.
    /// Unmapped boot-published backing can be adopted: its only public ID is a badge.
    pub fn register(&mut self, service: &mut Service) -> Result<(), VmError> {
        if service.memories.iter().flatten().any(|m| m.references != 0)
            || service.regions.iter().any(Option::is_some)
            || service.derived.iter().any(Option::is_some)
        {
            return Err(VmError::Busy);
        }
        let count = service.max_objects;
        self.handles
            .try_reserve(count)
            .map_err(|_| VmError::Exhausted)?;
        self.memories
            .try_reserve(count)
            .map_err(|_| VmError::Exhausted)?;
        self.regions
            .try_reserve(count)
            .map_err(|_| VmError::Exhausted)?;
        self.memories.append(&mut service.memories);
        self.regions
            .resize_with(self.regions.len() + count, || None);
        self.handles
            .resize_with(self.handles.len() + count, || None);
        service.derived = Vec::new();
        service.memories = Vec::new();
        service.regions = Vec::new();
        Ok(())
    }
    pub fn owner(&self, badge: ::sel4::Word) -> Option<::sel4::Word> {
        self.memories
            .iter()
            .flatten()
            .find(|m| m.handle.badge == badge && m.handle.authority.is_some())
            .map(|m| m.owner)
            .or_else(|| {
                self.handles
                    .iter()
                    .flatten()
                    .find(|h| h.badge == badge)
                    .and_then(|h| self.memories[h.memory].as_ref().map(|m| m.owner))
            })
            .or_else(|| {
                self.regions
                    .iter()
                    .flatten()
                    .find(|r| r.badge == badge)
                    .map(|r| r.owner)
            })
    }
}

pub struct Service {
    allocator: KernelAllocator,
    pool: FramePool,
    endpoint: ::sel4::cap::Endpoint,
    authority: [Option<usize>; 2],
    vspace: ::sel4::cap::VSpace,
    window: Range<usize>,
    memories: Vec<Option<Memory>>,
    regions: Vec<Option<Region>>,
    delegations: Vec<Option<Delegation>>,
    badges: [::sel4::Word; 2],
    outgoing: Option<usize>,
    max_objects: usize,
    derived: Vec<Option<DerivedHandle>>,
}

impl Service {
    /// The loader must reserve this window and install its translation tables.
    /// The adopted VSpace remains owned by the process service; destroy is Busy.
    pub fn new(
        mut allocator: KernelAllocator,
        pool: FramePool,
        endpoint: ::sel4::cap::Endpoint,
        vspace: ::sel4::cap::VSpace,
        window: Range<usize>,
        max_objects: usize,
    ) -> Result<Self, VmError> {
        if window.start >= window.end
            || window.start % PAGE_SIZE != 0
            || window.end % PAGE_SIZE != 0
        {
            return Err(VmError::Invalid);
        }
        let mut memories = Vec::new();
        memories
            .try_reserve_exact(max_objects)
            .map_err(|_| VmError::Exhausted)?;
        memories.resize_with(max_objects, || None);
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(max_objects)
            .map_err(|_| VmError::Exhausted)?;
        regions.resize_with(max_objects, || None);
        let mut delegations = Vec::new();
        delegations.resize_with(max_objects, || None);
        let mut derived = Vec::new();
        derived
            .try_reserve_exact(max_objects)
            .map_err(|_| VmError::Exhausted)?;
        derived.resize_with(max_objects, || None);
        let badges = [new_badge()?, new_badge()?];
        let (factory, space) = provision(&mut allocator, endpoint, badges)?;
        Ok(Self {
            authority: [Some(factory.bits() as usize), Some(space.bits() as usize)],
            allocator,
            pool,
            endpoint,
            vspace,
            window,
            memories,
            regions,
            delegations,
            badges,
            outgoing: None,
            max_objects,
            derived,
        })
    }

    /// Exclusively loan the manager's tables for one serialized operation.
    /// Panics abort root; cleanup failures remain represented in these tables.
    pub fn with_registry<R>(
        &mut self,
        registry: &mut Registry,
        operation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        assert!(self.memories.is_empty() && self.regions.is_empty() && self.derived.is_empty());
        core::mem::swap(&mut self.derived, &mut registry.handles);
        core::mem::swap(&mut self.memories, &mut registry.memories);
        core::mem::swap(&mut self.regions, &mut registry.regions);
        let result = operation(self);
        core::mem::swap(&mut self.derived, &mut registry.handles);
        core::mem::swap(&mut self.memories, &mut registry.memories);
        core::mem::swap(&mut self.regions, &mut registry.regions);
        result
    }
    pub fn routes(&self, badge: ::sel4::Word, registry: &Registry) -> bool {
        self.badges.contains(&badge)
            || self.delegation_index(badge).is_ok()
            || registry.owner(badge) == Some(self.badges[SPACE])
    }
    pub fn owns_registered_memory(&self, cap: CapRef, registry: &Registry) -> bool {
        cap.unwrapped_badge().is_some_and(|badge| {
            registry.owner(badge as _) == Some(self.badges[SPACE])
                && !registry
                    .regions
                    .iter()
                    .flatten()
                    .any(|r| r.badge == badge as _)
        })
    }
    pub fn factory(&self) -> ::sel4::cap::Endpoint {
        ::sel4::Cap::from_bits(self.authority[FACTORY].unwrap() as _)
    }
    pub fn address_space(&self) -> ::sel4::cap::Endpoint {
        ::sel4::Cap::from_bits(self.authority[SPACE].unwrap() as _)
    }
    /// Trusted root routing only; badges must come from the kernel receive path.
    pub fn handles(&self, badge: ::sel4::Word) -> bool {
        self.badges.contains(&badge)
            || self.memory_index(badge).is_ok()
            || self.region_index(badge).is_ok()
            || self.delegation_index(badge).is_ok()
    }

    pub fn owns_memory(&self, cap: CapRef) -> bool {
        cap.unwrapped_badge()
            .is_some_and(|badge| self.memory_index(badge as _).is_ok())
    }

    pub fn owns_address_space(&self, cap: CapRef) -> bool {
        cap.unwrapped_badge() == Some(self.badges[SPACE] as u64)
    }
    /// Trusted device broker only. The bank has already checked physical device
    /// authority. This does not charge MMIO bytes as resident RAM. CPU mapping
    /// objects/page tables still belong to this process's account.
    pub fn import_mmio(
        &mut self,
        backing: MemoryObject<Frame>,
        ceiling: VmProtection,
    ) -> Result<(MmioLease, Cap<VmMemoryObjectTag>), VmError> {
        let result = (|| {
            permissions(ceiling)?;
            if ceiling.0 & VmProtection::EXECUTE.0 != 0
                || backing.resident_size() != 0
                || backing.page_count() == 0
            {
                return Err(VmError::Denied);
            }
            if self
                .memories
                .iter()
                .flatten()
                .filter(|m| m.owner == self.badges[SPACE])
                .count()
                >= self.max_objects
            {
                return Err(VmError::Exhausted);
            }
            let index = self
                .memories
                .iter()
                .position(Option::is_none)
                .ok_or(VmError::Exhausted)?;
            let badge = new_badge()?;
            let (authority, cap) = self.export(badge)?;
            Ok((index, badge, authority, cap))
        })();
        let (index, badge, authority, cap) = match result {
            Ok(value) => value,
            Err(value) => {
                backing.release(&mut self.pool).map_err(error)?;
                return Err(value);
            }
        };
        self.memories[index] = Some(Memory {
            handle: MemoryHandle {
                badge,
                authority: Some(authority),
                ceiling,
            },
            owner: self.badges[SPACE],
            allocator: self.allocator.clone(),
            backing,
            pool: self.pool.clone(),
            immutable: false,
            references: 1,
            dma: crate::dma::Pins::default(),
            mmio_lease: true,
            poisoned: false,
        });
        Ok((
            MmioLease {
                memory: index,
                badge: badge as u64,
            },
            cap,
        ))
    }
    /// Acknowledges removal of EVERY alias, even after the client closed its
    /// memory authority. Only after success may devmgr stop/reset the function
    /// and tear down its DMA domain. On failure retain DMA pins and fail-stop.
    pub fn revoke_mmio(&mut self, lease: MmioLease) -> Result<(), VmError> {
        let memory = self
            .memories
            .get(lease.memory)
            .and_then(Option::as_ref)
            .ok_or(VmError::Stale)?;
        if memory.handle.badge as u64 != lease.badge || !memory.mmio_lease {
            return Err(VmError::Stale);
        }
        self.revoke_tree(lease.memory, lease.badge as _)?;
        let memory = self.memories[lease.memory].as_mut().unwrap();
        memory.mmio_lease = false;
        memory.references -= 1;
        self.collect(lease.memory)
    }
    /// Borrow immutable resident bytes without copying or transferring ownership.
    ///
    /// # Safety
    /// `vspace` must be the current VSpace. `window` must be reserved exclusively
    /// for this call, have translation tables, and not overlap the callback's
    /// scratch mappings. A Quarantined result forbids reusing the window.
    pub unsafe fn with_immutable_bytes<R>(
        &mut self,
        cap: CapRef,
        vspace: ::sel4::cap::VSpace,
        window: Range<usize>,
        read: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, VmError> {
        let badge = cap.unwrapped_badge().ok_or(VmError::Denied)?;
        let index = self.memory_index(badge as _)?;
        let memory = self.memory(index);
        if memory.poisoned {
            return Err(VmError::Quarantined);
        }
        if !memory.immutable {
            return Err(VmError::Denied);
        }
        let size = usize::try_from(memory.backing.size()).map_err(|_| VmError::Invalid)?;
        if window.start >= window.end
            || window.start % PAGE_SIZE != 0
            || window.end % PAGE_SIZE != 0
            || window.len() > isize::MAX as usize
            || memory.backing.resident_size() > window.len() as u64
        {
            return Err(VmError::Invalid);
        }
        let mut mapped = 0;
        let mapping = (|| {
            for frame in memory.backing.frames() {
                memory
                    .pool
                    .cap(frame)
                    .map_err(error)?
                    .frame_map(
                        vspace,
                        window.start + mapped * PAGE_SIZE,
                        ::sel4::CapRights::new(false, false, true, false),
                        ::sel4::VmAttributes::EXECUTE_NEVER,
                    )
                    .map_err(|_| VmError::Backend)?;
                mapped += 1;
            }
            Ok(())
        })();
        let value = mapping.map(|()| {
            // Immutability excludes all managed writers; &mut self pins backing until
            // the callback returns and prevents concurrent close/revoke here.
            read(unsafe { core::slice::from_raw_parts(window.start as *const u8, size) })
        });
        let mut failed = false;
        for frame in &memory.backing.frames()[..mapped] {
            failed |= match memory.pool.cap(frame) {
                Ok(cap) => cap.frame_unmap().is_err(),
                Err(_) => true,
            };
        }
        if failed {
            self.memories[index].as_mut().unwrap().poisoned = true;
            return Err(VmError::Quarantined);
        }
        value
    }

    /// Root dispatcher uses this alongside other generated protocol dispatchers.
    pub fn dispatch(&mut self, request: VmHostRequest, badge: ::sel4::Word) -> VmHostResponse {
        vm_host_dispatch(
            request,
            &mut Caller {
                service: self,
                badge,
            },
        )
    }
    pub fn finish_exports(&mut self) -> Result<(), RpcError> {
        if let Some(slot) = self.outgoing.take() {
            self.allocator
                .release_slot(slot)
                .map_err(|_| RpcError::Transport)?;
        }
        Ok(())
    }
    pub fn finish_reply(&mut self, server: &mut Sel4Server<Sel4CapSpace>) -> Result<(), RpcError> {
        self.finish_exports()?;
        server.recv_slot.delete().map_err(|_| RpcError::Transport)
    }

    /// Initialize boot-provided bytes before publishing immutable backing.
    /// # Safety
    /// `vspace` is the current VSpace, with one exclusively reserved unmapped
    /// scratch page and installed translation tables.
    pub unsafe fn publish(
        &mut self,
        bytes: &[u8],
        vspace: ::sel4::cap::VSpace,
        scratch: usize,
    ) -> Result<Cap<VmMemoryObjectTag>, VmError> {
        let (index, cap) =
            self.allocate(bytes.len() as _, VmBacking::Resident, VmProtection::READ)?;
        for (frame, data) in self
            .memory(index)
            .backing
            .frames()
            .iter()
            .zip(bytes.chunks(PAGE_SIZE))
        {
            let frame = self.memory(index).pool.cap(frame).map_err(error)?;
            if frame
                .frame_map(
                    vspace,
                    scratch,
                    ::sel4::CapRights::read_write(),
                    ::sel4::VmAttributes::EXECUTE_NEVER,
                )
                .is_err()
            {
                self.close(index)?;
                return Err(VmError::Backend);
            }
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), scratch as *mut u8, data.len());
            }
            if frame.frame_unmap().is_err() {
                self.memories[index].as_mut().unwrap().poisoned = true;
                return Err(VmError::Quarantined);
            }
        }
        self.make_immutable(index)?;
        Ok(cap)
    }
    /// All threads using this adopted address space must already be stopped.
    pub fn shutdown(&mut self) -> Result<(), VmError> {
        // Account destruction would revoke RAM even if a backing reference is
        // retained. Refuse BEFORE any teardown while hardware can still use it.
        if self
            .memories
            .iter()
            .flatten()
            .any(|m| m.owner == self.badges[SPACE] && (m.dma.count != 0 || m.mmio_lease))
        {
            return Err(VmError::Busy);
        }
        for index in 0..self.delegations.len() {
            if self.delegations[index].is_some() {
                self.close_delegation(index)?;
            }
        }
        for index in 0..self.regions.len() {
            if self.regions[index]
                .as_ref()
                .is_some_and(|r| r.owner == self.badges[SPACE])
            {
                self.destroy_region(index)?;
            }
        }
        for index in 0..self.memories.len() {
            if self.memories[index]
                .as_ref()
                .is_some_and(|m| m.owner == self.badges[SPACE])
            {
                let badge = self.memory(index).handle.badge;
                self.revoke_tree(index, badge)?;
            }
        }
        if self
            .memories
            .iter()
            .flatten()
            .any(|m| m.owner == self.badges[SPACE])
        {
            return Err(VmError::Quarantined);
        }
        for authority in &mut self.authority {
            if let Some(slot) = *authority {
                self.allocator
                    .root()
                    .absolute_cptr(::sel4::CPtr::from_bits(slot as _))
                    .revoke()
                    .map_err(|_| VmError::Backend)?;
                self.allocator.release_slot(slot).map_err(error)?;
                *authority = None;
            }
        }
        Ok(())
    }
    fn delegation_index(&self, badge: ::sel4::Word) -> Result<usize, VmError> {
        self.delegations
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|d| d.badge == badge))
            .ok_or(VmError::Stale)
    }
    fn close_delegation(&mut self, index: usize) -> Result<(), VmError> {
        let entry = self.delegations[index].as_mut().unwrap();
        let lease = entry.lease.take().ok_or(VmError::Quarantined)?;
        self.allocator.reclaim(lease).map_err(error)?;
        let authority = entry.authority;
        self.allocator
            .root()
            .absolute_cptr(::sel4::CPtr::from_bits(authority as _))
            .revoke()
            .map_err(|_| VmError::Backend)?;
        self.allocator.release_slot(authority).map_err(error)?;
        self.delegations[index] = None;
        Ok(())
    }
    fn export<T: CapType>(&mut self, badge: ::sel4::Word) -> Result<(usize, Cap<T>), VmError> {
        let authority = mint(&mut self.allocator, self.endpoint, badge)?;
        let slot = match self.allocator.reserve_slot() {
            Ok(slot) => slot,
            Err(value) => {
                self.allocator.release_slot(authority).map_err(error)?;
                return Err(error(value));
            }
        };
        let root = self.allocator.root();
        if root
            .absolute_cptr(::sel4::CPtr::from_bits(slot as _))
            .copy(
                &root.absolute_cptr(::sel4::CPtr::from_bits(authority as _)),
                ::sel4::CapRights::all(),
            )
            .is_err()
        {
            self.allocator.release_slot(slot).map_err(error)?;
            self.allocator.release_slot(authority).map_err(error)?;
            return Err(VmError::Backend);
        }
        self.outgoing = Some(slot);
        Ok((
            authority,
            Cap::from_ref(CapRef::new(
                root.bits() as u64,
                slot as u32,
                ::sel4::WORD_SIZE as u8,
                0,
                CapMode::MOVE,
                Rights::ALL,
            )),
        ))
    }

    fn memory_index(&self, badge: ::sel4::Word) -> Result<usize, VmError> {
        resolve_handle(&self.memories, &self.derived, badge).map(|(index, _)| index)
    }
    fn handle_rights(&self, badge: ::sel4::Word) -> Result<VmMemoryRights, VmError> {
        resolve_handle(&self.memories, &self.derived, badge).map(|(_, rights)| rights)
    }
    fn require(&self, badge: ::sel4::Word, right: VmMemoryRights) -> Result<(), VmError> {
        if self.handle_rights(badge)?.0 & right.0 != right.0 {
            return Err(VmError::Denied);
        }
        Ok(())
    }
    fn lineage(&self, badge: ::sel4::Word) -> ([::sel4::Word; HANDLE_DEPTH], usize) {
        self.derived
            .iter()
            .flatten()
            .find(|h| h.badge == badge)
            .map(|h| (h.lineage.ancestors, h.lineage.depth))
            .unwrap_or(([0; HANDLE_DEPTH], 0))
    }
    fn delegate_handle(
        &mut self,
        parent: ::sel4::Word,
        rights: VmMemoryRights,
    ) -> Result<Cap<VmMemoryObjectTag>, VmError> {
        self.require(parent, VmMemoryRights::DELEGATE)?;
        let allowed = self.handle_rights(parent)?;
        if rights.0 & !allowed.0 != 0 || rights.0 & !(7 | HANDLE_CONTROL) != 0 {
            return Err(VmError::Denied);
        }
        permissions(VmProtection(rights.0 & 7))?;
        let memory = self.memory_index(parent)?;
        if self.memory(memory).poisoned {
            return Err(VmError::Quarantined);
        }
        if self
            .derived
            .iter()
            .flatten()
            .filter(|h| self.memory(h.memory).owner == self.badges[SPACE])
            .count()
            >= self.max_objects
        {
            return Err(VmError::Exhausted);
        }
        let slot = self
            .derived
            .iter()
            .position(Option::is_none)
            .ok_or(VmError::Exhausted)?;
        let (mut ancestors, depth) = self.lineage(parent);
        if depth == HANDLE_DEPTH {
            return Err(VmError::Exhausted);
        }
        ancestors[depth] = parent;
        let references = self
            .memory(memory)
            .references
            .checked_add(1)
            .ok_or(VmError::Exhausted)?;
        let badge = new_badge()?;
        let (authority, cap) = self.export(badge)?;
        self.derived[slot] = Some(DerivedHandle {
            memory,
            badge,
            authority,
            rights,
            lineage: Rc::new(Lineage {
                ancestors,
                depth: depth + 1,
            }),
        });
        self.memories[memory].as_mut().unwrap().references = references;
        Ok(cap)
    }
    fn close_handle(&mut self, badge: ::sel4::Word) -> Result<(), VmError> {
        let memory = self.memory_index(badge)?;
        if self.memory(memory).handle.badge == badge {
            return self.close(memory);
        }
        let index = self
            .derived
            .iter()
            .position(|h| h.as_ref().is_some_and(|h| h.badge == badge))
            .ok_or(VmError::Stale)?;
        let authority = self.derived[index].as_ref().unwrap().authority;
        let mut allocator = self.memory(memory).allocator.clone();
        allocator
            .root()
            .absolute_cptr(::sel4::CPtr::from_bits(authority as _))
            .revoke()
            .map_err(|_| VmError::Backend)?;
        allocator.release_slot(authority).map_err(error)?;
        self.derived[index] = None;
        self.memories[memory].as_mut().unwrap().references -= 1;
        self.collect(memory)
    }
    /// Revoke a subtree and only its mappings. The temporary reference prevents
    /// collection while handles and page references are removed in either order.
    fn revoke_tree(&mut self, memory: usize, badge: ::sel4::Word) -> Result<(), VmError> {
        if self.memory(memory).dma.count != 0 {
            return Err(VmError::Busy);
        }
        self.memories[memory].as_mut().unwrap().references = self
            .memory(memory)
            .references
            .checked_add(1)
            .ok_or(VmError::Exhausted)?;
        let result = (|| {
            for region in 0..self.regions.len() {
                if self.regions[region].is_none() {
                    continue;
                }
                for page in 0..self.region(region).pages.len() {
                    let state = self.page(region, page);
                    if state.memory == Some(memory)
                        && (state.handle == badge
                            || state
                                .lineage
                                .as_ref()
                                .is_some_and(|l| l.ancestors[..l.depth].contains(&badge)))
                    {
                        if self.unmap(region, page..page + 1).is_err() {
                            return Err(self.poison(region));
                        }
                    }
                }
            }
            for index in 0..self.derived.len() {
                let target = self.derived[index]
                    .as_ref()
                    .filter(|h| {
                        h.memory == memory
                            && (h.badge == badge
                                || h.lineage.ancestors[..h.lineage.depth].contains(&badge))
                    })
                    .map(|h| h.badge);
                if let Some(target) = target {
                    self.close_handle(target)?;
                }
            }
            if self.memory(memory).handle.badge == badge
                && self.memory(memory).handle.authority.is_some()
            {
                self.close(memory)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.memories[memory].as_mut().unwrap().poisoned = true;
        }
        self.memories[memory].as_mut().unwrap().references -= 1;
        result?;
        self.collect(memory)
    }
    fn region_index(&self, badge: ::sel4::Word) -> Result<usize, VmError> {
        self.regions
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|r| r.badge == badge))
            .ok_or(VmError::Stale)
    }
    fn memory(&self, index: usize) -> &Memory {
        self.memories[index].as_ref().unwrap()
    }
    fn region(&self, index: usize) -> &Region {
        self.regions[index].as_ref().unwrap()
    }
    fn page(&self, region: usize, page: usize) -> Page {
        self.region(region).pages[page].clone()
    }
    fn page_mut(&mut self, region: usize, page: usize) -> &mut Page {
        &mut self.regions[region].as_mut().unwrap().pages[page]
    }
    fn poison(&mut self, region: usize) -> VmError {
        self.regions[region].as_mut().unwrap().poisoned = true;
        for page in 0..self.region(region).pages.len() {
            if let Some(memory) = self.page(region, page).memory {
                self.memories[memory].as_mut().unwrap().poisoned = true;
            }
        }
        VmError::Quarantined
    }

    fn allocate(
        &mut self,
        size: u64,
        backing: VmBacking,
        ceiling: VmProtection,
    ) -> Result<(usize, Cap<VmMemoryObjectTag>), VmError> {
        // A ceiling is potential authority, not an active mapping.
        ceiling_permissions(ceiling)?;
        if backing != VmBacking::Resident {
            return Err(VmError::Unsupported);
        }
        if size == 0 || size.checked_add(PAGE_SIZE as u64 - 1).is_none() {
            return Err(VmError::Invalid);
        }
        if size > (self.pool.available() * PAGE_SIZE) as u64 {
            return Err(VmError::QuotaExceeded);
        }
        if self
            .memories
            .iter()
            .flatten()
            .filter(|m| m.owner == self.badges[SPACE])
            .count()
            >= self.max_objects
        {
            return Err(VmError::Exhausted);
        }
        let index = self
            .memories
            .iter()
            .position(Option::is_none)
            .ok_or(VmError::Exhausted)?;
        let badge = new_badge()?;
        let object = MemoryObject::allocate(size, &mut self.pool).map_err(error)?;
        let (authority, cap) = match self.export(badge) {
            Ok(value) => value,
            Err(value) => {
                object.release(&mut self.pool).map_err(error)?;
                return Err(value);
            }
        };
        self.memories[index] = Some(Memory {
            handle: MemoryHandle {
                badge,
                authority: Some(authority),
                ceiling,
            },
            owner: self.badges[SPACE],
            allocator: self.allocator.clone(),
            backing: object,
            pool: self.pool.clone(),
            immutable: false,
            references: 0,
            dma: crate::dma::Pins::default(),
            mmio_lease: false,
            poisoned: false,
        });
        Ok((index, cap))
    }

    fn reserve(&mut self, request: VmRegionRequest) -> Result<Cap<VmRegionObjectTag>, VmError> {
        ceiling_permissions(request.ceiling)?;
        if request.length == 0
            || request.length % PAGE_SIZE as u64 != 0
            || !request.alignment.is_power_of_two()
            || request.alignment < PAGE_SIZE as u64
        {
            return Err(VmError::Invalid);
        }
        let size = usize::try_from(request.length).map_err(|_| VmError::Invalid)?;
        let alignment = usize::try_from(request.alignment).map_err(|_| VmError::Invalid)?;
        let address = usize::try_from(request.address).map_err(|_| VmError::Invalid)?;
        let range = crate::placement::choose(
            self.window.clone(),
            size,
            alignment,
            (request.placement == VmPlacement::Fixed).then_some(address),
            self.regions
                .iter()
                .flatten()
                .filter(|r| r.owner == self.badges[SPACE])
                .map(|r| r.address..r.address + r.range.len()),
        )?;
        let range = range.start - self.window.start..range.end - self.window.start;
        if self
            .regions
            .iter()
            .flatten()
            .filter(|r| r.owner == self.badges[SPACE])
            .count()
            >= self.max_objects
        {
            return Err(VmError::Exhausted);
        }
        let index = self
            .regions
            .iter()
            .position(Option::is_none)
            .ok_or(VmError::Exhausted)?;
        let badge = new_badge()?;
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(size / PAGE_SIZE)
            .map_err(|_| VmError::Exhausted)?;
        pages.resize(size / PAGE_SIZE, Page::default());
        let (authority, cap) = self.export(badge)?;
        self.regions[index] = Some(Region {
            owner: self.badges[SPACE],
            allocator: self.allocator.clone(),
            vspace: self.vspace,
            address: self.window.start + range.start,
            badge,
            authority,
            range,
            ceiling: request.ceiling,
            pages,
            poisoned: false,
        });
        Ok(cap)
    }

    fn collect(&mut self, index: usize) -> Result<(), VmError> {
        collect_backing(&mut self.memories, index)
    }

    fn close(&mut self, index: usize) -> Result<(), VmError> {
        let authority = self.memory(index).handle.authority.ok_or(VmError::Stale)?;
        let mut allocator = self.memory(index).allocator.clone();
        allocator
            .root()
            .absolute_cptr(::sel4::CPtr::from_bits(authority as _))
            .revoke()
            .map_err(|_| VmError::Backend)?;
        allocator.release_slot(authority).map_err(error)?;
        self.memories[index].as_mut().unwrap().handle.authority = None;
        self.collect(index)
    }

    fn make_immutable(&mut self, index: usize) -> Result<(), VmError> {
        let memory = self.memory(index);
        if memory.poisoned {
            return Err(VmError::Quarantined);
        }
        if memory.backing.resident_size() == 0 {
            return Err(VmError::Unsupported);
        }
        if memory.immutable {
            return Ok(());
        }
        // Serialized with CPU mapping changes and DMA pins. Protect(READ)
        // retains MAYWRITE; callers must unmap before freezing backing.
        let may_write = memory.dma.writers != 0
            || self.regions.iter().flatten().any(|region| {
                region
                    .pages
                    .iter()
                    .any(|page| page.memory == Some(index) && page.may_write)
            });
        if may_write {
            return Err(VmError::Busy);
        }
        self.memories[index].as_mut().unwrap().immutable = true;
        Ok(())
    }

    fn prepare(&mut self, region: usize, range: Range<usize>) -> Result<(), VmError> {
        for page in range.clone() {
            if self.page(region, page).slot.is_some() {
                continue;
            }
            match self.allocator.reserve_slot() {
                Ok(slot) => {
                    let state = self.page_mut(region, page);
                    state.slot = Some(slot);
                    state.fresh = true;
                }
                Err(value) => {
                    for previous in range.clone() {
                        let state = self.page(region, previous);
                        if state.fresh {
                            if self.allocator.release_slot(state.slot.unwrap()).is_err() {
                                return Err(self.poison(region));
                            }
                            *self.page_mut(region, previous) = Page::default();
                        }
                    }
                    return Err(error(value));
                }
            }
        }
        for page in range {
            self.page_mut(region, page).fresh = false;
        }
        Ok(())
    }

    fn address(&self, region: usize, page: usize) -> usize {
        self.region(region).address + page * PAGE_SIZE
    }
    fn map_page(
        &mut self,
        region: usize,
        page: usize,
        protection: VmProtection,
    ) -> Result<(), VmError> {
        let state = self.page(region, page);
        ::sel4::cap::Granule::from_bits(state.slot.unwrap() as _)
            .frame_map(
                self.region(region).vspace,
                self.address(region, page),
                ::sel4::CapRights::new(
                    false,
                    false,
                    true,
                    protection.0 & VmProtection::WRITE.0 != 0,
                ),
                if self.memory(state.memory.unwrap()).backing.resident_size() == 0 {
                    ::sel4::VmAttributes::CACHE_DISABLED | ::sel4::VmAttributes::EXECUTE_NEVER
                } else if protection.0 & VmProtection::EXECUTE.0 == 0 {
                    ::sel4::VmAttributes::EXECUTE_NEVER
                } else {
                    ::sel4::VmAttributes::default()
                },
            )
            .map_err(|_| VmError::Backend)?;
        let state = self.page_mut(region, page);
        state.mapped = true;
        state.protection = protection;
        Ok(())
    }
    fn unmap_page(&mut self, region: usize, page: usize) -> Result<(), VmError> {
        let state = self.page(region, page);
        if state.mapped {
            ::sel4::cap::Granule::from_bits(state.slot.unwrap() as _)
                .frame_unmap()
                .map_err(|_| VmError::Backend)?;
            self.page_mut(region, page).mapped = false;
        }
        Ok(())
    }
    fn clear_page(&mut self, region: usize, page: usize) -> Result<(), VmError> {
        self.unmap_page(region, page)?;
        let state = self.page(region, page);
        if state.memory.is_some() {
            self.region(region)
                .allocator
                .clone()
                .clear_slot(state.slot.unwrap())
                .map_err(error)?;
        }
        let memory = state.memory;
        *self.page_mut(region, page) = Page {
            slot: state.slot,
            ..Page::default()
        };
        if let Some(memory) = memory {
            self.memories[memory].as_mut().unwrap().references -= 1;
            self.collect(memory)?;
        }
        Ok(())
    }

    fn map(
        &mut self,
        region: usize,
        range: Range<usize>,
        memory: usize,
        source: usize,
        protection: VmProtection,
        handle: ::sel4::Word,
    ) -> Result<(), VmError> {
        permissions(protection)?;
        let ceiling = VmProtection(self.handle_rights(handle)?.0 & 7);
        let (ancestors, depth) = self.lineage(handle);
        let lineage = Rc::new(Lineage { ancestors, depth });
        if self.memory(memory).poisoned {
            return Err(VmError::Quarantined);
        }
        if protection.0 & !self.region(region).ceiling.0 != 0 || protection.0 & !ceiling.0 != 0 {
            return Err(VmError::Denied);
        }
        if protection.0 & VmProtection::WRITE.0 != 0 && self.memory(memory).immutable {
            return Err(VmError::Denied);
        }
        let may_write = self.region(region).ceiling.0 & ceiling.0 & VmProtection::WRITE.0 != 0
            && !self.memory(memory).immutable;
        if source
            .checked_add(range.len())
            .is_none_or(|end| end > self.memory(memory).backing.page_count())
        {
            return Err(VmError::Invalid);
        }
        for page in range.clone() {
            let state = self.page(region, page);
            if state.slot.is_none() {
                return Err(VmError::Invalid);
            }
            if state.memory.is_some() {
                return Err(VmError::Overlap);
            }
        }
        let root = self.allocator.root();
        for (offset, page) in range.clone().enumerate() {
            let state = self.page(region, page);
            let frame = self
                .memory(memory)
                .pool
                .cap(&self.memory(memory).backing.frames()[source + offset])
                .map_err(error)?;
            let result = root
                .absolute_cptr(::sel4::CPtr::from_bits(state.slot.unwrap() as _))
                .copy(
                    &root.absolute_cptr(::sel4::CPtr::from_bits(frame.bits())),
                    ::sel4::CapRights::all(),
                );
            if result.is_ok() {
                self.memories[memory].as_mut().unwrap().references += 1;
                let state = self.page_mut(region, page);
                state.memory = Some(memory);
                state.handle = handle;
                state.ceiling = ceiling;
                state.lineage = Some(lineage.clone());
                state.may_write = may_write;
            }
            if result.is_err() || self.map_page(region, page, protection).is_err() {
                for rollback in range.start..=page {
                    if self.clear_page(region, rollback).is_err() {
                        return Err(self.poison(region));
                    }
                }
                return Err(VmError::Backend);
            }
        }
        Ok(())
    }

    fn protect(
        &mut self,
        region: usize,
        range: Range<usize>,
        protection: VmProtection,
    ) -> Result<(), VmError> {
        permissions(protection)?;
        if protection.0 & !self.region(region).ceiling.0 != 0 {
            return Err(VmError::Denied);
        }
        for page in range.clone() {
            let state = self.page(region, page);
            let memory = state.memory.ok_or(VmError::Invalid)?;
            if self.memory(memory).poisoned {
                return Err(VmError::Quarantined);
            }
            if !state.mapped {
                return Err(VmError::Invalid);
            }
            if protection.0 & !state.ceiling.0 != 0
                || (protection.0 & VmProtection::WRITE.0 != 0
                    && (!state.may_write || self.memory(memory).immutable))
            {
                return Err(VmError::Denied);
            }
            self.page_mut(region, page).previous = state.protection;
        }
        for page in range.clone() {
            if self.unmap_page(region, page).is_err()
                || self.map_page(region, page, protection).is_err()
            {
                for previous in range.start..=page {
                    let old = self.page(region, previous).previous;
                    if self.unmap_page(region, previous).is_err()
                        || self.map_page(region, previous, old).is_err()
                    {
                        return Err(self.poison(region));
                    }
                }
                return Err(VmError::Backend);
            }
        }
        Ok(())
    }

    fn unmap(&mut self, region: usize, range: Range<usize>) -> Result<(), VmError> {
        for page in range.clone() {
            let old = self.page(region, page).mapped;
            self.page_mut(region, page).fresh = old;
        }
        for page in range.clone() {
            if self.unmap_page(region, page).is_err() {
                for previous in range.start..page {
                    let state = self.page(region, previous);
                    if state.fresh && self.map_page(region, previous, state.protection).is_err() {
                        return Err(self.poison(region));
                    }
                }
                return Err(VmError::Backend);
            }
        }
        for page in range {
            if self.clear_page(region, page).is_err() {
                return Err(self.poison(region));
            }
        }
        Ok(())
    }

    fn apply(
        &mut self,
        region: usize,
        memory: Option<Cap<VmMemoryObjectTag>>,
        updates: Parcel<List<VmUpdate, 16>>,
    ) -> Result<(), VmBatchError> {
        if self.region(region).poisoned {
            return Err(VmBatchError {
                completed: 0,
                error: VmError::Quarantined,
            });
        }
        let source = memory
            .map(|cap| {
                let badge = cap.as_ref().unwrapped_badge().ok_or(VmError::Denied)?;
                self.memory_index(badge as _)
            })
            .transpose()
            .map_err(|error| VmBatchError {
                completed: 0,
                error,
            })?;
        for (index, update) in updates.data.data[..updates.data.len as usize]
            .iter()
            .enumerate()
        {
            let result = (|| {
                let (offset, length) = match update {
                    VmUpdate::Prepare(r) | VmUpdate::Unmap(r) => (r.offset, r.length),
                    VmUpdate::Map(r) => (r.offset, r.length),
                    VmUpdate::Protect(r) => (r.offset, r.length),
                };
                let range = crate::page_range(
                    offset,
                    length,
                    self.region(region).range.len() as u64,
                    PAGE_SIZE as u64,
                )
                .map_err(error)?;
                match *update {
                    VmUpdate::Prepare(_) => self.prepare(region, range),
                    VmUpdate::Map(r) => {
                        if r.memory_offset % PAGE_SIZE as u64 != 0 {
                            return Err(VmError::Invalid);
                        }
                        let source_page = usize::try_from(r.memory_offset / PAGE_SIZE as u64)
                            .map_err(|_| VmError::Invalid)?;
                        self.map(
                            region,
                            range,
                            source.ok_or(VmError::Invalid)?,
                            source_page,
                            r.protection,
                            memory
                                .unwrap()
                                .as_ref()
                                .unwrapped_badge()
                                .ok_or(VmError::Denied)? as _,
                        )
                    }
                    VmUpdate::Protect(r) => self.protect(region, range, r.protection),
                    VmUpdate::Unmap(_) => self.unmap(region, range),
                }
            })();
            if let Err(error) = result {
                return Err(VmBatchError {
                    completed: index as u32,
                    error,
                });
            }
        }
        Ok(())
    }

    fn destroy_region(&mut self, index: usize) -> Result<(), VmError> {
        // Retried cleanup may recover a quarantined region, but never reuses it early.
        let count = self.region(index).pages.len();
        for page in 0..count {
            if self.clear_page(index, page).is_err() {
                return Err(self.poison(index));
            }
            if let Some(slot) = self.page(index, page).slot {
                if self.allocator.release_slot(slot).is_err() {
                    return Err(self.poison(index));
                }
                self.page_mut(index, page).slot = None;
            }
        }
        let authority = self.region(index).authority;
        self.allocator
            .root()
            .absolute_cptr(::sel4::CPtr::from_bits(authority as _))
            .revoke()
            .map_err(|_| VmError::Backend)?;
        self.allocator.release_slot(authority).map_err(error)?;
        self.regions[index].take().unwrap();
        Ok(())
    }
}

fn ceiling_permissions(value: VmProtection) -> Result<(), VmError> {
    if value.0 & VmProtection::READ.0 == 0 || value.0 & !7 != 0 {
        return Err(VmError::Unsupported);
    }
    Ok(())
}
fn permissions(value: VmProtection) -> Result<(), VmError> {
    ceiling_permissions(value)?;
    if value.0 & (VmProtection::WRITE.0 | VmProtection::EXECUTE.0)
        == VmProtection::WRITE.0 | VmProtection::EXECUTE.0
    {
        return Err(VmError::Denied);
    }
    Ok(())
}

struct Caller<'a> {
    service: &'a mut Service,
    badge: ::sel4::Word,
}
impl VmFactoryServer for Caller<'_> {
    fn info(&mut self) -> Result<VmInfo, VmError> {
        if self.badge != self.service.badges[FACTORY] {
            return Err(VmError::Denied);
        }
        Ok(VmInfo {
            features: VmFeatures(
                VmFeatures::RESIDENT.0
                    | VmFeatures::PROTECTION.0
                    | VmFeatures::UNTYPED.0
                    | VmFeatures::IMMUTABLE.0
                    | VmFeatures::DUPLICATE_MEMORY.0
                    | VmFeatures::EXECUTABLE.0
                    | VmFeatures::FIXED_PLACEMENT.0,
            ),
            page_size: PAGE_SIZE as u64,
            batch_limit: 16,
            limit_bytes: self.service.pool.account().usage().limit as u64,
            charged_bytes: self.service.pool.account().usage().charged as u64,
            quarantined_bytes: self.service.pool.quarantined_bytes() as u64
                + self
                    .service
                    .memories
                    .iter()
                    .filter_map(Option::as_ref)
                    .filter(|memory| memory.owner == self.service.badges[SPACE] && memory.poisoned)
                    .map(|memory| memory.backing.resident_size())
                    .sum::<u64>(),
        })
    }
    fn allocate(
        &mut self,
        size: u64,
        backing: VmBacking,
        ceiling: VmProtection,
    ) -> Result<Cap<VmMemoryObjectTag>, VmError> {
        if self.badge != self.service.badges[FACTORY] {
            return Err(VmError::Denied);
        }
        self.service
            .allocate(size, backing, ceiling)
            .map(|(_, cap)| cap)
    }
    fn create_address_space(&mut self) -> Result<Cap<VmAddressSpaceObjectTag>, VmError> {
        if self.badge != self.service.badges[FACTORY] {
            return Err(VmError::Denied);
        }
        Err(VmError::Unsupported)
    }
    fn create_pager(&mut self, _: Cap<Notification>) -> Result<Cap<VmPagerObjectTag>, VmError> {
        if self.badge != self.service.badges[FACTORY] {
            return Err(VmError::Denied);
        }
        Err(VmError::Unsupported)
    }
    fn delegate(&mut self, bits: u32) -> Result<Cap<VmUntypedObjectTag>, VmError> {
        if self.badge != self.service.badges[FACTORY] {
            return Err(VmError::Denied);
        }
        let index = self
            .service
            .delegations
            .iter()
            .position(Option::is_none)
            .ok_or(VmError::Exhausted)?;
        let badge = new_badge()?;
        let lease = self
            .service
            .allocator
            .delegate(bits as usize)
            .map_err(error)?;
        let (authority, cap) = match self.service.export(badge) {
            Ok(result) => result,
            Err(error) => {
                self.service
                    .allocator
                    .reclaim(lease)
                    .map_err(crate::service::error)?;
                return Err(error);
            }
        };
        self.service.delegations[index] = Some(Delegation {
            badge,
            authority,
            lease: Some(lease),
        });
        Ok(cap)
    }
    fn derive(&mut self, _: u64) -> Result<Cap<VmServiceTag>, VmError> {
        if self.badge != self.service.badges[FACTORY] {
            return Err(VmError::Denied);
        }
        Err(VmError::Unsupported)
    }
}
impl VmUntypedServer for Caller<'_> {
    fn capability(&mut self) -> Result<VmUntypedGrant, VmError> {
        let index = self.service.delegation_index(self.badge)?;
        let lease = self.service.delegations[index]
            .as_ref()
            .unwrap()
            .lease
            .as_ref()
            .ok_or(VmError::Quarantined)?;
        Ok(VmUntypedGrant {
            capability: Cap::from_ref(CapRef::new(
                self.service.allocator.root().bits() as u64,
                lease.cap().bits() as u32,
                ::sel4::WORD_SIZE as u8,
                0,
                CapMode::COPY,
                Rights::ALL,
            )),
            size_bits: lease.size_bits() as u32,
        })
    }
    fn close(&mut self) -> Result<(), VmError> {
        let index = self.service.delegation_index(self.badge)?;
        self.service.close_delegation(index)
    }
}
impl VmMemoryServer for Caller<'_> {
    fn info(&mut self) -> Result<VmMemoryInfo, VmError> {
        let index = self.service.memory_index(self.badge)?;
        let memory = self.service.memory(index);
        Ok(VmMemoryInfo {
            size: memory.backing.size(),
            resident_size: memory.backing.resident_size(),
            ceiling: VmProtection(self.service.handle_rights(self.badge)?.0 & 7),
            immutable: memory.immutable,
        })
    }
    fn make_immutable(&mut self) -> Result<(), VmError> {
        self.service
            .require(self.badge, VmMemoryRights::MAKE_IMMUTABLE)?;
        let index = self.service.memory_index(self.badge)?;
        self.service.make_immutable(index)
    }
    fn duplicate(&mut self, ceiling: VmProtection) -> Result<Cap<VmMemoryObjectTag>, VmError> {
        ceiling_permissions(ceiling)?;
        self.service
            .delegate_handle(self.badge, VmMemoryRights(ceiling.0))
    }
    fn close(&mut self) -> Result<(), VmError> {
        self.service.require(self.badge, VmMemoryRights::CLOSE)?;
        self.service.close_handle(self.badge)
    }
    fn revoke(&mut self) -> Result<(), VmError> {
        self.service.require(self.badge, VmMemoryRights::REVOKE)?;
        let index = self.service.memory_index(self.badge)?;
        self.service.revoke_tree(index, self.badge)
    }
    fn delegate(&mut self, rights: VmMemoryRights) -> Result<Cap<VmMemoryObjectTag>, VmError> {
        self.service.delegate_handle(self.badge, rights)
    }
    fn rights(&mut self) -> Result<VmMemoryRights, VmError> {
        self.service.handle_rights(self.badge)
    }
}
impl VmAddressSpaceServer for Caller<'_> {
    fn reserve(&mut self, request: VmRegionRequest) -> Result<Cap<VmRegionObjectTag>, VmError> {
        if self.badge != self.service.badges[SPACE] {
            return Err(VmError::Denied);
        }
        self.service.reserve(request)
    }
    fn destroy(&mut self) -> Result<(), VmError> {
        if self.badge != self.service.badges[SPACE] {
            return Err(VmError::Denied);
        }
        Err(VmError::Busy)
    }
}
impl VmRegionServer for Caller<'_> {
    fn info(&mut self) -> Result<VmRegionInfo, VmError> {
        let index = self.service.region_index(self.badge)?;
        let region = self.service.region(index);
        Ok(VmRegionInfo {
            address: region.address as u64,
            length: region.range.len() as u64,
            ceiling: region.ceiling,
        })
    }
    fn apply(
        &mut self,
        memory: Option<Cap<VmMemoryObjectTag>>,
        updates: Parcel<List<VmUpdate, 16>>,
    ) -> Result<(), VmBatchError> {
        let region = self
            .service
            .region_index(self.badge)
            .map_err(|error| VmBatchError {
                completed: 0,
                error,
            })?;
        self.service.apply(region, memory, updates)
    }
    fn bind_pager(&mut self, _: Cap<VmPagerObjectTag>, _: u64) -> Result<(), VmError> {
        self.service.region_index(self.badge)?;
        Err(VmError::Unsupported)
    }
    fn destroy(&mut self) -> Result<(), VmError> {
        let region = self.service.region_index(self.badge)?;
        self.service.destroy_region(region)
    }
}
impl VmPagerServer for Caller<'_> {
    fn drain(&mut self) -> Result<Parcel<List<VmFault, 16>>, VmError> {
        Err(VmError::Denied)
    }
    fn resolve(
        &mut self,
        _: Option<Cap<VmMemoryObjectTag>>,
        _: Parcel<List<VmFaultReply, 16>>,
    ) -> Result<(), VmBatchError> {
        Err(VmBatchError {
            completed: 0,
            error: VmError::Denied,
        })
    }
    fn destroy(&mut self) -> Result<(), VmError> {
        Err(VmError::Denied)
    }
}
