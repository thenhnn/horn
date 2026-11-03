//! Single-port console driver. Device assignment/IRQ/IOMMU policy is in devmgr.
#![no_std]
#![no_main]
extern crate alloc;
mod hal;
mod service;
use core::ptr::NonNull;
use horn_console::protocol as c;
use horn_devmgr::protocol::*;
use horn_ipc::*;
use horn_virtio::{
    IommuPciTransport,
    config::ConfigSnapshot,
    virtio_drivers::transport::{
        DeviceType, Transport,
        pci::{
            PciTransport,
            bus::{ConfigurationAccess, DeviceFunction, PciRoot},
        },
    },
};
horn_runtime::task!(main);

fn main(mut ctx: horn_runtime::Context) -> ! {
    let transport = ctx.transport().expect("driver transport");
    let mut inbox = ctx.inbox("devmgr.service").unwrap();
    let session = horn_driver::DeviceSession::new(
        transport,
        ctx.cap("horn.vm").unwrap(),
        ctx.cap("horn.address-space").unwrap(),
        ctx.cap("devmgr.device").unwrap(),
    );
    hal::install(hal::State::new(
        session,
        ctx.cap("devmgr.supervisor").unwrap(),
        ctx.cap("horn.lifecycle").unwrap(),
    ));
    #[cfg(feature = "dma-probe")]
    hal::with(|state| {
        let _now = ctx
            .interface::<ClockInterfaceReceiver>("devmgr.clock")
            .unwrap()
            .now(state.session.transport())
            .expect("named monotonic clock");
        // Kernel-unwrapped caps for devmgr's endpoint are NOT VM capabilities.
        let wrong_memory = ctx.cap::<VmMemoryObjectTag>("devmgr.device").unwrap();
        assert!(matches!(
            state
                .session
                .control()
                .map(state.session.transport(), &wrong_memory, 0, 4096, 3),
            Err(CallError::Remote(DeviceError::Denied))
        ));
        let wrong_space = ctx.cap::<VmAddressSpaceObjectTag>("devmgr.device").unwrap();
        assert!(matches!(
            state
                .session
                .control()
                .bar(state.session.transport(), 4, &wrong_space),
            Err(CallError::Remote(DeviceError::Denied))
        ));
        sel4::debug_println!("DEVICE_CAP_CHECK_PASS wrong_vm_object=denied");
    });
    let resources = hal::with(|state| {
        let resources = state
            .session
            .control()
            .resources(state.session.transport())
            .expect("assigned PCI resources");
        #[cfg(feature = "dma-probe")]
        {
            // Exercise reusable broker/VM/cap slots without touching registers.
            let bar = resources
                .bar_lengths
                .data
                .iter()
                .position(|n| *n >= 4096)
                .expect("test memory BAR") as u32;
            let space = ctx
                .cap::<VmAddressSpaceObjectTag>("horn.address-space")
                .unwrap();
            assert!(matches!(
                DeviceMmioReceiver::from_cap(ctx.cap("devmgr.device").unwrap())
                    .close(state.session.transport(),),
                Err(CallError::Remote(DeviceError::Denied))
            ));
            // Closing an unopened lease must also retire its server-held memory.
            let unopened = state
                .session
                .control()
                .map_bar_window(state.session.transport(), bar, 0, 4096, &space)
                .unwrap();
            unopened
                .interface::<DeviceMmioReceiver>()
                .close(state.session.transport())
                .unwrap();
            drop(unopened);
            let lease = state
                .session
                .control()
                .map_bar_window(state.session.transport(), bar, 0, 4096, &space)
                .unwrap();
            let memory = lease
                .interface::<DeviceMmioReceiver>()
                .memory(state.session.transport())
                .unwrap();
            assert!(matches!(
                lease
                    .interface::<DeviceMmioReceiver>()
                    .memory(state.session.transport()),
                Err(CallError::Remote(DeviceError::NotFound))
            ));
            lease
                .interface::<DeviceMmioReceiver>()
                .close(state.session.transport())
                .unwrap();
            drop(memory);
            drop(lease);
            let mut windows = alloc::vec::Vec::new();
            for _ in 0..8 {
                windows.push(state.session.mmio_window(bar, 0, 4096));
            }
            assert!(matches!(
                state.session.control().map_bar_window(
                    state.session.transport(),
                    bar,
                    0,
                    4096,
                    &space
                ),
                Err(CallError::Remote(DeviceError::Exhausted))
            ));
            for window in windows {
                state.session.close_window(window);
            }
            for _ in 0..16 {
                let window = state.session.mmio_window(bar, 0, 4096);
                assert_eq!(window.length(), 4096);
                state.session.close_window(window);
            }
            sel4::debug_println!("DEVICE_MMIO_LEASE_PASS quota=8 cycles=16 scoped=true");
        }
        state.session.map_bars(&resources);
        resources
    });
    assert_eq!(resources.address.segment, 0);
    let function = DeviceFunction {
        bus: resources.address.bus.try_into().unwrap(),
        device: resources.address.device.try_into().unwrap(),
        function: resources.address.function.try_into().unwrap(),
    };
    let snapshot = ConfigSnapshot::new(
        function,
        resources.configuration.data,
        resources.bar_lengths.data,
    );
    let mut pci = PciRoot::new(snapshot.clone());
    // Upstream handles all virtio PCI capabilities/transport. Only locate the
    // common region again for the small ACCESS_PLATFORM/MSI-X adapter.
    let capability = pci
        .capabilities(function)
        .find(|cap| cap.id == 9 && cap.private_header as u8 >= 16 && cap.private_header >> 8 == 1)
        .expect("virtio common capability");
    assert!(capability.offset <= 240);
    let bar = snapshot.read_word(function, capability.offset + 4) as u8;
    let offset = snapshot.read_word(function, capability.offset + 8) as u64;
    let length = snapshot.read_word(function, capability.offset + 12) as usize;
    let (base, size) = pci
        .bar_info(function, bar)
        .unwrap()
        .unwrap()
        .memory_address_size()
        .unwrap();
    assert!(
        offset
            .checked_add(length as u64)
            .is_some_and(|end| end <= size)
    );
    let common: NonNull<u8> = hal::with(|state| {
        state
            .session
            .mmio(base.checked_add(offset).unwrap(), length)
    });
    let transport = PciTransport::new::<hal::CapabilityHal, _>(&mut pci, function)
        .expect("virtio PCI transport");
    assert_eq!(transport.device_type(), DeviceType::Console);
    // The first provisioning profile supplies MSI-X table entry zero.
    let transport = unsafe { IommuPciTransport::new(transport, common, length, 0) }
        .expect("IOMMU virtio transport");
    hal::with(|state| {
        let notification = state.session.interrupt_on(
            &ctx.cap("devmgr.irq_signal").unwrap(),
            ctx.cap("devmgr.wake").unwrap(),
        );
        #[cfg(feature = "dma-probe")]
        {
            state.fault_wait = Some(notification);
        }
        state.session.activate();
        notification
    });
    let mut console = Some(
        horn_virtio::console::Console::<hal::CapabilityHal, _>::new(transport)
            .expect("console queues"),
    );
    let mut service = service::Service::new();
    #[cfg(feature = "lifecycle-test")]
    hal::probe_for_test(); // Must pass before the host sees a readiness banner.

    ctx.notify_ready().unwrap();
    #[cfg(feature = "lifecycle-test")]
    match option_env!("HORN_DRIVER_TEST") {
        Some("fault") => unsafe {
            core::arch::asm!("ud2", options(noreturn));
        },
        Some("exit") => {
            hal::exit_for_test();
        }
        Some(mode @ ("orderly" | "ignore" | "force")) => {
            hal::stop_for_test(mode == "force");
        }
        _ => {}
    }
    let mut irq_reported = false;
    loop {
        if let Some(hw) = &mut console {
            pump(hw, &mut service);
        }
        service.notify();
        let mut received = inbox.receive();
        let badge = received.badge();
        if badge & (horn_supervisor::IRQ | horn_supervisor::STOP) != 0
            && badge < horn_console::CLIENT_BADGE
        {
            let ignore = cfg!(feature = "lifecycle-test")
                && option_env!("HORN_DRIVER_TEST") == Some("ignore");
            if badge & horn_supervisor::STOP != 0 && !ignore {
                service.console.close();
                service.notify();
                if let Some(hw) = console.take() {
                    unsafe {
                        hw.shutdown(|| hal::with(|s| s.session.discard_shared()));
                    }
                    horn_runtime::logging::info!("Console stopped");
                    let stopped = ctx.cap::<Notification>("devmgr.stopped").unwrap();
                    sel4::cap::Notification::from_bits(stopped.as_ref().slot as _).signal();
                }
            }
            if badge & horn_supervisor::IRQ != 0 && console.is_some() {
                console.as_mut().unwrap().interrupt().expect("console IRQ");
                hal::with(|s| s.session.ack_interrupt());
                if !irq_reported {
                    horn_runtime::logging::debug!("Console interrupt received");
                    irq_reported = true;
                }
            }
            continue;
        }
        if badge != horn_console::CLIENT_BADGE {
            continue;
        }
        let request = hal::with(|s| {
            received.decode(
                &mut s.session.transport().space,
                c::console_object_request_layout,
                c::ConsoleObjectRequest::decode,
            )
        });
        if let Ok((_, request, reply)) = request {
            let response = c::console_object_dispatch(request, &mut service);
            hal::with(|s| {
                c::console_object_reply(reply, &mut s.session.transport().space, &response).unwrap()
            });
        }
    }
}
fn pump(
    hw: &mut horn_virtio::console::Console<hal::CapabilityHal, IommuPciTransport<PciTransport>>,
    service: &mut service::Service,
) {
    hw.poll().expect("console completions");
    let size = hw
        .size()
        .expect("console geometry")
        .map(|(rows, columns)| c::ConsoleSize { rows, columns });
    service.console.set_size(size).expect("console size");
    let mut bytes = [0u8; 256];
    while service.console.rx_space() != 0 {
        let limit = service.console.rx_space().min(bytes.len());
        let count = hw.receive(&mut bytes[..limit]).expect("console receive");
        if count == 0 {
            break;
        }
        assert_eq!(service.console.receive(&bytes[..count]), count);
    }
    if hw.writable() {
        let count = service.console.transmit(&mut bytes);
        if count != 0 {
            hw.send(&bytes[..count]).expect("console transmit");
        }
    }
}
