//! Privileged discovery/resource service. Device-specific drivers run elsewhere.
#![no_std]
#![no_main]
extern crate alloc;
#[cfg(feature = "boot-tests")]
mod boot_tests;
mod drivers;
mod clock;
mod native;
mod service;

use acpi::{AcpiHandler, AcpiTables, PhysicalMapping, rsdp::Rsdp};
use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, cell::RefCell, mem, ptr::NonNull};
use horn_devmgr::{Inventory, firmware, pci::ConfigRead, protocol::*};
use horn_ipc::*;
const SLOTS: TaskSlots = TaskSlots::new(DEVICE_CAPS_FIRST_FREE_SLOT, DEVICE_CAPS_SIZE_BITS);
horn_runtime::task!(main, vm: horn_runtime::VmBootstrap {
    cnode: DEVICE_CAPS_SELF_SLOT,
    factory: DEVICE_CAPS_VM_SLOT,
    address_space: DEVICE_CAPS_ADDRESS_SPACE_SLOT,
    slots: SLOTS,
});

use native::PlatformClient;

#[derive(Clone)]
struct Firmware<'a>(&'a RefCell<PlatformClient>);
impl Firmware<'_> {
    fn read(&self, mut address: u64, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(256) {
            let data = self
                .0
                .borrow_mut()
                .read(address, chunk.len() as u32)
                .expect("physical read");
            assert_eq!(data.len as usize, chunk.len());
            chunk.copy_from_slice(&data.data[..chunk.len()]);
            address += chunk.len() as u64;
        }
    }
}
impl AcpiHandler for Firmware<'_> {
    unsafe fn map_physical_region<T>(
        &self,
        address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        // Immutable table snapshots, not writable AML/operation-region mappings.
        // acpi's mapping API cannot return an error: invalid/unreadable tables
        // are fatal bootstrap errors. Bound firmware-controlled allocations.
        assert!(size >= mem::size_of::<T>() && size <= 64 * 1024);
        let layout = Layout::from_size_align(size, mem::align_of::<T>()).unwrap();
        let pointer = unsafe { alloc(layout) };
        let Some(pointer) = NonNull::new(pointer) else {
            handle_alloc_error(layout)
        };
        self.read(address as u64, unsafe {
            core::slice::from_raw_parts_mut(pointer.as_ptr(), size)
        });
        unsafe { PhysicalMapping::new(address, pointer.cast(), size, size, self.clone()) }
    }
    fn unmap_physical_region<T>(region: &PhysicalMapping<Self, T>) {
        let layout = Layout::from_size_align(region.mapped_length(), mem::align_of::<T>()).unwrap();
        unsafe { dealloc(region.virtual_start().as_ptr().cast(), layout) };
    }
}
impl ConfigRead for Firmware<'_> {
    fn read(&mut self, address: u64) -> Result<u32, DeviceError> {
        let bytes = self
            .0
            .borrow_mut()
            .read(address, 4)
            .map_err(|_| DeviceError::Backend)?;
        if bytes.len != 4 {
            return Err(DeviceError::Backend);
        }
        Ok(u32::from_le_bytes(bytes.data[..4].try_into().unwrap()))
    }
}

