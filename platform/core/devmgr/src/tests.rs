use crate::{
    Inventory,
    firmware::{self, EcamRegion},
    pci::ConfigRead,
    protocol::*,
};
use acpi::{AcpiHandler, AcpiTables, PhysicalMapping};
use alloc::{collections::BTreeMap, vec, vec::Vec};
use core::{mem, ptr::NonNull};
use horn_ipc::{Decoder, Encoder, MAX_MESSAGE_BYTES, Wire, WireCaps};

#[derive(Clone)]
struct Identity;
impl AcpiHandler for Identity {
    unsafe fn map_physical_region<T>(
        &self,
        address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        unsafe {
            PhysicalMapping::new(
                address,
                NonNull::new(address as *mut T).unwrap(),
                size,
                size,
                Self,
            )
        }
    }
    fn unmap_physical_region<T>(_: &PhysicalMapping<Self, T>) {}
}
fn table(signature: &[u8; 4], length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    bytes[..4].copy_from_slice(signature);
    bytes[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[8] = 1;
    bytes[10..16].copy_from_slice(b"HORN  ");
    bytes[16..24].copy_from_slice(b"TEST    ");
    bytes
}
fn checksum(bytes: &mut [u8]) {
    bytes[9] = 0;
    bytes[9] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)));
}
fn region(segment: u16, bus: u8) -> EcamRegion {
    EcamRegion {
        base: 0x1000_0000 + (segment as u64) * 0x1000_0000,
        segment,
        first_bus: bus,
        last_bus: bus,
    }
}

#[test]
fn acpi_mcfg_and_nonzero_start_bus_use_bus_zero_base() {
    let mut mcfg = table(b"MCFG", 60);
    mcfg[44..52].copy_from_slice(&0xe000_0000u64.to_le_bytes());
    mcfg[52..54].copy_from_slice(&7u16.to_le_bytes());
    mcfg[54] = 0x20;
    mcfg[55] = 0x2f;
    checksum(&mut mcfg);
    let mut xsdt = table(b"XSDT", 44);
    xsdt[36..44].copy_from_slice(&(mcfg.as_ptr() as u64).to_le_bytes());
    checksum(&mut xsdt);
    let tables = unsafe { AcpiTables::from_rsdt(Identity, 2, xsdt.as_ptr() as usize) }.unwrap();
    let regions = firmware::ecam_regions(&tables).unwrap();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].address(0x2000, 0x100).unwrap(), 0xe200_0100);
    assert_eq!(regions[0].segment, 7);
    assert_eq!(regions[0].address(0x1000, 0), Err(DeviceError::Invalid));
    assert_eq!(regions[0].address(0x2000, 4096), Err(DeviceError::Invalid));
    assert_eq!(regions[0].address(0x2000, 3), Err(DeviceError::Invalid));
    drop(tables);
    mcfg[9] ^= 1;
    let tables = unsafe { AcpiTables::from_rsdt(Identity, 2, xsdt.as_ptr() as usize) }.unwrap();
    assert_eq!(
        firmware::ecam_regions(&tables),
        Err(DeviceError::Unsupported)
    );
    xsdt[9] ^= 1;
    assert!(unsafe { AcpiTables::from_rsdt(Identity, 2, xsdt.as_ptr() as usize) }.is_err());
}

#[derive(Default)]
struct Bus(BTreeMap<u64, Vec<u32>>);
impl Bus {
    fn add(&mut self, r: EcamRegion, bdf: u16, header: u32) -> &mut [u32] {
        let base = r.address(bdf, 0).unwrap();
        let words = self.0.entry(base).or_insert_with(|| vec![0; 1024]);
        words[0] = 0x1234_1af4;
        words[2] = 0x0200_0102;
        words[3] = header << 16;
        words
    }
}
impl ConfigRead for Bus {
    fn read(&mut self, address: u64) -> Result<u32, DeviceError> {
        assert_eq!(address & 3, 0);
        Ok(self
            .0
            .get(&(address & !4095))
            .map_or(u32::MAX, |v| v[(address as usize & 4095) / 4]))
    }
}

