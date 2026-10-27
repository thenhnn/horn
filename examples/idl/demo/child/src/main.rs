#![no_std]
#![no_main]
#![feature(allocator_api)]

extern crate alloc;

horn_ipc::horn_tag!();

mod runtime;
mod vm_probe;

#[allow(dead_code, unused_imports, non_camel_case_types)]
mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

use horn_ipc::*;
use protocol::*;

struct CounterHandler {
    value: u64,
    wake: Cap<Notification>,
}

impl Counter for CounterHandler {
    fn add(&mut self, amount: u64) -> CounterValue {
        self.value = self.value.wrapping_add(amount);
        CounterValue {
            value: self.value,
            generation: 0,
            state: if amount == 0 {
                CounterState::Empty
            } else {
                CounterState::Ready(self.wake)
            },
        }
    }
}

fn local_space(root: ::sel4::cap::CNode) -> Sel4CapSpace {
    let first_import = CHILD_CAPS_FIRST_FREE_SLOT + 1;
    Sel4CapSpace::new_local(root, first_import as _, 1 << CHILD_CAPS_SIZE_BITS)
}

fn main() -> ! {
    ::sel4::debug_println!("idl demo child: start");
    let root = ::sel4::cap::CNode::from_bits(CHILD_CAPS_SELF_SLOT as _);
    let service = ::sel4::cap::Endpoint::from_bits(CHILD_CAPS_SERVICE_SLOT as _);
    let wake = ::sel4::cap::Notification::from_bits(CHILD_CAPS_WAKE_SLOT as _);
    let reply_cap = ::sel4::cap::Reply::from_bits(CHILD_CAPS_REPLY_SLOT as _);
    let tcb = ::sel4::cap::Tcb::from_bits(CHILD_CAPS_TCB_SLOT as _);
    let recv_slot = root.absolute_cptr(::sel4::CPtr::from_bits(CHILD_CAPS_FIRST_FREE_SLOT as _));
    let mut server = Sel4Server::new(root, service, recv_slot, local_space(root));
    let mut handler = CounterHandler {
        value: 0,
        wake: Cap::from_ref(server.space.register(wake).unwrap()),
    };
    for _ in 0..2 {
        let (request, reply) = counter_service_recv(&mut server, reply_cap).unwrap();
        let response = counter_service_dispatch(request, &mut handler);
        counter_service_reply(reply, &mut server.space, &response).unwrap();
    }
    wake.signal();
    vm_probe::run(root, recv_slot, server.space);
    let lifecycle = ::sel4::cap::Endpoint::from_bits(CHILD_CAPS_LIFECYCLE_SLOT as _);
    let mut space = local_space(root);
    let target = Cap::<horn_procd::protocol::ProcessLifecycleTag>::from_ref(
        space.register(lifecycle).unwrap(),
    );
    let transport = Sel4Transport::new(root, recv_slot, space);
    let mut process = horn_procd::protocol::ProcessLifecycleClient::new(transport, target);
    assert!(process.info().unwrap().charged_bytes > 0);
    process.exit(0).unwrap();
    tcb.tcb_suspend().unwrap();
    unreachable!()
}
