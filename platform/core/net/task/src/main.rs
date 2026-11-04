//! DHCP/IPv4/TCP policy above a capability-scoped Ethernet device.
#![no_std]
#![no_main]
extern crate alloc;
mod ethernet;
mod tcp;
use alloc::vec;
use horn_clock::{AlarmInterfaceClient, ClockInterfaceClient};
use horn_ipc::*;
use horn_net::protocol::*;
use smoltcp::{
    iface::{Config, Interface, SocketSet},
    socket::dhcpv4,
    time::Instant,
    wire::{EthernetAddress, IpCidr},
};
horn_runtime::task!(main);
type Io = Sel4Transport<Sel4CapSpace>;
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let mut inbox = ctx.inbox("service").unwrap();
    let ethernet = ctx.cap("ethernet").unwrap();
    let ethernet_signal = ctx.cap::<Notification>("ethernet_signal").unwrap();
    let timer_signal = ctx.cap("timer_signal").unwrap();
    let time = ctx.clock(&mut io, "clock").unwrap();
    let alarm = time.capability().alarm(&mut io).unwrap();
    let mut armed = None;
    let info = EthernetInterface::from_ref(ethernet.as_ref())
        .info(&mut io)
        .unwrap();
    assert!(info.mac.len == 6 && info.mtu == 1500);
    let mut device = ethernet::Device::new(ethernet);
    let mut config = Config::new(EthernetAddress(info.mac.data).into());
    config.random_seed = time.now(&mut io).unwrap()
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
        Instant::from_micros(time.now(&mut io).unwrap() as i64),
    );
    let mut sockets = SocketSet::new(vec![]);
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let mut service = tcp::Service::new(ctx, io, sockets);
    let mut ready = false;
    loop {
        device.receive_rpc(&mut service.io);
        let now = time.now(&mut service.io).unwrap();
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
        EthernetInterface::from_ref(ethernet.as_ref())
            .watch(&mut service.io, &ethernet_signal, device.generation)
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
