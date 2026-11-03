//! DHCP/IPv4/TCP policy above a capability-scoped Ethernet device.
#![no_std]
#![no_main]
extern crate alloc;
mod ethernet;
mod tcp;
use alloc::vec;
use horn_clock::{Alarm, Clock, DefaultTimeManager, DefaultTimePage, TimeManager};
use horn_ipc::*;
use horn_net::protocol::*;
use horn_vm::protocol as v;
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    socket::dhcpv4,
    time::Instant,
    wire::{EthernetAddress, IpCidr},
};
horn_runtime::task!(main);
type Io = Sel4Transport<Sel4CapSpace>;
struct Time {
    page: *const DefaultTimePage,
    clock: Clock,
    _memory: Cap<v::VmMemoryObjectTag>,
    _region: Cap<v::VmRegionObjectTag>,
}
impl Time {
    fn new(io: &mut Io, ctx: &horn_runtime::Context) -> Self {
        let clock = ctx.interface::<Clock>("clock").unwrap();
        let page = DefaultTimePage::from(clock.calibration(io).unwrap());
        let memory = v::VmFactoryClient::new(&mut *io, ctx.cap("horn.vm").unwrap())
            .allocate(4096, v::VmBacking::Resident, v::VmProtection(3))
            .unwrap();
        let writable =
            v::VmAddressSpaceClient::new(&mut *io, ctx.cap("horn.address-space").unwrap())
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
            (address as *mut DefaultTimePage).write(page);
        }
        v::VmRegionClient::new(&mut *io, Cap::from_ref(writable.as_ref()))
            .destroy()
            .unwrap();
        io.space.delete(writable.as_ref()).unwrap();
        v::VmMemoryClient::new(&mut *io, Cap::from_ref(memory.as_ref()))
            .add_seals(v::VmSeals(15))
            .unwrap();
        let region = v::VmAddressSpaceClient::new(&mut *io, ctx.cap("horn.address-space").unwrap())
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
            page: address as *const DefaultTimePage,
            clock,
            _memory: memory,
            _region: region,
        }
    }
    fn now(&self, io: &mut Io) -> u64 {
        DefaultTimeManager::now(unsafe { &*self.page })
            .unwrap_or_else(|| self.clock.now(io).unwrap())
    }
}
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let mut inbox = ctx.inbox("service").unwrap();
    let ethernet = ctx.cap("ethernet").unwrap();
    let ethernet_signal = ctx.cap::<Notification>("ethernet_signal").unwrap();
    let timer_signal = ctx.cap("timer_signal").unwrap();
    let time = Time::new(&mut io, &ctx);
    let alarm_cap = time.clock.alarm(&mut io).unwrap();
    let alarm = Alarm::from_cap(Cap::from_ref(alarm_cap.as_ref()));
    let mut armed = None;
    let info = EthernetInterfaceClient::new(&mut io, ethernet)
        .info()
        .unwrap();
    assert!(info.mac.len == 6 && info.mtu == 1500);
    let mut device = ethernet::Device::new(ethernet);
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
    let mut service = tcp::Service::new(ctx, io, sockets);
    let mut ready = false;
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
                horn_runtime::logging::info!("DHCP assigned {}", config.address);
                if !ready {
                    ready = true;
                    service.ctx.notify_ready().unwrap();
                }
            }
            Some(dhcpv4::Event::Deconfigured) => {
                iface.update_ip_addrs(|a| a.clear());
                iface.routes_mut().remove_default_ipv4_route();
                horn_runtime::logging::warn!("DHCP configuration lost");
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
            sel4::cap::Notification::from_bits(ethernet_signal.as_ref().slot as _).signal();
        }
        EthernetInterfaceClient::new(&mut service.io, ethernet)
            .watch(&ethernet_signal, device.generation)
            .unwrap();
        let deadline = iface.poll_delay(timestamp, &service.sockets).and_then(|d| {
            if d.total_micros() == 0 && device.pending_tx() {
                None
            } else {
                Some(now.saturating_add(d.total_micros()))
            }
        });
        if deadline != armed {
            if let Some(d) = deadline {
                alarm.arm(&mut service.io, d, &timer_signal).unwrap();
            } else {
                alarm.cancel(&mut service.io).unwrap();
            }
            armed = deadline;
        }
        let mut received = inbox.receive();
        let badge = received.badge();
        if badge < 16 && badge & 3 != 0 {
            if badge & 2 != 0 {
                armed = None;
            }
            continue;
        }
        let request = received.decode(
            &mut service.io.space,
            tcp_host_request_layout,
            TcpHostRequest::decode,
        );
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
    }
}