fn main() -> ! {
    let root = sel4::cap::CNode::from_bits(DEVICE_CAPS_SELF_SLOT as _);
    let receive = root.absolute_cptr(sel4::CPtr::from_bits(SLOTS.public_receive() as _));
    // Public receive, private RPC and heap callbacks have disjoint slots.
    let transport = horn_runtime::transport(root, SLOTS.application()).expect("devmgr transport");
    let target = horn_runtime::bootstrap_cap(root, DEVICE_CAPS_PLATFORM_SLOT);
    let mut platform = DevicePlatformClient::new(transport, target);
    #[cfg(feature = "boot-tests")]
    boot_tests::allocator();
    let policy = platform.policy().expect("bootstrap policy");
    let supplied = platform.rsdp().expect("bootstrap RSDP");
    let mut bytes = [0u8; 36];
    assert!(supplied.len == 20 || supplied.len == 36);
    bytes[..supplied.len as usize].copy_from_slice(&supplied.data[..supplied.len as usize]);
    assert_eq!(
        bytes[..20].iter().fold(0u8, |sum, x| sum.wrapping_add(*x)),
        0
    );
    let rsdp = unsafe { &*bytes.as_ptr().cast::<Rsdp>() };
    if rsdp.revision() != 0 {
        assert_eq!(supplied.len, 36);
        assert_eq!(rsdp.length(), 36); // Bound acpi's checksum slice before validate.
    }
    rsdp.validate().expect("valid RSDP");
    let address = if rsdp.revision() == 0 {
        rsdp.rsdt_address() as usize
    } else {
        rsdp.xsdt_address() as usize
    };
    let platform = RefCell::new(platform);
    let mut firmware = Firmware(&platform);
    let tables = unsafe { AcpiTables::from_rsdt(firmware.clone(), rsdp.revision(), address) }
        .expect("ACPI root table");
    let regions = firmware::ecam_regions(&tables).expect("ACPI MCFG");
    let mut inventory = Inventory::discover(&mut firmware, &regions, 256).expect("PCIe discovery");
    let dmar = if policy.vtd {
        Some(horn_devmgr::vtd::dmar::Dmar::discover(&tables).expect("ACPI DMAR"))
    } else {
        None
    };
    let hpet=clock::discover(&tables);
    drop(tables);
    let count = inventory.status().unwrap().pci_devices;
    let mut io = native::Io::new(platform.into_inner());
    let mut service = if let Some(dmar) = dmar {
        assert!(regions.iter().all(|r| r.segment == 0 && r.first_bus == 0));
        let mut cursor = 0;
        loop {
            let page = inventory.list(cursor).unwrap();
            for device in &page.entries.data[..page.entries.len as usize] {
                let bdf = ((device.address.bus << 8)
                    | (device.address.device << 3)
                    | device.address.function) as u16;
                let address = regions
                    .iter()
                    .find(|r| r.segment == 0 && device.address.bus <= r.last_bus as u32)
                    .unwrap()
                    .address(bdf, 4)
                    .unwrap();
                let command = io.config(address) as u16;
                io.write(address, 2, (command & !4) as u64);
            }
            if page.next == 0 {
                break;
            }
            cursor = page.next;
        }
        io.initialize(dmar.registers);
        let unit =
            unsafe { horn_devmgr::vtd::Vtd::enable(io, dmar, policy.ticks_per_us * 1_000_000) }
                .expect("VT-d initialization");
        sel4::debug_println!(
            "VTD_ACTIVE translation=true interrupt_remap=true compatibility=false"
        );
        service::Service::enabled(inventory, regions, unit)
    } else {
        service::Service::new(inventory, regions, io)
    };
    sel4::debug_println!(
        "DEVMGR_READY devices={} ecam=true iommu={}",
        count,
        policy.vtd
    );
    let fault_irq = service.arm_fault_irq();
    service.init_clock(hpet);
    service.io().platform.ready().expect("platform preparation");
    #[cfg(feature = "boot-tests")]
    boot_tests::before_start();
    let mut drivers = drivers::Drivers::start(&mut service);
    #[cfg(feature = "boot-tests")]
    let mut observer = drivers
        .is_empty()
        .then(|| boot_tests::Observer::start(&mut service.io().platform.transport));
    let mut reported = false;
    let reply = sel4::cap::Reply::from_bits(DEVICE_CAPS_REPLY_SLOT as _);
    loop {
        #[cfg(not(feature = "boot-tests"))]
        let tests_finished = true;
        #[cfg(feature = "boot-tests")]
        let tests_finished = observer.as_ref().is_none_or(|probe| probe.finished);
        if !reported && tests_finished {
            reported = true;
            sel4::debug_println!("DEVMGR_SERVICE_READY configured=true");
            #[cfg(feature = "boot-tests")]
            boot_tests::ready();
            #[cfg(not(feature = "boot-tests"))]
            horn_runtime::notify_ready(DEVICE_CAPS_READY_SLOT);
        }
        let request = {
            let mut server = Sel4Server::new(
                root,
                sel4::cap::Endpoint::from_bits(DEVICE_CAPS_SERVICE_SLOT as _),
                receive,
                service.io().space(),
            );
            sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&server.recv_slot));
            let (info, badge) = server.endpoint.recv(reply);
            if badge & (horn_devmgr::FAULT_NOTIFICATION_BADGE | horn_supervisor::OWNER_CHANGED | horn_devmgr::TIMER_NOTIFICATION_BADGE) != 0
            {
                drop(server);
                receive.delete().unwrap();
                if badge & horn_devmgr::FAULT_NOTIFICATION_BADGE != 0 {
                    let faults =
                        service.handle_fault_irq(fault_irq.expect("unexpected fault notification"));
                    drivers.faulted(&mut service, faults);
                }
                if badge & horn_devmgr::TIMER_NOTIFICATION_BADGE != 0 {service.clock_irq();}
                if badge & horn_supervisor::OWNER_CHANGED != 0 {
                    drivers.changed(&mut service);
                    #[cfg(feature = "boot-tests")]
                    if let Some(probe) = &mut observer {
                        probe.changed(&mut service.io().platform.transport);
                    }
                }
                continue;
            }
            if badge == 0 {
                // No exported client has badge zero. Discard unsolicited caps
                // from unbadged bootstrap traffic without dispatching authority.
                receive.delete().unwrap();
                continue;
            }
            server.decode_received_dynamic(
                info,
                badge,
                reply,
                device_host_request_layout,
                DeviceHostRequest::decode,
            )
        };
        match request {
            Ok((_, request, reply)) => {
                let badge = reply.badge;
                let response = device_host_dispatch(
                    request,
                    &mut service::Caller {
                        service: &mut service,
                        badge,
                    },
                );
                device_host_reply(reply, service.io().space(), &response).unwrap();
                receive.delete().unwrap();
            }
            Err(_) => {
                // Discard unsolicited/invalid capability arguments and abandon
                // the malformed call, rather than killing the critical service.
                receive.delete().unwrap();
            }
        }
    }
}
