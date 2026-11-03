#![no_std]
#![no_main]
#![feature(allocator_api)]

extern crate alloc;

horn_runtime::task!(main, deferred_heap);

mod launch_probe;
mod vm_probe;

#[allow(dead_code, unused_imports, non_camel_case_types)]
mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

use horn_ipc::*;
use horn_procd::protocol::ProcessLifecycleClient;
use protocol::*;

struct CounterHandler {
    value: u64,
    wake: Cap<Notification>,
}

impl CounterServer for CounterHandler {
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

fn main(mut ctx: horn_runtime::Context) -> ! {
    ::sel4::debug_println!("idl demo child: start");
    let mut io = ctx.transport().unwrap();
    let mut inbox = ctx.inbox("service").unwrap();
    let wake = ctx.cap::<Notification>("wake").unwrap();
    let mut handler = CounterHandler { value: 0, wake };
    for _ in 0..2 {
        let (_, request, reply) = inbox
            .receive()
            .decode(
                &mut io.space,
                counter_service_request_layout,
                CounterServiceRequest::decode,
            )
            .unwrap();
        let response = counter_service_dispatch(request, &mut handler);
        counter_service_reply(reply, &mut io.space, &response).unwrap();
    }
    sel4::cap::Notification::from_bits(wake.as_ref().slot as _).signal();
    launch_probe::run(&ctx, &mut io);
    vm_probe::run(&mut ctx, &mut io);
    let process = ctx
        .object::<horn_procd::protocol::ProcessSelfTag>("horn.lifecycle")
        .unwrap();
    assert!(process.info(&mut io).unwrap().charged_bytes > 0);
    process.exit(&mut io, 0).unwrap();
    unreachable!()
}