#[test]
fn segments_multifunction_pagination_and_status() {
    let a = region(0, 2);
    let b = region(1, 0);
    let mut bus = Bus::default();
    bus.add(a, 0x0200, 0x80);
    bus.add(a, 0x0203, 0);
    bus.add(a, 0x0208, 0);
    // Must not read function 1 when function 0 isn't multifunction.
    bus.add(a, 0x0209, 0);
    bus.add(b, 0, 0);
    bus.add(b, 8, 0);
    let mut inventory = Inventory::discover(&mut bus, &[a, b], 256).unwrap();
    let status = inventory.status().unwrap();
    assert_eq!(status.pci_devices, 5);
    assert_eq!(status.phase, DevicePhase::DiscoveryReady);
    assert_eq!(status.iommu, IommuState::NotInitialized);
    let first = inventory.list(0).unwrap();
    assert_eq!(first.entries.len, 4);
    assert_eq!(first.entries.data[0].id, 0x0201);
    assert_eq!(first.entries.data[1].address.function, 3);
    assert_eq!(first.next, 0x1_0001);
    let last = inventory.list(first.next).unwrap();
    assert_eq!(last.entries.len, 1);
    assert_eq!(last.next, 0);
    assert_eq!(
        inventory
            .info(last.entries.data[0].id)
            .unwrap()
            .device
            .address
            .segment,
        1
    );
    assert_eq!(inventory.info(0), Err(DeviceError::NotFound));
    assert_eq!(inventory.list(u64::MAX), Err(DeviceError::Invalid));
    assert_eq!(inventory.list(1u64 << 32).unwrap().entries.len, 0);
}

#[test]
fn bars_preserve_64_bit_addresses_and_bridge_header_layout() {
    let r = region(0, 0);
    let mut bus = Bus::default();
    let words = bus.add(r, 0, 0);
    words[1] = 7; // Discovery must not enable or disable bus mastering.
    words[4] = 0x2340_000c;
    words[5] = 1;
    words[6] = 0xc001;
    words[7] = 0xdead_0008;
    let bridge = bus.add(r, 8, 1);
    bridge[4] = 0x1000_0000;
    bridge[6] = 0x0002_0100; // Bus numbers, NOT a third BAR.
    let mut inventory = Inventory::discover(&mut bus, &[r], 2).unwrap();
    let info = inventory.info(1).unwrap();
    assert_eq!(info.command, 7);
    assert_eq!(info.bars.len, 3);
    assert_eq!(info.bars.data[0].address, 0x1_2340_0000);
    assert_eq!(info.bars.data[0].kind, PciBarKind::Memory64);
    assert!(info.bars.data[0].prefetchable);
    assert_eq!(info.bars.data[1].index, 2);
    assert_eq!(info.bars.data[1].address, 0xc000);
    assert_eq!(inventory.info(9).unwrap().bars.len, 1);
}

fn capabilities(words: &mut [u32]) {
    words[1] = 1 << 20;
    words[4] = 0x8000_0008;
    words[0x34 / 4] = 0x40;
    words[0x40 / 4] = 0x4805;
    words[0x48 / 4] = 0x5011;
    words[0x50 / 4] = 0x10;
    words[0x100 / 4] = 0x0001_0015;
    words[0x104 / 4] = ((1 << 8) | (1 << 9)) << 4;
    words[0x108 / 4] = (1 << 5) | (8 << 8);
}
#[test]
fn msi_msix_and_rebar_are_reported_without_configuration_writes() {
    let r = region(0, 0);
    let mut bus = Bus::default();
    capabilities(bus.add(r, 0, 0));
    let before = bus.0.clone();
    let mut inventory = Inventory::discover(&mut bus, &[r], 1).unwrap();
    let info = inventory.info(1).unwrap();
    assert!(info.msi && info.msix);
    assert_eq!(info.resizable_bars.len, 1);
    assert_eq!(info.resizable_bars.data[0].current_size, 256 * 1024 * 1024);
    assert_eq!(
        info.resizable_bars.data[0].supported_sizes,
        (1 << 8) | (1 << 9)
    );
    assert_eq!(bus.0, before);
}

