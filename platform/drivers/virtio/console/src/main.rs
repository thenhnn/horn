//! Single-port console driver. Device assignment/IRQ/IOMMU policy is in devmgr.
#![no_std]
#![no_main]
extern crate alloc;
mod hal;
use core::ptr::NonNull;
use horn_devmgr::protocol::*;
#[cfg(feature = "dma-probe")]
use horn_ipc::CallError;
use horn_virtio::{
    IommuPciTransport, VirtIOConsole,
    config::ConfigSnapshot,
    virtio_drivers::transport::{
        DeviceType, Transport,
        pci::{
            PciTransport,
            bus::{ConfigurationAccess, DeviceFunction, PciRoot},
        },
    },
};
const SLOTS: horn_runtime::TaskSlots = horn_runtime::TaskSlots::new(
    DEVICE_DRIVER_CAPS_FIRST_FREE_SLOT,
    DEVICE_DRIVER_CAPS_SIZE_BITS,
);
horn_runtime::task!(main, vm: horn_runtime::VmBootstrap {
    cnode: DEVICE_DRIVER_CAPS_SELF_SLOT,
    factory: DEVICE_DRIVER_CAPS_VM_SLOT,
    address_space: DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,
    slots: SLOTS,
});

fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _);
    let transport = horn_runtime::transport(root, SLOTS.application()).expect("driver transport");
    let session = horn_driver::DeviceSession::new(
        transport,
        hal::target(DEVICE_DRIVER_CAPS_VM_SLOT),
        hal::target(DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT),
        hal::target(DEVICE_DRIVER_CAPS_DEVICE_SLOT),
    );
    hal::install(hal::State::new(session));
    #[cfg(feature = "dma-probe")]
    hal::with(|state| {
        // Kernel-unwrapped caps for devmgr's endpoint are NOT VM capabilities.
        let wrong_memory = hal::target::<VmMemoryObjectTag>(DEVICE_DRIVER_CAPS_DEVICE_SLOT);
        assert!(matches!(
            state.session.control().map(&wrong_memory, 0, 4096, 3),
            Err(CallError::Remote(DeviceError::Denied))
        ));
        let wrong_space = hal::target::<VmAddressSpaceObjectTag>(DEVICE_DRIVER_CAPS_DEVICE_SLOT);
        assert!(matches!(
            state.session.control().bar(4, &wrong_space),
            Err(CallError::Remote(DeviceError::Denied))
        ));
        sel4::debug_println!("DEVICE_CAP_CHECK_PASS wrong_vm_object=denied");
    });
    let resources = hal::with(|state| {
        let resources = state
            .session
            .control()
            .resources()
            .expect("assigned PCI resources");
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
    let notification = hal::with(|state| {
        let notification = state.session.interrupt_on(&hal::target(DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT),hal::target(DEVICE_DRIVER_CAPS_WAKE_SLOT));
        #[cfg(feature = "dma-probe")]
        {
            state.fault_wait = Some(notification);
        }
        state.session.activate();
        notification
    });
    let mut console =
        VirtIOConsole::<hal::CapabilityHal, _>::new(transport).expect("console queues");
    #[cfg(feature="lifecycle-test")]
    hal::probe_for_test(); // Must pass before the host sees a readiness banner.
    console
        .send_bytes(b"HORN_VIRTIO_CONSOLE_READY\n")
        .expect("console transmit");
    sel4::debug_println!(
        "VIRTIO_CONSOLE_READY allocator=mimalloc transport=upstream dma=iova irq=msix"
    );
    horn_runtime::notify_ready(DEVICE_DRIVER_CAPS_READY_SLOT);
    #[cfg(feature = "lifecycle-test")]
    match option_env!("HORN_DRIVER_TEST") {
        Some("fault") => unsafe {core::arch::asm!("ud2",options(noreturn));},
        Some("exit") => { hal::exit_for_test(); }
        Some(mode @ ("orderly" | "ignore" | "force")) => { hal::stop_for_test(mode=="force"); }
        _=>{}
    }
    #[cfg(feature = "dma-probe")]
    if option_env!("HORN_DMA_PROBE") == Some("stale") {
        // The readiness TX warmed a real translation; HAL has since acknowledged
        // its unmap/drain and released that backing. Reuse ONLY its stale IOVA.
        console
            .send_bytes(b"THIS_STALE_DMA_MUST_NOT_REACH_HOST")
            .expect("stale probe submit");
    }
    let mut irq_reported = false;
    loop {
        let (_,badge) = notification.wait();
        if badge & horn_supervisor::STOP != 0 {
            #[cfg(feature = "lifecycle-test")]
            if option_env!("HORN_DRIVER_TEST") == Some("ignore") { continue; }
            // queue_unset in upstream PCI is a no-op. Explicit protocol reset
            // and status-zero acknowledgment must precede all upstream drops.
            let status=unsafe {common.as_ptr().add(20)};
            unsafe {status.write_volatile(0);}
            let mut reset=false;
            for _ in 0..1_000_000 {
                if unsafe {status.read_volatile()}==0 {reset=true;break;}
                core::hint::spin_loop();
            }
            assert!(reset,"virtio reset timeout: retain backing");
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            drop(console);
            unsafe {hal::with(|state|state.session.discard_shared());}
            sel4::debug_println!("VIRTIO_CONSOLE_STOPPED reset=true dma_closed=true");
            sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_STOPPED_SLOT as _).signal();
            loop {notification.wait();}
        }
        console.ack_interrupt().expect("console interrupt");
        while let Some(byte) = console.recv(true).expect("console receive") {
            console.send(byte).expect("console echo");
        }
        hal::with(|state| state.session.ack_interrupt());
        if !irq_reported {
            sel4::debug_println!("VIRTIO_CONSOLE_IRQ delivered=true");
            irq_reported = true;
        }
    }
}
