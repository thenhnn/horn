//! DHCP/IPv4/TCP policy above a capability-scoped Ethernet device.
#![no_std]
#![no_main]
extern crate alloc;
mod ethernet;
mod tcp;
use alloc::vec;
use horn_clock::{ClockPage, protocol as c};
use horn_ipc::*;
use horn_net::protocol::*;
use horn_vm::protocol as v;
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    socket::dhcpv4,
    time::Instant,
    wire::{EthernetAddress, IpCidr},
};
const SLOTS: TaskSlots = TaskSlots::new(NETD_CAPS_FIRST_FREE_SLOT, NETD_CAPS_SIZE_BITS);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{cnode:NETD_CAPS_SELF_SLOT,factory:NETD_CAPS_VM_SLOT,address_space:NETD_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS});
type Io = Sel4Transport<Sel4CapSpace>;
fn root() -> sel4::cap::CNode {
    sel4::cap::CNode::from_bits(NETD_CAPS_SELF_SLOT as _)
}
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(root(), slot)
}
struct Time {
    page: *const ClockPage,
    _memory: Cap<v::VmMemoryObjectTag>,
    _region: Cap<v::VmRegionObjectTag>,
}
impl Time {
    fn new(io: &mut Io) -> Self {
        let page = ClockPage::from(
            c::ClockInterfaceClient::new(&mut *io, cap(NETD_CAPS_CLOCK_SLOT))
                .calibration()
                .unwrap(),
        );
        assert!(page.version == 1 && page.flags & !1 == 0 && page.span_cycles != 0);
        let memory = v::VmFactoryClient::new(&mut *io, cap(NETD_CAPS_VM_SLOT))
            .allocate(4096, v::VmBacking::Resident, v::VmProtection(3))
            .unwrap();
        let writable = v::VmAddressSpaceClient::new(&mut *io, cap(NETD_CAPS_ADDRESS_SPACE_SLOT))
            .reserve(v::VmRegionRequest {
                address: 0,
                length: 4096,
                alignment: 4096,
                placement: v::VmPlacement::Anywhere,
                ceiling: v::VmProtection(3),
            })
            .unwrap();
        let address = v::VmRegionClient::new(&mut *io, Cap::from_ref(writable.as_ref()))
            .info()
            .unwrap()
            .address;
        let mut updates = List::default();
        updates.len = 2;
        updates.data[0] = v::VmUpdate::Prepare(v::VmRange {
            offset: 0,
            length: 4096,
        });
        updates.data[1] = v::VmUpdate::Map(v::VmMap {
            offset: 0,
            length: 4096,
            memory_offset: 0,
            protection: v::VmProtection(3),
        });
        v::VmRegionClient::new(&mut *io, Cap::from_ref(writable.as_ref()))
            .apply(
                Some(Cap::from_ref(memory.as_ref())),
                Parcel::inline(updates),
            )
            .unwrap();
        unsafe {
            (address as *mut ClockPage).write(page);
        }
        v::VmRegionClient::new(&mut *io, Cap::from_ref(writable.as_ref()))
            .destroy()
            .unwrap();
        io.space.delete(writable.as_ref()).unwrap();
        v::VmMemoryClient::new(&mut *io, Cap::from_ref(memory.as_ref()))
            .add_seals(v::VmSeals(15))
            .unwrap();
        let region = v::VmAddressSpaceClient::new(&mut *io, cap(NETD_CAPS_ADDRESS_SPACE_SLOT))
            .reserve(v::VmRegionRequest {
                address: 0,
                length: 4096,
                alignment: 4096,
                placement: v::VmPlacement::Anywhere,
                ceiling: v::VmProtection::READ,
            })
            .unwrap();
        let address = v::VmRegionClient::new(&mut *io, Cap::from_ref(region.as_ref()))
            .info()
            .unwrap()
            .address;
        let mut updates = List::default();
        updates.len = 2;
        updates.data[0] = v::VmUpdate::Prepare(v::VmRange {
            offset: 0,
            length: 4096,
        });
        updates.data[1] = v::VmUpdate::Map(v::VmMap {
            offset: 0,
            length: 4096,
            memory_offset: 0,
            protection: v::VmProtection::READ,
        });
        v::VmRegionClient::new(&mut *io, Cap::from_ref(region.as_ref()))
            .apply(
                Some(Cap::from_ref(memory.as_ref())),
                Parcel::inline(updates),
            )
            .unwrap();
        Self {
            page: address as *const ClockPage,
            _memory: memory,
            _region: region,
        }
    }
    fn now(&self, io: &mut Io) -> u64 {
        unsafe { &*self.page }.fast_now().unwrap_or_else(|| {
            c::ClockInterfaceClient::new(io, cap(NETD_CAPS_CLOCK_SLOT))
                .now()
                .unwrap()
        })
    }
}
fn main() -> ! {
    let mut io = horn_runtime::transport(root(), SLOTS.application()).unwrap();
    let time = Time::new(&mut io);
    let alarm = c::ClockInterfaceClient::new(&mut io, cap(NETD_CAPS_CLOCK_SLOT))
        .alarm()
        .unwrap();
    let mut armed = None;
    let info = EthernetInterfaceClient::new(&mut io, cap(NETD_CAPS_ETHERNET_SLOT))
        .info()
        .unwrap();
    assert!(info.mac.len == 6 && info.mtu == 1500);
    let mut device = ethernet::Device::new();
    let mut config = Config::new(EthernetAddress(info.mac.data).into());
    config.random_seed = time.now(&mut io)
        ^ u64::from_le_bytes([
            info.mac.data[0],
            info.mac.data[1],
            info.mac.data[2],
            info.mac.data[3],
            info.mac.data[4],
            info.mac.data[5],
            0,
            0,
        ]);
    let mut iface = Interface::new(
        config,
        &mut device,
        Instant::from_micros(time.now(&mut io) as i64),
    );
    let mut sockets = SocketSet::new(vec![]);
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let mut service = tcp::Service::new(io, sockets);
    let mut ready = false;
    let endpoint = sel4::cap::Endpoint::from_bits(NETD_CAPS_SERVICE_SLOT as _);
    let reply = sel4::cap::Reply::from_bits(NETD_CAPS_REPLY_SLOT as _);
    let receive = root().absolute_cptr(sel4::CPtr::from_bits(SLOTS.public_receive() as _));
    loop {
        device.receive_rpc(&mut service.io);
        let now = time.now(&mut service.io);
        let timestamp = Instant::from_micros(now as i64);
        // Replenish a listener between SYNs, including packets from one burst.
        // Work stays bounded by the local Ethernet ingress queue.
        service.maintain();
        for _ in 0..16 {
            if iface.poll_ingress_single(timestamp, &mut device, &mut service.sockets)
                == smoltcp::iface::PollIngressSingleResult::None
            {
                break;
            }
            service.maintain();
        }
        iface.poll_egress(timestamp, &mut device, &mut service.sockets);
        match service.sockets.get_mut::<dhcpv4::Socket>(dhcp).poll() {
            Some(dhcpv4::Event::Configured(config)) => {
                iface.update_ip_addrs(|a| {
                    a.clear();
                    a.push(IpCidr::Ipv4(config.address)).unwrap();
                });
                iface.routes_mut().remove_default_ipv4_route();
                if let Some(router) = config.router {
                    iface.routes_mut().add_default_ipv4_route(router).unwrap();
                }
                sel4::debug_println!("NETD_IPV4 address={} source=dhcp", config.address);
                if !ready {
                    ready = true;
                    horn_runtime::notify_ready(NETD_CAPS_READY_SLOT);
                }
            }
            Some(dhcpv4::Event::Deconfigured) => {
                iface.update_ip_addrs(|a| a.clear());
                iface.routes_mut().remove_default_ipv4_route();
                sel4::debug_println!("NETD_IPV4 deconfigured");
            }
            None => {}
        }
        service.maintain();
        device.transmit_rpc(&mut service.io);
        // A full RX work budget does not prove the driver's queue is empty;
        // TX RPCs may also have unblocked already-imported RX frames. Neither
        // needs a new device IRQ. A bounded-work hint drains them, and stops
        // once WouldBlock is observed or the local queues cannot make progress.
        if device.local_work() {
            sel4::cap::Notification::from_bits(NETD_CAPS_ETHERNET_SIGNAL_SLOT as _).signal();
        }
        EthernetInterfaceClient::new(&mut service.io, cap(NETD_CAPS_ETHERNET_SLOT))
            .watch(&cap(NETD_CAPS_ETHERNET_SIGNAL_SLOT), device.generation)
            .unwrap();
        let deadline = iface.poll_delay(timestamp, &service.sockets).and_then(|d| {
            if d.total_micros() == 0 && device.pending_tx() {
                None
            } else {
                Some(now.saturating_add(d.total_micros()))
            }
        });
        if deadline != armed {
            let mut timer =
                c::AlarmInterfaceClient::new(&mut service.io, Cap::from_ref(alarm.as_ref()));
            if let Some(d) = deadline {
                timer.arm(d, &cap(NETD_CAPS_TIMER_SIGNAL_SLOT)).unwrap();
            } else {
                timer.cancel().unwrap();
            }
            armed = deadline;
        }
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive));
        let (info, badge) = endpoint.recv(reply);
        if badge < 16 && badge & 3 != 0 {
            if badge & 2 != 0 {
                armed = None;
            }
            receive.delete().unwrap();
            continue;
        }
        let request = {
            let mut server = Sel4Server::new(root(), endpoint, receive, &mut service.io.space);
            server.decode_received_dynamic(
                info,
                badge,
                reply,
                tcp_host_request_layout,
                TcpHostRequest::decode,
            )
        };
        if let Ok((_, request, reply)) = request {
            let response = tcp_host_dispatch(
                request,
                &mut tcp::Caller {
                    service: &mut service,
                    badge,
                },
            );
            tcp_host_reply(reply, &mut service.io.space, &response).unwrap();
        }
        receive.delete().unwrap();
    }
}
