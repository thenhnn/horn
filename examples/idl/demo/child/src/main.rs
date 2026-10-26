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

fn main() -> ! {
    ::sel4::debug_println!("idl demo child: start");

    // ChildCaps slot layout: 0 = self CNode, 1 = service, 2 = wake,
    // 3 = reply, 4 = TCB.
    let root_cnode = ::sel4::cap::CNode::from_bits(0);
    let service = ::sel4::cap::Endpoint::from_bits(1);
    let wake = ::sel4::cap::Notification::from_bits(2);
    let reply_cap = ::sel4::cap::Reply::from_bits(3);
    let tcb = ::sel4::cap::Tcb::from_bits(4);

    let mut space = Sel4CapSpace::new_local(root_cnode, 5, 8);
    let recv_slot = root_cnode.absolute_cptr(::sel4::CPtr::from_bits(5));
    let mut server = Sel4Server::new(root_cnode, service, recv_slot, space);
    let mut handler = CounterHandler { value: 0 };

    let (_label, request, reply) = server
        .recv_request(&COUNTER_ADD_REQ_LAYOUT, reply_cap, |label, input| {
            CounterServiceRequest::decode(label, input)
        })
        .unwrap();
    let response = counter_service_dispatch(request, &mut handler);
    reply
        .reply_with(
            &mut server.space,
            &COUNTER_ADD_RESULT_LAYOUT,
            |out| response.encode(out),
            |caps| response.caps(caps),
        )
        .unwrap();

    ::sel4::debug_println!("idl demo child: replied value={}", handler.value);
    wake.signal();
    tcb.tcb_suspend().unwrap();
    unreachable!()
}
