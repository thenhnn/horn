use crate::protocol::*;
use horn_ipc::*;

fn error(code: MemoryError) -> RpcError {
    RpcError::Server(code as u32)
}

pub fn run(root: ::sel4::cap::CNode, recv: ::sel4::AbsoluteCPtr, space: Sel4CapSpace) {
    let allocator_ref = CapRef::new(
        root.bits() as u64,
        CHILD_CAPS_MEMORY_SLOT,
        ::sel4::WORD_SIZE as u8,
        0,
        CapMode::COPY,
        Rights::ALL,
    );
    let target = Cap::<MemoryAllocatorTag>::from_ref(allocator_ref);
    let mut client = MemoryAllocatorClient::new(Sel4Transport::new(root, recv, space), target);
    assert_eq!(
        client.allocate(0).unwrap_err(),
        error(MemoryError::InvalidSize)
    );
    assert_eq!(
        client.allocate(u64::MAX).unwrap_err(),
        error(MemoryError::InvalidSize)
    );
    assert_eq!(
        client.allocate(u64::MAX / 2).unwrap_err(),
        error(MemoryError::Exhausted)
    );
    let page = ::sel4::FrameObjectType::GRANULE.bytes() as u64;
    for size in [page + 7, 7, page * 2] {
        let object = client.allocate(size).unwrap();
        let object_ref = object.as_ref();
        let mut memory = MemoryClient::new(client.transport, Cap::from_ref(object_ref));
        let info = memory.info().unwrap();
        assert_eq!(info.size, size);
        assert_eq!(info.resident_size, size.next_multiple_of(page));
        assert_eq!(info.page_size, page);
        memory.target = Cap::from_ref(allocator_ref);
        assert_eq!(memory.info().unwrap_err(), error(MemoryError::Denied));
        let mut wrong = MemoryAllocatorClient::new(memory.transport, Cap::from_ref(object_ref));
        assert_eq!(wrong.allocate(1).unwrap_err(), error(MemoryError::Denied));
        let mut memory = MemoryClient::new(wrong.transport, Cap::from_ref(object_ref));
        memory.release().unwrap();
        memory.transport.space.delete(object_ref).unwrap();
        // Reuse the import slot only after releasing its previous handle.
        memory.transport.space = super::local_space(root);
        client = MemoryAllocatorClient::new(memory.transport, target);
    }
    ::sel4::debug_println!("MEMORY_RPC_PASS allocation=true errors=true isolation=true");
}
