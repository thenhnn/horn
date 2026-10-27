//! Privileged discovery process. No drivers are started and no DMA is enabled.
#![no_std]
#![no_main]
extern crate alloc;
mod runtime;

use acpi::{AcpiHandler, AcpiTables, PhysicalMapping, rsdp::Rsdp};
use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::{alloc::Layout, cell::RefCell, mem, ptr::NonNull};
use horn_devmgr::{Inventory, firmware, pci::ConfigRead, protocol::*};
use horn_ipc::*;
horn_ipc::horn_tag!();

type PlatformClient = DevicePlatformClient<Sel4Transport<Sel4CapSpace>>;

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
    let receive = root.absolute_cptr(sel4::CPtr::from_bits(DEVICE_CAPS_FIRST_FREE_SLOT as _));
    let space = Sel4CapSpace::new_local(
        root,
        (DEVICE_CAPS_FIRST_FREE_SLOT + 1) as _,
        1 << DEVICE_CAPS_SIZE_BITS,
    );
    let target = Cap::new(
        root.bits() as _,
        DEVICE_CAPS_PLATFORM_SLOT,
        sel4::WORD_SIZE as _,
        0,
        CapMode::COPY,
        Rights::ALL,
    );
    let mut platform = DevicePlatformClient::new(Sel4Transport::new(root, receive, space), target);
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
    drop(tables);
    let count = inventory.status().unwrap().pci_devices;
    sel4::debug_println!("DEVMGR_READY devices={} ecam=true iommu=false", count);
    firmware.0.borrow_mut().ready().expect("bootstrap ready");
    // Reuse the bootstrap transport's local slots; never export its authority.
    let client = platform.into_inner();
    let mut server = Sel4Server::new(
        root,
        sel4::cap::Endpoint::from_bits(DEVICE_CAPS_SERVICE_SLOT as _),
        receive,
        client.transport.space,
    );
    let reply = sel4::cap::Reply::from_bits(DEVICE_CAPS_REPLY_SLOT as _);
    loop {
        match server.recv_request_dynamic(
            device_discovery_request_layout,
            reply,
            DeviceDiscoveryRequest::decode,
        ) {
            Ok((_, request, reply)) => {
                let response = device_discovery_dispatch(request, &mut inventory);
                device_discovery_reply(reply, &mut server.space, &response).unwrap();
            }
            Err(_) => {
                // Observer requests carry no caps. Discard an unsolicited cap
                // and abandon the malformed call, rather than killing devmgr.
                receive.delete().unwrap();
            }
        }
    }
}