#[test]
fn unassigned_rebar_is_not_confused_with_an_absent_device_or_upper_bar_half() {
    let r = region(0, 0);
    let mut bus = Bus::default();
    let words = bus.add(r, 0, 0);
    capabilities(words);
    words[4] = 0;
    let mut inventory = Inventory::discover(&mut bus, &[r], 1).unwrap();
    let info = inventory.info(1).unwrap();
    assert_eq!(info.bars.len, 0);
    assert_eq!(info.resizable_bars.len, 1);
    let words = bus.0.get_mut(&r.address(0, 0).unwrap()).unwrap();
    words[4] = 0x8000_000c;
    words[5] = 0;
    words[0x108 / 4] |= 1; // ReBAR illegally targets upper half of BAR0.
    assert!(matches!(
        Inventory::discover(&mut bus, &[r], 1),
        Err(DeviceError::Backend)
    ));
}

#[test]
fn malformed_capability_chains_and_rebar_are_rejected() {
    let r = region(0, 0);
    for (offset, value) in [
        (0x40, 0x4005),
        (0x40, 0x4305),
        (0x100, 0x1001_0015),
        (0x108, 7 << 5),
        (0x108, (1 << 5) | (8 << 8) | 6),
        (0x108, (1 << 5) | (7 << 8)),
    ] {
        let mut bus = Bus::default();
        let words = bus.add(r, 0, 0);
        capabilities(words);
        words[offset / 4] = value;
        assert!(
            matches!(
                Inventory::discover(&mut bus, &[r], 1),
                Err(DeviceError::Backend)
            ),
            "offset {offset:x}"
        );
    }
}

#[test]
fn bounds_and_region_validation_do_not_publish_partial_inventory() {
    let r = region(0, 0);
    let mut bus = Bus::default();
    bus.add(r, 0, 0);
    bus.add(r, 8, 0);
    assert!(matches!(
        Inventory::discover(&mut bus, &[r], 1),
        Err(DeviceError::Exhausted)
    ));
    assert!(matches!(
        Inventory::discover(&mut bus, &[r], 257),
        Err(DeviceError::Invalid)
    ));
    for regions in [
        vec![],
        vec![r, r],
        vec![EcamRegion { base: 3, ..r }],
        vec![EcamRegion {
            first_bus: 2,
            last_bus: 1,
            ..r
        }],
        vec![EcamRegion {
            base: u64::MAX & !((1 << 20) - 1),
            ..r
        }],
        vec![r, EcamRegion { segment: 1, ..r }],
    ] {
        assert!(matches!(
            Inventory::discover(&mut bus, &regions, 256),
            Err(DeviceError::Invalid)
        ));
    }
}

#[test]
fn discovery_wire_messages_are_bounded_and_capability_free() {
    assert!(DevicePage::SIZE + 8 <= MAX_MESSAGE_BYTES);
    assert!(PciDeviceInfo::SIZE + 8 <= MAX_MESSAGE_BYTES);
    assert_eq!(DeviceobserverInfoArgs::CAPS, 0);
    assert_eq!(PciDeviceInfo::CAPS, 0);
    let r = region(0, 0);
    let mut bus = Bus::default();
    capabilities(bus.add(r, 0, 0));
    let mut inventory = Inventory::discover(&mut bus, &[r], 1).unwrap();
    let info = inventory.info(1).unwrap();
    let mut bytes = vec![0; PciDeviceInfo::SIZE];
    info.encode(&mut Encoder::new(&mut bytes));
    assert_eq!(
        PciDeviceInfo::decode(&mut Decoder::new(&bytes, &mut WireCaps::default())).unwrap(),
        info
    );
    assert!(mem::size_of::<PciDeviceInfo>() < 1024);
}
