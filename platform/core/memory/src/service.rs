use crate::protocol::{self, MemoryError, MemoryInfo, MemoryObjectTag};
use crate::sel4::{Frame, FramePool, KernelAllocator, PAGE_SIZE};
use crate::{Error, MemoryObject};
use alloc::vec::Vec;
use core::convert::Infallible;
use horn_ipc::*;

const ALLOCATOR_BADGE: ::sel4::Word = 1;

fn rpc(error: Error) -> RpcError {
    let code = match error {
        Error::InvalidSize => MemoryError::InvalidSize,
        Error::Exhausted => MemoryError::Exhausted,
        Error::MetadataExhausted => MemoryError::MetadataExhausted,
        Error::Backend => MemoryError::Backend,
    };
    failure(code)
}

fn failure(error: MemoryError) -> RpcError {
    RpcError::Server(error as u32)
}

fn mint(
    allocator: &mut KernelAllocator,
    endpoint: ::sel4::cap::Endpoint,
    badge: ::sel4::Word,
) -> Result<usize, Error> {
    let slot = allocator.reserve_slot()?;
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
        allocator.release_slot(slot)?;
        return Err(Error::Backend);
    }
    Ok(slot)
}

pub fn allocator_cap(
    allocator: &mut KernelAllocator,
    endpoint: ::sel4::cap::Endpoint,
) -> Result<::sel4::cap::Endpoint, Error> {
    mint(allocator, endpoint, ALLOCATOR_BADGE)
        .map(|slot| ::sel4::cap::Endpoint::from_bits(slot as _))
}

struct Entry {
    badge: ::sel4::Word,
    authority: usize,
    backing: MemoryObject<Frame>,
}

pub struct Service {
    allocator: KernelAllocator,
    pool: FramePool,
    endpoint: ::sel4::cap::Endpoint,
    objects: Vec<Entry>,
    max_objects: usize,
    next_badge: ::sel4::Word,
    outgoing: Option<usize>,
}

impl Service {
    pub fn new(
        allocator: KernelAllocator,
        pool: FramePool,
        endpoint: ::sel4::cap::Endpoint,
        max_objects: usize,
    ) -> Result<Self, Error> {
        let mut objects = Vec::new();
        objects
            .try_reserve_exact(max_objects)
            .map_err(|_| Error::MetadataExhausted)?;
        Ok(Self {
            allocator,
            pool,
            endpoint,
            objects,
            max_objects,
            next_badge: ALLOCATOR_BADGE + 1,
            outgoing: None,
        })
    }

    pub fn run(
        mut self,
        mut server: Sel4Server<Sel4CapSpace>,
        authority: ::sel4::ReplyAuthority,
    ) -> Result<Infallible, RpcError> {
        loop {
            let (request, reply) = protocol::memory_host_recv(&mut server, authority)?;
            let response = protocol::memory_host_dispatch(
                request,
                &mut Caller {
                    service: &mut self,
                    badge: reply.badge,
                },
            );
            let sent = protocol::memory_host_reply(reply, &mut server.space, &response);
            let cleanup = match self.outgoing.take() {
                Some(slot) => self.allocator.release_slot(slot).map_err(rpc),
                None => Ok(()),
            };
            sent.and(cleanup)?;
        }
    }

    fn allocate(&mut self, size: u64) -> Result<Cap<MemoryObjectTag>, RpcError> {
        if size == 0 || size.checked_add(PAGE_SIZE as u64 - 1).is_none() {
            return Err(failure(MemoryError::InvalidSize));
        }
        if size > (self.pool.capacity() * PAGE_SIZE) as u64
            || self.objects.len() == self.max_objects
        {
            return Err(failure(MemoryError::Exhausted));
        }
        let next = self
            .next_badge
            .checked_add(1)
            .ok_or(failure(MemoryError::Exhausted))?;
        let backing = MemoryObject::allocate(size, &mut self.pool).map_err(rpc)?;
        let caps = (|| {
            let authority = mint(&mut self.allocator, self.endpoint, self.next_badge)?;
            let outgoing = match self.allocator.reserve_slot() {
                Ok(slot) => slot,
                Err(error) => {
                    self.allocator.release_slot(authority)?;
                    return Err(error);
                }
            };
            let root = self.allocator.root();
            if root
                .absolute_cptr(::sel4::CPtr::from_bits(outgoing as _))
                .copy(
                    &root.absolute_cptr(::sel4::CPtr::from_bits(authority as _)),
                    ::sel4::CapRights::all(),
                )
                .is_err()
            {
                self.allocator.release_slot(outgoing)?;
                self.allocator.release_slot(authority)?;
                return Err(Error::Backend);
            }
            Ok((authority, outgoing))
        })();
        let (authority, outgoing) = match caps {
            Ok(caps) => caps,
            Err(error) => {
                backing.release(&mut self.pool).map_err(rpc)?;
                return Err(rpc(error));
            }
        };
        self.objects.push(Entry {
            badge: self.next_badge,
            authority,
            backing,
        });
        self.next_badge = next;
        self.outgoing = Some(outgoing);
        Ok(Cap::from_ref(CapRef::new(
            self.allocator.root().bits() as u64,
            outgoing as u32,
            ::sel4::WORD_SIZE as u8,
            0,
            CapMode::MOVE,
            Rights::ALL,
        )))
    }

    fn release(&mut self, badge: ::sel4::Word) -> Result<(), RpcError> {
        let index = self
            .objects
            .iter()
            .position(|entry| entry.badge == badge)
            .ok_or(failure(MemoryError::BadHandle))?;
        self.allocator
            .root()
            .absolute_cptr(::sel4::CPtr::from_bits(self.objects[index].authority as _))
            .revoke()
            .map_err(|_| failure(MemoryError::Backend))?;
        let entry = self.objects.swap_remove(index);
        let backing = entry.backing.release(&mut self.pool).map_err(rpc);
        let slot = self.allocator.release_slot(entry.authority).map_err(rpc);
        backing.and(slot)
    }
}

struct Caller<'a> {
    service: &'a mut Service,
    badge: ::sel4::Word,
}

impl protocol::MemoryAllocator for Caller<'_> {
    fn allocate(&mut self, size: u64) -> Result<Cap<MemoryObjectTag>, RpcError> {
        if self.badge != ALLOCATOR_BADGE {
            return Err(failure(MemoryError::Denied));
        }
        self.service.allocate(size)
    }
}

impl protocol::Memory for Caller<'_> {
    fn info(&mut self) -> Result<MemoryInfo, RpcError> {
        if self.badge == ALLOCATOR_BADGE {
            return Err(failure(MemoryError::Denied));
        }
        let entry = self
            .service
            .objects
            .iter()
            .find(|entry| entry.badge == self.badge)
            .ok_or(failure(MemoryError::BadHandle))?;
        Ok(MemoryInfo {
            size: entry.backing.size(),
            resident_size: entry.backing.resident_size(),
            page_size: PAGE_SIZE as u64,
        })
    }
    fn release(&mut self) -> Result<(), RpcError> {
        if self.badge == ALLOCATOR_BADGE {
            return Err(failure(MemoryError::Denied));
        }
        self.service.release(self.badge)
    }
}
