//! Modern virtio-net: Ethernet only, no IP or configuration authority.
#![no_std]
#![no_main]
extern crate alloc;
mod hardware;
mod service;
use horn_devmgr::protocol::*;
use horn_driver::{DeviceSession, virtio};
use horn_ipc::*;
use horn_net::protocol::*;
const SLOTS: TaskSlots = TaskSlots::new(
    DEVICE_DRIVER_CAPS_FIRST_FREE_SLOT,
    DEVICE_DRIVER_CAPS_SIZE_BITS,
);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{cnode:DEVICE_DRIVER_CAPS_SELF_SLOT,factory:DEVICE_DRIVER_CAPS_VM_SLOT,address_space:DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS});
fn root() -> sel4::cap::CNode {
    sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _)
}
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(root(), slot)
}
fn main() -> ! {
    let io = horn_runtime::transport(root(), SLOTS.application()).unwrap();
    virtio::install(DeviceSession::new(
        io,
        cap(DEVICE_DRIVER_CAPS_VM_SLOT),
        cap(DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT),
        cap(DEVICE_DRIVER_CAPS_DEVICE_SLOT),
    ));
    let (transport, common) =
        virtio::transport(horn_virtio::virtio_drivers::transport::DeviceType::Network);
    virtio::with(|s| {
        s.interrupt_on(
            &cap(DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT),
            cap(DEVICE_DRIVER_CAPS_WAKE_SLOT),
        );
        s.activate();
    });
    let mut hardware = Some(hardware::Hardware::new(transport, common));
    let mut service = service::Service::new(hardware.as_ref().unwrap().mac());
    sel4::debug_println!("VIRTIO_NET_READY transport=upstream dma=iova irq=msix");
    horn_runtime::notify_ready(DEVICE_DRIVER_CAPS_READY_SLOT);
    let endpoint = sel4::cap::Endpoint::from_bits(DEVICE_DRIVER_CAPS_SERVICE_SLOT as _);
    let reply = sel4::cap::Reply::from_bits(DEVICE_DRIVER_CAPS_REPLY_SLOT as _);
    let receive = root().absolute_cptr(sel4::CPtr::from_bits(SLOTS.public_receive() as _));
    loop {
        if let Some(hw) = &mut hardware {
            if hw.complete() {
                service.changed();
            }
            while let Some(frame) = hw.receive() {
                if !frame.is_empty() && service.rx.len() < 8 {
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
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive));
        let (info, badge) = endpoint.recv(reply);
        if badge < 16 && badge & (horn_supervisor::IRQ | horn_supervisor::STOP) != 0 {
            receive.delete().unwrap();
            if badge & horn_supervisor::STOP != 0 {
                service.closed = true;
                service.notify();
                if let Some(hw) = hardware.take() {
                    hw.shutdown();
                    sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_STOPPED_SLOT as _)
                        .signal();
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
            receive.delete().unwrap();
            continue;
        }
        let request = virtio::with(|s| {
            let mut server = Sel4Server::new(root(), endpoint, receive, &mut s.transport().space);
            server.decode_received_dynamic(
                info,
                badge,
                reply,
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
        receive.delete().unwrap();
    }
}
