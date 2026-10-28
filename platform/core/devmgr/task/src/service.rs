//! PCI resource assignment and revocation, independent of the device driver.
//! The initial topology profile has no bridges/aliases or requester reuse after
//! detach. Drivers receive BAR memory and IOVAs, never physical RAM addresses,
//! writable PCI configuration or the platform bootstrap authority.
use crate::native::{Io, Mapping, target};
use alloc::vec::Vec;
use horn_devmgr::{
    Inventory,
    firmware::EcamRegion,
    protocol::*,
    vtd::{self, Domain, Interrupt, Vtd},
};
use horn_ipc::*;

enum Hardware {
    ReadOnly(Io),
    Remapping(Vtd<Io>),
}
struct Bar {
    index: u32,
    address: u64,
    length: usize,
}
struct Irq {
    token: u64,
    route: Interrupt,
    notification: CapRef,
    backing: Option<(Cap<VmUntypedObjectTag>, Cap<Untyped>)>,
    table: usize,
}
struct Device {
    badge: u64,
    authority: CapRef,
    sid: u16,
    config: u64,
    domain: Domain,
    bars: Vec<Bar>,
    msix: Mapping,
    msix_offset: usize,
    msix_cap: u16,
    irq: Option<Irq>,
    mmios: Vec<u64>,
    next_iova: u64,
    active: bool,
}
struct Dma {
    badge: u64,
    authority: CapRef,
    device: usize,
    token: u64,
    iova: u64,
    pages: usize,
}
pub struct Faults {
    pub requesters: [u64; 4],
    pub overflow: bool,
}
pub struct Service {
    inventory: Inventory,
    regions: Vec<EcamRegion>,
    hardware: Hardware,
    devices: Vec<Device>,
    maps: Vec<Option<Dma>>,
    next_badge: u64,
}
pub struct Caller<'a> {
    pub service: &'a mut Service,
    pub badge: u64,
}
fn failure(error: vtd::Error) -> DeviceError {
    match error {
        vtd::Error::Exhausted => DeviceError::Exhausted,
        vtd::Error::Invalid => DeviceError::Invalid,
        vtd::Error::Unsupported => DeviceError::Unsupported,
        _ => panic!("unrecoverable VT-d failure: {:?}", error),
    }
}
impl Service {
    pub fn devices(&self) -> impl Iterator<Item = &PciDevice> {
        self.inventory.devices()
    }
    /// Trusted in-process driver policy owns this grant; wire IDs alone never
    /// authorize close. Keep the private badge only after a successful claim.
    pub fn claim_driver(&mut self, id: u64) -> Result<(Cap<DeviceObjectTag>, u64), DeviceError> {
        let grant = DeviceAssignment::claim(
            &mut Caller {
                service: self,
                badge: horn_devmgr::ASSIGNMENT_BADGE,
            },
            id,
        )?;
        Ok((grant, self.devices.last().unwrap().badge))
    }
    pub fn revoke_driver(&mut self, badge: u64) -> Result<(), DeviceError> {
        DeviceControl::close(&mut Caller {
            service: self,
            badge,
        })
    }
    pub fn new(inventory: Inventory, regions: Vec<EcamRegion>, io: Io) -> Self {
        Self {
            inventory,
            regions,
            hardware: Hardware::ReadOnly(io),
            devices: Vec::new(),
            maps: Vec::new(),
            next_badge: 3,
        }
    }
    pub fn enabled(mut inventory: Inventory, regions: Vec<EcamRegion>, unit: Vtd<Io>) -> Self {
        inventory.iommu_enabled();
        Self {
            inventory,
            regions,
            hardware: Hardware::Remapping(unit),
            devices: Vec::new(),
            maps: Vec::new(),
            next_badge: 3,
        }
    }
    pub fn io(&mut self) -> &mut Io {
        match &mut self.hardware {
            Hardware::ReadOnly(io) => io,
            Hardware::Remapping(unit) => unit.hardware(),
        }
    }
    pub fn arm_fault_irq(&mut self) -> Option<u64> {
        if matches!(self.hardware, Hardware::ReadOnly(_)) {
            return None;
        }
        // The unit's own fault MSI uses the kernel's MSI-vector mechanism,
        // not a PCI function or an interrupt-remapping table entry.
        let io = self.io();
        let irq = io.platform.irq(0).expect("fault kernel IRQ");
        io.platform
            .bind_irq(irq.token, &target(DEVICE_CAPS_FAULT_SLOT))
            .expect("fault IRQ bind");
        io.platform.ack_irq(irq.token).expect("fault IRQ arm");
        let vector = u8::try_from(irq.vector).expect("fault IRQ vector");
        self.unit()
            .fault_interrupt(vector, 0)
            .expect("fault event MSI");
        sel4::debug_println!("VTD_FAULT_IRQ armed=true");
        Some(irq.token)
    }
    pub fn handle_fault_irq(&mut self, token: u64) -> Faults {
        let mut faults = Faults {
            requesters: [0; 4],
            overflow: false,
        };
        let unit = self.unit();
        unit.mask_fault_interrupt(true).expect("mask fault event");
        loop {
            let report = unit.poll_fault().expect("VT-d fault records");
            faults.overflow |= report.overflow;
            if let Some(fault) = report.fault {
                if fault.source < 256 {
                    faults.requesters[fault.source as usize / 64] |= 1u64 << (fault.source % 64);
                }
                sel4::debug_println!(
                    "VTD_FAULT source={:04x} address={:x} reason={:x} read={} overflow={}",
                    fault.source,
                    fault.address,
                    fault.reason,
                    fault.read,
                    report.overflow
                );
            } else {
                if report.overflow {
                    sel4::debug_println!("VTD_FAULT overflow=true");
                }
                break;
            }
        }
        self.io().platform.ack_irq(token).expect("fault IRQ ack");
        self.unit()
            .mask_fault_interrupt(false)
            .expect("unmask fault event");
        faults
    }
    fn unit(&mut self) -> &mut Vtd<Io> {
        match &mut self.hardware {
            Hardware::Remapping(unit) => unit,
            _ => panic!("assignment without IOMMU"),
        }
    }
    fn mint(&mut self) -> Result<(u64, CapRef, CapRef), DeviceError> {
        let badge = self.next_badge;
        if badge >= horn_supervisor::OWNER_CHANGED {
            return Err(DeviceError::Exhausted);
        }
        self.next_badge = badge.checked_add(1).ok_or(DeviceError::Exhausted)?;
        let space = self.io().space();
        let original = space.reserve_slot().map_err(|_| DeviceError::Exhausted)?;
        let root = sel4::cap::CNode::from_bits(DEVICE_CAPS_SELF_SLOT as _);
        if root
            .absolute_cptr(*original.path())
            .mint(
                &root.absolute_cptr(sel4::CPtr::from_bits(DEVICE_CAPS_SERVICE_SLOT as _)),
                sel4::CapRights::new(true, true, false, true),
                badge,
            )
            .is_err()
        {
            return Err(DeviceError::Quarantined);
        }
        let authority = space
            .register(sel4::cap::Endpoint::from_bits(original.path().bits()))
            .unwrap();
        let exported = match space.reserve_slot() {
            Ok(slot) => slot,
            Err(_) => {
                space.delete(authority).unwrap();
                return Err(DeviceError::Exhausted);
            }
        };
        root.absolute_cptr(*exported.path())
            .copy(
                &root.absolute_cptr(*original.path()),
                sel4::CapRights::all(),
            )
            .unwrap();
        let exported = space
            .register(sel4::cap::Endpoint::from_bits(exported.path().bits()))
            .unwrap();
        Ok((badge, authority, exported))
    }
    fn retire(&mut self, authority: CapRef) {
        let root = sel4::cap::CNode::from_bits(DEVICE_CAPS_SELF_SLOT as _);
        root.absolute_cptr(sel4::CPtr::from_bits(authority.slot as _))
            .revoke()
            .expect("authority revoke");
        self.io().space().delete(authority).unwrap();
    }
    fn device(&self, badge: u64) -> Result<usize, DeviceError> {
        self.devices
            .iter()
            .position(|d| d.badge == badge && d.active)
            .ok_or(DeviceError::Denied)
    }
    fn close_map(&mut self, index: usize) {
        let map = self.maps[index].take().unwrap();
        let domain = self.devices[map.device].domain;
        self.unit()
            .unmap(domain, map.iova, map.pages)
            .expect("DMA invalidate/drain");
        self.io()
            .platform
            .unpin(map.token)
            .expect("DMA unpin after drain");
        self.retire(map.authority);
    }
}
impl DeviceObserver for Caller<'_> {
    fn status(&mut self) -> Result<DeviceStatus, DeviceError> {
        self.service.inventory.status()
    }
    fn list(&mut self, after: u64) -> Result<DevicePage, DeviceError> {
        self.service.inventory.list(after)
    }
    fn info(&mut self, id: u64) -> Result<PciDeviceInfo, DeviceError> {
        self.service.inventory.info(id)
    }
}
impl DeviceAssignment for Caller<'_> {
    fn claim(&mut self, id: u64) -> Result<Cap<DeviceObjectTag>, DeviceError> {
        if self.badge != horn_devmgr::ASSIGNMENT_BADGE
            || !matches!(self.service.hardware, Hardware::Remapping(_))
        {
            return Err(DeviceError::Denied);
        }
        // A bounded single-assignment profile until requester groups and
        // per-assignment resource admission are implemented.
        if !self.service.devices.is_empty() {
            return Err(DeviceError::Exhausted);
        }
        let info = self.service.inventory.info(id)?;
        let d = &info.device;
        if d.address.segment != 0
            || d.address.bus != 0
            || d.address.function != 0
            || d.header_type != 0
        {
            return Err(DeviceError::Unsupported);
        }
        let sid = ((d.address.device << 3) | d.address.function) as u16;
        let config = self
            .service
            .regions
            .iter()
            .find(|r| r.segment == 0 && r.first_bus == 0)
            .ok_or(DeviceError::Unsupported)?
            .address(sid, 0)?;
        let io = self.service.io();
        let command = io.config(config + 4) as u16;
        io.write(config + 4, 2, (command & !7) as u64);
        let mut bars = Vec::new();
        let mut index = 0;
        while index < 6 {
            let register = config + 0x10 + index * 4;
            let low = io.config(register);
            if low == 0 || low & 1 != 0 {
                index += 1;
                continue;
            }
            let wide = low & 6 == 4;
            if wide && index == 5 {
                return Err(DeviceError::Invalid);
            }
            let high = if wide { io.config(register + 4) } else { 0 };
            io.write(register, 4, u32::MAX as u64);
            if wide {
                io.write(register + 4, 4, u32::MAX as u64);
            }
            let mask_low = io.config(register) & !15;
            let mask_high = if wide {
                io.config(register + 4)
            } else {
                u32::MAX
            };
            if wide {
                io.write(register + 4, 4, high as u64);
            }
            io.write(register, 4, low as u64);
            let mask = ((mask_high as u64) << 32) | mask_low as u64;
            let length = (!mask).wrapping_add(1);
            let address = ((high as u64) << 32) | (low as u64 & !15);
            if length >= 4096
                && length.is_power_of_two()
                && length <= 65536
                && address & (length - 1) == 0
                && address >= 0x100000
                && address.checked_add(length).is_some()
            {
                bars.push(Bar {
                    index: index as u32,
                    address,
                    length: length as usize,
                });
            }
            index += if wide { 2 } else { 1 };
        }
        io.write(config + 4, 2, ((command & !5) | 2 | 0x400) as u64);
        let mut cap = io.config(config + 0x34) as u8 & !3;
        let mut msix = None;
        for _ in 0..48 {
            if cap == 0 {
                break;
            }
            let word = io.config(config + cap as u64);
            if word & 255 == 0x11 {
                let table = io.config(config + cap as u64 + 4);
                msix = Some((cap as u16, table & 7, (table & !7) as usize));
            }
            cap = ((word >> 8) as u8) & !3;
        }
        let (msix_cap, msix_bir, msix_offset) = msix.ok_or(DeviceError::Unsupported)?;
        let bar = bars
            .iter()
            .find(|b| b.index == msix_bir && msix_offset + 16 <= b.length)
            .ok_or(DeviceError::Invalid)?;
        let msix = io.mmio(bar.address, bar.length).expect("MSI-X BAR");
        let domain = self.service.unit().attach(sid).map_err(failure)?;
        let (badge, authority, exported) = self.service.mint()?;
        self.service.devices.push(Device {
            badge,
            authority,
            sid,
            config,
            domain,
            bars,
            msix,
            msix_offset,
            msix_cap,
            irq: None,
            mmios: Vec::new(),
            next_iova: 0x10000,
            active: true,
        });
        Ok(Cap::from_ref(exported))
    }
}
impl DeviceControl for Caller<'_> {
    fn resources(&mut self) -> Result<PciResources, DeviceError> {
        let d = self.service.device(self.badge)?;
        let device = &self.service.devices[d];
        let config = device.config;
        let address = PciAddress {
            segment: 0,
            bus: 0,
            device: (device.sid >> 3) as u32,
            function: (device.sid & 7) as u32,
        };
        let mut bar_lengths = List::default();
        bar_lengths.len = 6;
        for bar in &device.bars {
            bar_lengths.data[bar.index as usize] = bar.length as u64;
        }
        let mut configuration = List::default();
        configuration.len = 64;
        for index in 0..64 {
            configuration.data[index] = self.service.io().config(config + index as u64 * 4);
        }
        Ok(PciResources {
            address,
            configuration,
            bar_lengths,
        })
    }
    fn bar(
        &mut self,
        index: u32,
        space: Cap<VmAddressSpaceObjectTag>,
    ) -> Result<Cap<VmMemoryObjectTag>, DeviceError> {
        let d = self.service.device(self.badge)?;
        if space.as_ref().unwrapped_badge().is_some() {
            return Err(DeviceError::Denied);
        }
        let bar = self.service.devices[d]
            .bars
            .iter()
            .find(|b| b.index == index)
            .ok_or(DeviceError::NotFound)?;
        let (address, length) = (bar.address, bar.length);
        if self.service.devices[d].mmios.len() >= 8 {
            return Err(DeviceError::Exhausted);
        }
        let grant = self
            .service
            .io()
            .platform
            .mmio(&space, address, length as u64)
            .map_err(|_| DeviceError::Invalid)?;
        self.service.devices[d].mmios.push(grant.token);
        Ok(grant.memory)
    }
    fn map(
        &mut self,
        memory: Cap<VmMemoryObjectTag>,
        offset: u64,
        length: u64,
        access: u32,
    ) -> Result<DeviceDma, DeviceError> {
        let d = self.service.device(self.badge)?;
        // A cap unwrapped by OUR endpoint is not VM authority. It cannot be
        // forwarded to the root VM broker; reject it before minting resources
        // instead of turning a caller-supplied wrong object into a fatal RPC.
        if memory.as_ref().unwrapped_badge().is_some() {
            return Err(DeviceError::Denied);
        }
        if self.service.maps.iter().flatten().count() >= 8 {
            return Err(DeviceError::Exhausted);
        }
        let index = if let Some(index) = self.service.maps.iter().position(Option::is_none) {
            index
        } else {
            self.service
                .maps
                .try_reserve(1)
                .map_err(|_| DeviceError::Exhausted)?;
            self.service.maps.push(None);
            self.service.maps.len() - 1
        };
        let (badge, authority, exported) = self.service.mint()?;
        let pin = match self
            .service
            .io()
            .platform
            .pin(&memory, offset, length, access)
        {
            Ok(pin) => pin,
            Err(CallError::Remote(e)) => {
                self.service.retire(authority);
                self.service.io().space().delete(exported).unwrap();
                return Err(e);
            }
            Err(_) => panic!("DMA pin transport"),
        };
        let domain = self.service.devices[d].domain;
        let iova = self.service.devices[d].next_iova;
        if let Err(error) = self.service.unit().map(
            domain,
            iova,
            &pin.pages.data[..pin.pages.len as usize],
            access as u8,
        ) {
            if self.service.unit().poisoned() {
                panic!("DMA map poisoned: {:?}", error);
            }
            self.service
                .io()
                .platform
                .unpin(pin.token)
                .expect("unpublished pin release");
            self.service.retire(authority);
            self.service.io().space().delete(exported).unwrap();
            return Err(failure(error));
        }
        self.service.devices[d].next_iova += length;
        self.service.maps[index] = Some(Dma {
            badge,
            authority,
            device: d,
            token: pin.token,
            iova,
            pages: pin.pages.len as usize,
        });
        Ok(DeviceDma {
            mapping: Cap::from_ref(exported),
            iova,
        })
    }
    fn interrupt(&mut self) -> Result<Cap<Notification>, DeviceError> {
        let d = self.service.device(self.badge)?;
        if self.service.devices[d].irq.is_some() {
            return Err(DeviceError::Busy);
        }
        let sid = self.service.devices[d].sid;
        let domain = self.service.devices[d].domain;
        let io = self.service.io();
        let lease = VmFactoryClient::new(&mut io.platform.transport, target(DEVICE_CAPS_VM_SLOT))
            .delegate(12)
            .expect("IRQ Untyped");
        let grant = VmUntypedClient::new(&mut io.platform.transport, Cap::from_ref(lease.as_ref()))
            .capability()
            .expect("IRQ Untyped cap");
        let untyped = grant.capability;
        let kernel = io
            .space()
            .cap::<sel4::cap_type::Untyped>(untyped.as_ref())
            .unwrap();
        let slot = io.space().reserve_slot().expect("notification slot");
        let root = sel4::cap::CNode::from_bits(DEVICE_CAPS_SELF_SLOT as _);
        kernel
            .untyped_retype(
                &sel4::ObjectBlueprint::Notification,
                &root.absolute_cptr_for_self(),
                slot.path().bits() as usize,
                1,
            )
            .expect("notification retype");
        let notification = io
            .space()
            .register(sel4::cap::Notification::from_bits(slot.path().bits()))
            .unwrap();
        let irq = io.platform.irq(sid as u32).expect("kernel IRQ");
        io.platform
            .bind_irq(irq.token, &Cap::from_ref(notification))
            .expect("IRQ bind");
        let route = self
            .service
            .unit()
            .interrupt(domain, irq.vector as u8, 0)
            .expect("interrupt remap");
        let device = &self.service.devices[d];
        let table = device.msix.address + device.msix_offset;
        let control = device.config + device.msix_cap as u64 + 2;
        unsafe {
            (table as *mut u32).add(3).write_volatile(1);
            (table as *mut u32).write_volatile(route.address() as u32);
            (table as *mut u32)
                .add(1)
                .write_volatile((route.address() >> 32) as u32);
            (table as *mut u32).add(2).write_volatile(route.data());
        }
        self.service.io().write(control, 2, 0x8000); // enable MSI-X, function mask clear
        unsafe {
            (table as *mut u32).add(3).write_volatile(0);
        }
        self.service
            .io()
            .platform
            .ack_irq(irq.token)
            .expect("IRQ arm");
        let slot = self
            .service
            .io()
            .space()
            .reserve_slot()
            .expect("notification export");
        root.absolute_cptr(*slot.path())
            .copy(
                &root.absolute_cptr(sel4::CPtr::from_bits(notification.slot as _)),
                sel4::CapRights::read_only(),
            )
            .unwrap();
        let exported = self
            .service
            .io()
            .space()
            .register(sel4::cap::Notification::from_bits(slot.path().bits()))
            .unwrap();
        self.service.devices[d].irq = Some(Irq {
            token: irq.token,
            route,
            notification,
            backing: Some((lease, untyped)),
            table,
        });
        Ok(Cap::from_ref(exported))
    }
    fn interrupt_to(&mut self, notification: Cap<Notification>) -> Result<(), DeviceError> {
        let d = self.service.device(self.badge)?;
        if self.service.devices[d].irq.is_some()
            || notification.as_ref().unwrapped_badge().is_some()
        {
            return Err(DeviceError::Denied);
        }
        let root = sel4::cap::CNode::from_bits(DEVICE_CAPS_SELF_SLOT as _);
        let slot = self
            .service
            .io()
            .space()
            .reserve_slot()
            .expect("IRQ notification retention");
        root.absolute_cptr(*slot.path())
            .copy(
                &root.absolute_cptr(sel4::CPtr::from_bits(notification.as_ref().slot as _)),
                sel4::CapRights::all(),
            )
            .expect("notification copy");
        let notification = self
            .service
            .io()
            .space()
            .register(sel4::cap::Notification::from_bits(slot.path().bits()))
            .unwrap();
        let sid = self.service.devices[d].sid;
        let domain = self.service.devices[d].domain;
        let irq = self
            .service
            .io()
            .platform
            .irq(sid as u32)
            .expect("kernel IRQ");
        self.service
            .io()
            .platform
            .bind_irq(irq.token, &Cap::from_ref(notification))
            .expect("IRQ bind");
        let route = self
            .service
            .unit()
            .interrupt(domain, irq.vector as u8, 0)
            .expect("interrupt remap");
        let device = &self.service.devices[d];
        let table = device.msix.address + device.msix_offset;
        let control = device.config + device.msix_cap as u64 + 2;
        unsafe {
            (table as *mut u32).add(3).write_volatile(1);
            (table as *mut u32).write_volatile(route.address() as u32);
            (table as *mut u32)
                .add(1)
                .write_volatile((route.address() >> 32) as u32);
            (table as *mut u32).add(2).write_volatile(route.data());
        }
        self.service.io().write(control, 2, 0x8000);
        unsafe {
            (table as *mut u32).add(3).write_volatile(0);
        }
        self.service
            .io()
            .platform
            .ack_irq(irq.token)
            .expect("IRQ arm");
        self.service.devices[d].irq = Some(Irq {
            token: irq.token,
            route,
            notification,
            backing: None,
            table,
        });
        Ok(())
    }
    fn ack(&mut self) -> Result<(), DeviceError> {
        let d = self.service.device(self.badge)?;
        let token = self.service.devices[d]
            .irq
            .as_ref()
            .ok_or(DeviceError::NotFound)?
            .token;
        self.service
            .io()
            .platform
            .ack_irq(token)
            .map_err(|_| DeviceError::Backend)
    }
    fn activate(&mut self) -> Result<(), DeviceError> {
        let d = self.service.device(self.badge)?;
        let device = &self.service.devices[d];
        if device.irq.is_none() {
            return Err(DeviceError::Busy);
        }
        let config = device.config;
        let command = self.service.io().config(config + 4) as u16;
        self.service
            .io()
            .write(config + 4, 2, (command | 6 | 0x400) as u64);
        Ok(())
    }
    fn close(&mut self) -> Result<(), DeviceError> {
        let d = self.service.device(self.badge)?;
        // Stop the CPU from issuing new device commands BEFORE touching DMA.
        let mmios = core::mem::take(&mut self.service.devices[d].mmios);
        for token in mmios {
            self.service
                .io()
                .platform
                .revoke_mmio(token)
                .expect("MMIO revoke acknowledgement");
        }
        let device = &self.service.devices[d];
        let config = device.config;
        let domain = device.domain;
        let command = self.service.io().config(config + 4) as u16;
        self.service
            .io()
            .write(config + 4, 2, (command & !4) as u64);
        if let Some(irq) = self.service.devices[d].irq.take() {
            unsafe {
                ((irq.table + 12) as *mut u32).write_volatile(1);
            }
            self.service
                .unit()
                .remove_interrupt(domain, irq.route)
                .expect("interrupt invalidate");
            self.service
                .io()
                .platform
                .close_irq(irq.token)
                .expect("kernel IRQ revoke");
            self.service.retire(irq.notification);
            if let Some((lease, untyped)) = irq.backing {
                VmUntypedClient::new(
                    &mut self.service.io().platform.transport,
                    Cap::from_ref(lease.as_ref()),
                )
                .close()
                .expect("notification backing revoke");
                self.service.io().space().delete(untyped.as_ref()).unwrap();
                self.service.io().space().delete(lease.as_ref()).unwrap();
            }
        }
        // A driver-specific reset is not part of devmgr. Deny the requester and
        // acknowledge translation invalidation/read+write draining before any
        // backing is released. A late request faults: this SID and its IOVAs are
        // NEVER reassigned during this boot, even if the device keeps running.
        self.service
            .unit()
            .detach(domain)
            .expect("domain detach drain");
        for index in 0..self.service.maps.len() {
            if self.service.maps[index]
                .as_ref()
                .is_some_and(|m| m.device == d)
            {
                let map = self.service.maps[index].take().unwrap();
                self.service
                    .io()
                    .platform
                    .unpin(map.token)
                    .expect("detached DMA unpin");
                self.service.retire(map.authority);
            }
        }
        let msix = &self.service.devices[d].msix;
        let (region, memory, token) = (msix.region, msix.memory, msix.token);
        self.service.io().release_mmio(region, memory, token);
        let authority = self.service.devices[d].authority;
        self.service.devices[d].active = false;
        self.service.retire(authority);
        Ok(())
    }
}
impl DeviceMapping for Caller<'_> {
    fn close(&mut self) -> Result<(), DeviceError> {
        let index = self
            .service
            .maps
            .iter()
            .position(|m| m.as_ref().is_some_and(|m| m.badge == self.badge))
            .ok_or(DeviceError::Denied)?;
        self.service.close_map(index);
        Ok(())
    }
}
