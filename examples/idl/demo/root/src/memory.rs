//! Root-hosted memory RPC adapter. Kernel badges, not client-supplied IDs,
//! select objects. This is a bounded resident-backing milestone, not a pager.
use alloc::vec::Vec;
use horn_ipc::*;
use horn_memory::{Error, MemoryObject as Backing};
use horn_memory::sel4::{Frame, FramePool, PAGE_SIZE};
use crate::protocol::*;

pub const ALLOCATOR_BADGE: ::sel4::Word = 1;
const MAX_OBJECTS: usize = 4;
const DENIED: u32 = 5;
const BAD_HANDLE: u32 = 6;

struct Entry {
    badge: ::sel4::Word,
    authority: ::sel4::AbsoluteCPtr,
    backing: Backing<Frame>,
}

pub fn status(error: Error) -> u32 {
    match error {
        Error::InvalidSize => 1,
        Error::Exhausted => 2,
        Error::MetadataExhausted => 3,
        Error::Backend => 4,
    }
}

/// Mint a send-only endpoint view. It cannot receive other clients' calls.
pub fn mint(
    space: &mut Sel4CapSpace,
    endpoint: ::sel4::cap::Endpoint,
    badge: ::sel4::Word,
) -> Result<::sel4::AbsoluteCPtr, RpcError> {
    let slot = space.reserve_slot()?;
    slot.mint(
        &space.root.absolute_cptr(::sel4::CPtr::from_bits(endpoint.bits())),
        ::sel4::CapRights::new(true, true, false, true),
        badge,
    ).map_err(|_| RpcError::Transport)?;
    Ok(slot)
}

pub fn serve(
    mut server: Sel4Server<Sel4CapSpace>,
    reply_authority: ::sel4::cap::Reply,
    mut pool: FramePool,
) {
    let mut objects: Vec<Entry> = Vec::new();
    objects.try_reserve_exact(MAX_OBJECTS).unwrap();
    let mut next_badge: ::sel4::Word = 2;
    let mut released = 0;
    // The demo stops after three releases. A real service runs until shutdown.
    while released < 3 {
        let (request, reply) = memory_host_recv(&mut server, reply_authority).unwrap();
        let badge = reply.badge;
        let result: Result<(MemoryHostResponse, Option<::sel4::AbsoluteCPtr>), u32> = (|| {
            match request {
                MemoryHostRequest::MemoryallocatorAllocate(args) => {
                    if badge != ALLOCATOR_BADGE { return Err(DENIED); }
                    if args.size == 0 || args.size > (pool.capacity() * PAGE_SIZE * 2) as u64 {
                        return Err(status(Error::InvalidSize));
                    }
                    if objects.len() == MAX_OBJECTS { return Err(status(Error::Exhausted)); }
                    let backing = Backing::allocate(args.size, &mut pool).map_err(status)?;
                    // Real-kernel zeroing probe, including pages reclaimed from earlier objects.
                    for frame in backing.frames() { assert!(pool.is_zeroed(frame).unwrap()); }
                    let authority = match mint(&mut server.space, server.endpoint, next_badge) {
                        Ok(cap) => cap,
                        Err(_) => { backing.release(&mut pool).unwrap(); return Err(status(Error::Exhausted)); }
                    };
                    let outgoing = match server.space.reserve_slot() {
                        Ok(cap) => cap,
                        Err(_) => {
                            authority.delete().unwrap();
                            backing.release(&mut pool).unwrap();
                            return Err(status(Error::Exhausted));
                        }
                    };
                    if outgoing.copy(&authority, ::sel4::CapRights::all()).is_err() {
                        authority.delete().unwrap();
                        backing.release(&mut pool).unwrap();
                        return Err(status(Error::Backend));
                    }
                    let handle = Cap::from_ref(CapRef::new(
                        server.cnode.bits() as u64, outgoing.path().bits() as u32,
                        ::sel4::WORD_SIZE as u8, 0, CapMode::MOVE, Rights::ALL,
                    ));
                    objects.push(Entry { badge: next_badge, authority, backing });
                    next_badge = next_badge.checked_add(1).unwrap();
                    Ok((MemoryHostResponse::MemoryallocatorAllocate(handle), None))
                }
                MemoryHostRequest::MemoryInfo(_) => {
                    if badge == ALLOCATOR_BADGE { return Err(DENIED); }
                    let entry = objects.iter().find(|entry| entry.badge == badge).ok_or(BAD_HANDLE)?;
                    Ok((MemoryHostResponse::MemoryInfo(MemoryInfo {
                        size: entry.backing.size(), resident_size: entry.backing.resident_size(),
                        page_size: PAGE_SIZE as u64,
                    }), None))
                }
                MemoryHostRequest::MemoryRelease(_) => {
                    if badge == ALLOCATOR_BADGE { return Err(DENIED); }
                    let index = objects.iter().position(|entry| entry.badge == badge).ok_or(BAD_HANDLE)?;
                    let entry = objects.swap_remove(index);
                    // Dirty backing before reclamation, to check subsequent zero-on-allocation.
                    for frame in entry.backing.frames() { pool.fill(frame, 0xa5).unwrap(); }
                    entry.backing.release(&mut pool).map_err(status)?;
                    released += 1;
                    Ok((MemoryHostResponse::MemoryRelease(()), Some(entry.authority)))
                }
            }
        })();
        match result {
            Ok((response, retire)) => {
                memory_host_reply(reply, &mut server.space, &response).unwrap();
                if let Some(authority) = retire {
                    // Revoke only this object's descendants, not the allocator or other objects.
                    authority.revoke().unwrap();
                    authority.delete().unwrap();
                }
            }
            Err(status) => reply.reply_status(status).unwrap(),
        }
    }
    assert!(objects.is_empty());
    assert_eq!(pool.available(), pool.capacity());
    ::sel4::debug_println!("MEMORY_RPC_PASS zeroed=true rollback=true reclaimed=true");
}
