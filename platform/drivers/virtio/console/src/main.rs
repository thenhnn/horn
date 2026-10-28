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
horn_runtime::task!(main, vm: horn_runtime::VmBootstrap {
    cnode: DEVICE_DRIVER_CAPS_SELF_SLOT,
    factory: DEVICE_DRIVER_CAPS_VM_SLOT,
    address_space: DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,
    slots: 128..256,
});

fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(DEVICE_DRIVER_CAPS_SELF_SLOT as _);
    let transport = horn_runtime::transport(root, DEVICE_DRIVER_CAPS_FIRST_FREE_SLOT..128)
        .expect("driver transport");
    hal::install(hal::State::new(transport));
    #[cfg(feature = "dma-probe")]
    hal::with(|state| {
        // Kernel-unwrapped caps for devmgr's endpoint are NOT VM capabilities.
        let wrong_memory = hal::target::<VmMemoryObjectTag>(DEVICE_DRIVER_CAPS_DEVICE_SLOT);
        assert!(matches!(
            state.device().map(&wrong_memory, 0, 4096, 3),
            Err(CallError::Remote(DeviceError::Denied))
        ));
        let wrong_space = hal::target::<VmAddressSpaceObjectTag>(DEVICE_DRIVER_CAPS_DEVICE_SLOT);
        assert!(matches!(
            state.device().bar(4, &wrong_space),
            Err(CallError::Remote(DeviceError::Denied))
        ));
        sel4::debug_println!("DEVICE_CAP_CHECK_PASS wrong_vm_object=denied");
    });
    let resources = hal::with(|state| {
        let resources = state.device().resources().expect("assigned PCI resources");
        state.map_bars(&resources);
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
    let common: NonNull<u8> =
        hal::with(|state| state.mmio(base.checked_add(offset).unwrap(), length));
    let transport = PciTransport::new::<hal::CapabilityHal, _>(&mut pci, function)
        .expect("virtio PCI transport");
    assert_eq!(transport.device_type(), DeviceType::Console);
    // The first provisioning profile supplies MSI-X table entry zero.
    let transport = unsafe { IommuPciTransport::new(transport, common, length, 0) }
        .expect("IOMMU virtio transport");
    let notification = hal::with(|state| {
        let capability = state.device().interrupt().expect("remapped IRQ");
        let notification = state
            .transport
            .space
            .cap::<sel4::cap_type::Notification>(capability.as_ref())
            .unwrap();
        #[cfg(feature = "dma-probe")]
        {
            state.fault_wait = Some(notification);
        }
        state.device().activate().expect("bus mastering");
        notification
    });
    let mut console =
        VirtIOConsole::<hal::CapabilityHal, _>::new(transport).expect("console queues");
    console
        .send_bytes(b"HORN_VIRTIO_CONSOLE_READY\n")
        .expect("console transmit");
    sel4::debug_println!(
        "VIRTIO_CONSOLE_READY allocator=mimalloc transport=upstream dma=iova irq=msix"
    );
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
        let _ = notification.wait();
        console.ack_interrupt().expect("console interrupt");
        while let Some(byte) = console.recv(true).expect("console receive") {
            console.send(byte).expect("console echo");
        }
        hal::with(|state| state.device().ack().expect("IRQ rearm"));
        if !irq_reported {
            sel4::debug_println!("VIRTIO_CONSOLE_IRQ delivered=true");
            irq_reported = true;
        }
    }
}
