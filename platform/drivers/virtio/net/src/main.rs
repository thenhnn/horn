//! Modern virtio-net: Ethernet only, no IP or configuration authority.
#![no_std]
#![no_main]
extern crate alloc;
mod hardware;
mod service;
use horn_driver::{DeviceSession, virtio};
use horn_net::protocol::*;
horn_runtime::task!(main);

fn main(mut ctx: horn_runtime::Context) -> ! {
    let io = ctx.transport().unwrap();
    let mut inbox = ctx.inbox("devmgr.service").unwrap();
    virtio::install(DeviceSession::new(
        io,
        ctx.cap("horn.vm").unwrap(),
        ctx.cap("horn.address-space").unwrap(),
        ctx.cap("devmgr.device").unwrap(),
    ));
    let (transport, common) =
        virtio::transport(horn_virtio::virtio_drivers::transport::DeviceType::Network);
    virtio::with(|s| {
        s.interrupt_on(
            &ctx.cap("devmgr.irq_signal").unwrap(),
            ctx.cap("devmgr.wake").unwrap(),
        );
        s.activate();
    });
    let mut hardware = Some(hardware::Hardware::new(transport, common));
    let mut service = service::Service::new(hardware.as_ref().unwrap().mac());
    ctx.notify_ready().unwrap();
    loop {
        if let Some(hw) = &mut hardware {
            if hw.complete() {
                service.changed();
            }
            while let Some(frame) = hw.receive() {
                if !frame.is_empty() && service.rx.len() < 64 {
                    service.rx.push_back(frame);
                    service.changed();
                }
            }
            if hw.writable() {
                if let Some(frame) = service.tx.pop_front() {
                    hw.send(&frame);
                    service.changed();
                }
            }
        }
        service.notify();
        let mut received = inbox.receive();
        let badge = received.badge();
        if badge < 16 && badge & (horn_supervisor::IRQ | horn_supervisor::STOP) != 0 {
            if badge & horn_supervisor::STOP != 0 {
                service.closed = true;
                service.notify();
                if let Some(hw) = hardware.take() {
                    hw.shutdown();
                    let stopped = ctx.cap::<horn_ipc::Notification>("devmgr.stopped").unwrap();
                    sel4::cap::Notification::from_bits(stopped.as_ref().slot as _).signal();
                }
            }
            if badge & horn_supervisor::IRQ != 0 {
                if let Some(hw) = &mut hardware {
                    hw.interrupt();
                    virtio::with(|s| s.ack_interrupt());
                }
            }
            continue;
        }
        if badge != 16 {
            continue;
        }
        let request = virtio::with(|s| {
            received.decode(
                &mut s.transport().space,
                ethernet_object_request_layout,
                EthernetObjectRequest::decode,
            )
        });
        if let Ok((_, request, reply)) = request {
            let response = ethernet_object_dispatch(request, &mut service);
            virtio::with(|s| {
                ethernet_object_reply(reply, &mut s.transport().space, &response).unwrap()
            });
        }
    }
}
