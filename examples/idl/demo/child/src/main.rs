#![no_std]
#![no_main]

mod runtime;

#[allow(dead_code, unused_imports, non_camel_case_types)]
mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

use horn_ipc::*;
use protocol::*;

struct CounterHandler {
    value: u64,
}

impl Counter for CounterHandler {
    fn add(&mut self, amount: u64) -> CounterValue {
        self.value = self.value.wrapping_add(amount);
        CounterValue {
            value: self.value,
            generation: 0,
        }
    }
}

fn memory_probe(root: ::sel4::cap::CNode, recv: ::sel4::AbsoluteCPtr, space: Sel4CapSpace) {
    let allocator_ref = CapRef::new(root.bits() as u64, 5, ::sel4::WORD_SIZE as u8, 0, CapMode::COPY, Rights::ALL);
    let target = Cap::<MemoryAllocatorTag>::from_ref(allocator_ref);
    let mut client = MemoryAllocatorClient::new(Sel4Transport::new(root, recv, space), target);
    assert_eq!(client.allocate(0).unwrap_err(), RpcError::Server(1));
    assert_eq!(client.allocate(u64::MAX).unwrap_err(), RpcError::Server(1));
    // Four pages are available: this must fail after partial allocation and roll back.
    assert_eq!(client.allocate(5 * 4096).unwrap_err(), RpcError::Server(2));
    for size in [4103, 7, 8192] {
        let object = client.allocate(size).unwrap();
        let object_ref = object.as_ref();
        let mut memory = MemoryClient::new(client.transport, Cap::from_ref(object_ref));
        let info = memory.info().unwrap();
        assert_eq!(info.size, size);
        assert_eq!(info.resident_size, size.next_multiple_of(4096));
        assert_eq!(info.page_size, 4096);
        // A typed interface view is not security: the server must check the badge.
        memory.target = Cap::from_ref(allocator_ref);
        assert_eq!(memory.info().unwrap_err(), RpcError::Server(5));
        let mut wrong = MemoryAllocatorClient::new(memory.transport, Cap::from_ref(object_ref));
        assert_eq!(wrong.allocate(1).unwrap_err(), RpcError::Server(5));
        let mut memory = MemoryClient::new(wrong.transport, Cap::from_ref(object_ref));
        memory.release().unwrap();
        memory.transport.space.delete(object_ref).unwrap();
        // This tiny bootstrap CSpace has one import slot. Reuse it only after
        // explicitly releasing and deleting the previous object handle.
        memory.transport.space = Sel4CapSpace::new_local(root, 7, 8);
        client = MemoryAllocatorClient::new(memory.transport, target);
    }
    ::sel4::debug_println!("memory client: allocation, info, errors and badge isolation passed");
}

fn main() -> ! {
    ::sel4::debug_println!("idl demo child: start");

    // ChildCaps slot layout: 0 = self CNode, 1 = service, 2 = wake,
    // 3 = reply, 4 = TCB, 5 = memory allocator, 6 = receive, 7 = imported cap.
    let root_cnode = ::sel4::cap::CNode::from_bits(0);
    let service = ::sel4::cap::Endpoint::from_bits(1);
    let wake = ::sel4::cap::Notification::from_bits(2);
    let reply_cap = ::sel4::cap::Reply::from_bits(3);
    let tcb = ::sel4::cap::Tcb::from_bits(4);

    let space = Sel4CapSpace::new_local(root_cnode, 7, 8);
    let recv_slot = root_cnode.absolute_cptr(::sel4::CPtr::from_bits(6));
    let mut server = Sel4Server::new(root_cnode, service, recv_slot, space);
    let mut handler = CounterHandler { value: 0 };

    let (request, reply) = counter_service_recv(&mut server, reply_cap).unwrap();
    let response = counter_service_dispatch(request, &mut handler);
    counter_service_reply(reply, &mut server.space, &response).unwrap();

    ::sel4::debug_println!("idl demo child: replied value={}", handler.value);
    wake.signal();
    memory_probe(root_cnode, recv_slot, server.space);
    wake.signal();
    tcb.tcb_suspend().unwrap();
    unreachable!()
}
