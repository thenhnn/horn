//! PCI resource assignment and revocation, independent of the device driver.
//! The initial topology profile has no bridges/aliases or requester reuse after
//! detach. Drivers receive BAR memory and IOVAs, never physical RAM addresses,
//! writable PCI configuration or the platform bootstrap authority.
use crate::native::{Io, Mapping};
use alloc::vec::Vec;
use horn_devmgr::{
    Inventory,
    dma::DmaPolicy,
    firmware::EcamRegion,
    interrupt::{self, Config as _},
    protocol::*,
    vtd::{self, Domain, Interrupt, Vtd},
};
use horn_ipc::*;

struct PciConfig<'a> {
    io: &'a mut Io,
    base: u64,
}
impl interrupt::Config for PciConfig<'_> {
    fn read32(&mut self, o: u16) -> u32 {
        self.io.config(self.base + o as u64)
    }
    fn write(&mut self, o: u16, w: u32, v: u32) {
        self.io.write(self.base + o as u64, w, v as u64);
    }
}
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
}
struct Device {
    badge: u64,
    authority: CapRef,
    sid: u16,
    config: u64,
    domain: Domain,
    bars: Vec<Bar>,
    capabilities: interrupt::Capabilities,
    msix: Option<Mapping>,
    msix_offset: usize,
    dma_policy: DmaPolicy,
    dma_bytes: u64,
    irq: Option<Irq>,
    mmios: Vec<Mmio>,
    next_iova: u64,
    active: bool,
}
struct Mmio {
    token: u64,
    badge: u64,
    authority: CapRef,
    memory: Option<Cap<VmMemoryObjectTag>>,
}
struct Dma {
    badge: u64,
    authority: CapRef,
    device: usize,
    iova: u64,
    length: u64,
    access: u32,
    reservation: Option<vtd::ReservedDma>,
    committed: bool,
    pins: Vec<Pin>,
}
struct Pin {
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
    clock: Option<crate::clock::Clock>,
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
    pub fn driver_dma_policy(&mut self, badge: u64, policy: DmaPolicy) -> Result<(), DeviceError> {
        policy.validate()?;
        let d = self.device(badge)?;
        if self.devices[d].dma_bytes != 0 {
            return Err(DeviceError::Busy);
        }
        self.devices[d].dma_policy = policy;
        Ok(())
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
            clock: None,
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
            clock: None,
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
    pub fn init_clock(&mut self, base: u64) {
        let irq = self.io().platform.irq(0).expect("HPET kernel vector");
        // QEMU HPET writes through the system address space (not PCI DMA),
        // and Q35 supplies no HPET DMAR scope. This is explicit platform policy,
        // not a compatibility-interrupt fallback for PCI devices.
        let (address, data) = if option_env!("HORN_CLOCK_ROUTE") == Some("qemu-system-msi") {
            (0xfee0_0000, irq.vector)
        } else {
            match &mut self.hardware {
                Hardware::Remapping(unit) => {
                    let route = unit
                        .timer_interrupt(irq.vector.try_into().unwrap(), 0)
                        .expect("HPET interrupt source from DMAR");
                    (route.address(), route.data())
                }
                Hardware::ReadOnly(_) => (0xfee0_0000, irq.vector),
            }
        };
        let io = self.io();
        io.platform
            .bind_irq(irq.token, &io.ctx.cap("timer").unwrap())
            .expect("HPET IRQ bind");
        let map = self.io().mmio(base, 4096).expect("HPET registers");
        self.clock = Some(crate::clock::Clock::new(map, irq.token, address, data));
        self.io().platform.ack_irq(irq.token).expect("HPET IRQ arm");
    }
    pub fn clock_irq(&mut self) {
        let mut clock = self.clock.take().expect("HPET IRQ without clock");
        clock.poll(self.io());
        self.io().platform.ack_irq(clock.irq).expect("HPET IRQ ack");
        self.clock = Some(clock);
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
            .bind_irq(irq.token, &io.ctx.cap("fault").unwrap())
            .expect("fault IRQ bind");
        io.platform.ack_irq(irq.token).expect("fault IRQ arm");
        let vector = u8::try_from(irq.vector).expect("fault IRQ vector");
        self.unit()
            .fault_interrupt(vector, 0)
            .expect("fault event MSI");
        horn_runtime::logging::debug!("DMA fault reporting enabled");
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
                horn_runtime::logging::warn!(
                    "DMA fault from device {:04x} at {:#x} (reason {:#x}, read {}, overflow {})",
                    fault.source,
                    fault.address,
                    fault.reason,
                    fault.read,
                    report.overflow
                );
            } else {
                if report.overflow {
                    horn_runtime::logging::warn!("DMA fault report overflow");
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
        if badge >= horn_devmgr::CLOCK_SOURCE_BADGE {
            return Err(DeviceError::Exhausted);
        }
        self.next_badge = badge.checked_add(1).ok_or(DeviceError::Exhausted)?;
        let endpoint = self.io().ctx.cap::<Endpoint>("service").unwrap();
        let space = self.io().space();
        let original = space.reserve_slot().map_err(|_| {
            horn_runtime::logging::warn!("cannot allocate device authority {:#x}", badge);
            DeviceError::Exhausted
        })?;
        let root = space.root;
        if root
            .absolute_cptr(*original.path())
            .mint(
                &root.absolute_cptr(sel4::CPtr::from_bits(endpoint.as_ref().slot as _)),
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
                horn_runtime::logging::warn!("cannot export device authority {:#x}", badge);
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
        let root = self.io().platform.transport.space.root;
        root.absolute_cptr(sel4::CPtr::from_bits(authority.slot as _))
            .revoke()
            .expect("authority revoke");
        self.io().space().delete(authority).unwrap();
    }
    /// Retain metadata and quota until root acknowledges all-alias revocation.
    /// Uncertain cleanup is fail-stop, never slot reuse or an ordinary error.
    fn close_mmio(&mut self, device: usize, index: usize) {
        let token = self.devices[device].mmios[index].token;
        self.io()
            .platform
            .revoke_mmio(token)
            .expect("MMIO revoke acknowledgement");
        let mmio = self.devices[device].mmios.swap_remove(index);
        if let Some(memory) = mmio.memory {
            self.io().space().delete(memory.as_ref()).unwrap();
        }
        self.retire(mmio.authority);
    }
    fn mmio_index(&self, badge: u64) -> Result<(usize, usize), DeviceError> {
        self.devices
            .iter()
            .enumerate()
            .filter(|(_, device)| device.active)
            .find_map(|(d, device)| {
                device
                    .mmios
                    .iter()
                    .position(|m| m.badge == badge)
                    .map(|i| (d, i))
            })
            .ok_or(DeviceError::Denied)
    }
    fn device(&self, badge: u64) -> Result<usize, DeviceError> {
        self.devices
            .iter()
            .position(|d| d.badge == badge && d.active)
            .ok_or(DeviceError::Denied)
    }
    fn program_irq(&mut self, d: usize, route: Interrupt, token: u64) {
        let device = &self.devices[d];
        let config = device.config;
        let caps = device.capabilities;
        if let Some(map) = &device.msix {
            let table = map.address + device.msix_offset;
            unsafe {
                (table as *mut u32).add(3).write_volatile(1);
                (table as *mut u32).write_volatile(route.address() as u32);
                (table as *mut u32)
                    .add(1)
                    .write_volatile((route.address() >> 32) as u32);
                (table as *mut u32).add(2).write_volatile(route.data());
            }
            self.io()
                .platform
                .ack_irq(token)
                .expect("IRQ arm before device enable");
            let x = caps.msix.unwrap();
            let mut cfg = PciConfig {
                io: self.io(),
                base: config,
            };
            let control = cfg.read32(x.offset) >> 16;
            cfg.write(x.offset + 2, 2, (control | 0x8000) & !0x4000);
            let _ = cfg.read32(x.offset);
            unsafe {
                (table as *mut u32).add(3).write_volatile(0);
            }
        } else {
            let m = caps.msi.expect("message interrupt capability");
            m.prepare(
                &mut PciConfig {
                    io: self.io(),
                    base: config,
                },
                route.address(),
                route.data(),
            )
            .expect("MSI format");
            self.io()
                .platform
                .ack_irq(token)
                .expect("IRQ arm before MSI enable");
            m.enable(&mut PciConfig {
                io: self.io(),
                base: config,
            });
        }
    }
    fn close_map(&mut self, index: usize) {
        let map = self.maps[index].take().unwrap();
        let domain = self.devices[map.device].domain;
        // One invalidation for the whole reservation, then release every pin.
        let ranges: Vec<(u64, usize)> = map.pins.iter().map(|p| (p.iova, p.pages)).collect();
        if !ranges.is_empty() {
            self.unit()
                .unmap_many(domain, &ranges)
                .expect("DMA invalidate/drain");
        }
        for pin in map.pins {
            self.io()
                .platform
                .unpin(pin.token)
                .expect("DMA unpin after drain");
        }
        self.devices[map.device].dma_bytes -= map.length;
        self.retire(map.authority);
    }
    fn reserve_dma(
        &mut self,
        badge: u64,
        length: u64,
        access: u32,
    ) -> Result<DeviceDma, DeviceError> {
        let d = self.device(badge)?;
        if access == 0 || access & !3 != 0 {
            return Err(DeviceError::Invalid);
        }
        let objects = self.maps.iter().flatten().filter(|m| m.device == d).count();
        let device = &self.devices[d];
        if let Err(error) = device.dma_policy.admit(device.dma_bytes, objects, length) {
            horn_runtime::logging::warn!(
                "DMA quota exceeded: {} bytes in {} objects (limits {} bytes, {} objects), request {} bytes at {:#x}: {:?}",
                device.dma_bytes,
                objects,
                device.dma_policy.bytes,
                device.dma_policy.objects,
                length,
                device.next_iova,
                error
            );
            return Err(error);
        }
        let iova = device.next_iova;
        let next = iova
            .checked_add(length)
            .filter(|end| *end <= 0x8000_0000)
            .ok_or_else(|| {
                horn_runtime::logging::warn!(
                    "DMA address space exhausted at {:#x}, requested {} bytes",
                    iova,
                    length
                );
                DeviceError::Exhausted
            })?;
        let domain = device.domain;
        let mut pins = Vec::new();
        pins.try_reserve_exact(length.div_ceil(65536) as usize)
            .map_err(|_| {
                horn_runtime::logging::warn!(
                    "cannot allocate DMA pin metadata for {} bytes",
                    length
                );
                DeviceError::Exhausted
            })?;
        let index = if let Some(n) = self.maps.iter().position(Option::is_none) {
            n
        } else {
            self.maps.try_reserve(1).map_err(|_| {
                horn_runtime::logging::warn!(
                    "cannot grow DMA mapping metadata beyond {} entries",
                    self.maps.len()
                );
                DeviceError::Exhausted
            })?;
            self.maps.push(None);
            self.maps.len() - 1
        };
        let (id, authority, exported) = self.mint()?;
        let reservation = match self.unit().reserve(domain, iova, (length / 4096) as usize) {
            Ok(r) => r,
            Err(e) => {
                horn_runtime::logging::warn!(
                    "cannot reserve DMA mappings at {:#x} for {} pages: {:?}",
                    iova,
                    length / 4096,
                    e
                );
                self.retire(authority);
                self.io().space().delete(exported).unwrap();
                return Err(failure(e));
            }
        };
        self.devices[d].next_iova = next;
        self.devices[d].dma_bytes += length;
        self.maps[index] = Some(Dma {
            badge: id,
            authority,
            device: d,
            iova,
            length,
            access,
            reservation: Some(reservation),
            committed: false,
            pins,
        });
        Ok(DeviceDma {
            mapping: Cap::from_ref(exported),
            iova,
        })
    }
    /// Publish the deferred translations of one reservation with a single
    /// invalidation. The caller must not use the device address before this.
    fn commit_dma(&mut self, badge: u64) -> Result<(), DeviceError> {
        let index = self
            .maps
            .iter()
            .position(|m| m.as_ref().is_some_and(|m| m.badge == badge))
            .ok_or(DeviceError::Denied)?;
        let mut map = self.maps[index].take().ok_or(DeviceError::Denied)?;
        let result = match map.reservation.as_mut() {
            Some(reservation) => self.unit().commit_reserved(reservation).map_err(failure),
            None => Err(DeviceError::Invalid),
        };
        self.maps[index] = Some(map);
        result
    }
    fn append_dma(
        &mut self,
        badge: u64,
        memory: Cap<VmMemoryObjectTag>,
        offset: u64,
        length: u64,
    ) -> Result<u64, DeviceError> {
        if memory.as_ref().unwrapped_badge().is_some() {
            return Err(DeviceError::Denied);
        }
        let index = self
            .maps
            .iter()
            .position(|m| m.as_ref().is_some_and(|m| m.badge == badge))
            .ok_or(DeviceError::Denied)?;
        let map = self.maps[index].as_ref().unwrap();
        let r = map.reservation.as_ref().ok_or(DeviceError::Invalid)?;
        if map.committed {
            return Err(DeviceError::Busy);
        }
        let populated = r.populated() as u64 * 4096;
        if length != (map.length - populated).min(65536)
            || length == 0
            || offset & 4095 != 0
            || offset.checked_add(length).is_none()
            || length > map.length - populated
        {
            return Err(DeviceError::Invalid);
        }
        let access = map.access;
        let pin = match self.io().platform.pin(&memory, offset, length, access) {
            Ok(p) => p,
            Err(CallError::Remote(e)) => return Err(e),
            Err(_) => panic!("DMA pin transport"),
        };
        assert_eq!(pin.pages.len as u64, length / 4096, "platform pin extent");
        let mut map = self.maps[index].take().unwrap();
        let iova = map.iova + populated;
        let result = self.unit().map_reserved(
            map.reservation.as_mut().unwrap(),
            &pin.pages.data[..pin.pages.len as usize],
            access as u8,
        );
        if let Err(e) = result {
            // Keep all platform pins on uncertain hardware failure. No pin is
            // released by dropping these numeric-token records.
            if self.unit().poisoned() {
                panic!("DMA map poisoned: {:?}", e);
            }
            self.io()
                .platform
                .unpin(pin.token)
                .expect("unpublished pin release");
            self.maps[index] = Some(map);
            return Err(failure(e));
        }
        map.pins.push(Pin {
            token: pin.token,
            iova,
            pages: pin.pages.len as usize,
        });
        self.maps[index] = Some(map);
        Ok(populated + length)
    }
}
impl ClockSource for Caller<'_> {
    fn source_now(&mut self) -> Result<u64, ClockError> {
        if self.badge != horn_devmgr::CLOCK_SOURCE_BADGE {
            return Err(ClockError::Denied);
        }
        self.service
            .clock
            .as_ref()
            .map(|c| c.now())
            .ok_or(ClockError::Unavailable)
    }
    fn source_arm(
        &mut self,
        deadline_us: u64,
        signal: Cap<Notification>,
    ) -> Result<(), ClockError> {
        if self.badge != horn_devmgr::CLOCK_SOURCE_BADGE {
            return Err(ClockError::Denied);
        }
        let mut clock = self.service.clock.take().ok_or(ClockError::Unavailable)?;
        let result = clock.arm(self.service.io(), deadline_us, signal);
        self.service.clock = Some(clock);
        result
    }
    fn source_cancel(&mut self) -> Result<(), ClockError> {
        if self.badge != horn_devmgr::CLOCK_SOURCE_BADGE {
            return Err(ClockError::Denied);
        }
        let mut clock = self.service.clock.take().ok_or(ClockError::Unavailable)?;
        clock.cancel(self.service.io());
        self.service.clock = Some(clock);
        Ok(())
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
        // Two independent bus-0/function-0 endpoints (the Q35 demo profile).
        // No bridges, multifunction requester groups, restart or reassignment.
        if self.service.devices.len() >= 2 {
            return Err(DeviceError::Exhausted);
        }
        let info = self.service.inventory.info(id)?;
        let d = &info.device;
        if d.address.segment != 0
            || d.address.bus != 0
            || d.address.function != 0
            || d.header_type & 0x7f != 0
            || self.service.inventory.devices().any(|p| {
                p.address.segment == 0
                    && p.address.bus == 0
                    && p.address.device == d.address.device
                    && p.address.function != 0
            })
        {
            return Err(DeviceError::Unsupported);
        }
        let sid = ((d.address.device << 3) | d.address.function) as u16;
        if self.service.devices.iter().any(|device| device.sid == sid) {
            return Err(DeviceError::Denied);
        }
        let config = self
            .service
            .regions
            .iter()
            .find(|r| r.segment == 0 && r.first_bus == 0)
            .ok_or(DeviceError::Unsupported)?
            .address(sid, 0)?;
        let io = self.service.io();
        let command = io.config(config + 4) as u16;
        io.write(config + 4, 2, ((command & !7) | 0x400) as u64);
        let capabilities = interrupt::Capabilities::discover(&mut PciConfig { io, base: config })?;
        capabilities.disable(&mut PciConfig { io, base: config });
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
                && length <= (1u64 << 47)
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
        let (msix, msix_offset) = if let Some(x) = capabilities.msix {
            let bar = bars
                .iter()
                .find(|b| {
                    b.index == x.bir
                        && (x.table_offset as usize)
                            .checked_add(16)
                            .is_some_and(|end| end <= b.length)
                })
                .ok_or(DeviceError::Invalid)?;
            let physical = bar.address + x.table_offset as u64;
            let offset = (physical & 4095) as usize;
            let length = (offset + 16).div_ceil(4096) * 4096;
            (
                Some(
                    io.mmio(physical & !4095, length)
                        .expect("MSI-X table window"),
                ),
                offset,
            )
        } else if capabilities.msi.is_some() {
            (None, 0)
        } else {
            return Err(DeviceError::Unsupported);
        };
        let domain = self.service.unit().attach(sid).map_err(failure)?;
        let (badge, authority, exported) = self.service.mint()?;
        self.service.devices.push(Device {
            badge,
            authority,
            sid,
            config,
            domain,
            bars,
            capabilities,
            msix,
            msix_offset,
            dma_policy: DmaPolicy::DEFAULT,
            dma_bytes: 0,
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
        let length = self.service.devices[d]
            .bars
            .iter()
            .find(|b| b.index == index)
            .ok_or(DeviceError::NotFound)?
            .length;
        self.bar_window(index, 0, length as u64, space)
    }
    fn bar_window(
        &mut self,
        index: u32,
        offset: u64,
        length: u64,
        space: Cap<VmAddressSpaceObjectTag>,
    ) -> Result<Cap<VmMemoryObjectTag>, DeviceError> {
        let lease = self.map_bar_window(index, offset, length, space)?;
        // Calls are serialized: this is the window just appended above.
        // Legacy clients receive only its memory and retain until device close.
        let d = self.service.device(self.badge)?;
        let memory = self.service.devices[d]
            .mmios
            .last_mut()
            .unwrap()
            .memory
            .take()
            .unwrap();
        self.service.io().space().delete(lease.as_ref()).unwrap();
        Ok(memory)
    }
    fn map_bar_window(
        &mut self,
        index: u32,
        offset: u64,
        length: u64,
        space: Cap<VmAddressSpaceObjectTag>,
    ) -> Result<Cap<DeviceMmioObjectTag>, DeviceError> {
        let d = self.service.device(self.badge)?;
        if space.as_ref().unwrapped_badge().is_some() {
            return Err(DeviceError::Denied);
        }
        let bar = self.service.devices[d]
            .bars
            .iter()
            .find(|b| b.index == index)
            .ok_or(DeviceError::NotFound)?;
        if length == 0
            || length > 65536
            || offset & 4095 != 0
            || length & 4095 != 0
            || offset
                .checked_add(length)
                .is_none_or(|end| end > bar.length as u64)
        {
            return Err(DeviceError::Invalid);
        }
        let address = bar
            .address
            .checked_add(offset)
            .ok_or(DeviceError::Invalid)?;
        if self.service.devices[d].mmios.len() >= 8 {
            return Err(DeviceError::Exhausted);
        }
        self.service.devices[d]
            .mmios
            .try_reserve(1)
            .map_err(|_| DeviceError::Exhausted)?;
        // Reserve metadata/endpoint slots before creating any CPU aliases.
        let (badge, authority, exported) = self.service.mint()?;
        let grant = match self.service.io().platform.mmio(&space, address, length) {
            Ok(grant) => grant,
            Err(CallError::Remote(error))
                if error != DeviceError::Quarantined && error != DeviceError::Backend =>
            {
                horn_runtime::logging::warn!(
                    "cannot map BAR {} at {:#x} for {} bytes: {:?} ({} open mappings)",
                    index,
                    offset,
                    length,
                    error,
                    self.service.devices[d].mmios.len()
                );
                self.service.retire(authority);
                self.service.io().space().delete(exported).unwrap();
                return Err(error);
            }
            Err(error) => panic!("uncertain MMIO import: {:?}", error),
        };
        self.service.devices[d].mmios.push(Mmio {
            token: grant.token,
            badge,
            authority,
            memory: Some(grant.memory),
        });
        Ok(Cap::from_ref(exported))
    }
    fn map(
        &mut self,
        memory: Cap<VmMemoryObjectTag>,
        offset: u64,
        length: u64,
        access: u32,
    ) -> Result<DeviceDma, DeviceError> {
        self.service.device(self.badge)?;
        // A cap unwrapped by OUR endpoint is not VM authority. It cannot be
        // forwarded to the root VM broker; reject it before minting resources
        // instead of turning a caller-supplied wrong object into a fatal RPC.
        if memory.as_ref().unwrapped_badge().is_some() {
            return Err(DeviceError::Denied);
        }
        if length > 65536 {
            return Err(DeviceError::Invalid);
        }
        let grant = self
            .service
            .reserve_dma(self.badge, length, access)
            .map_err(|error| {
                horn_runtime::logging::warn!(
                    "cannot reserve {} bytes of DMA memory: {:?}",
                    length,
                    error
                );
                error
            })?;
        let badge = self
            .service
            .maps
            .iter()
            .flatten()
            .find(|m| m.iova == grant.iova && self.service.devices[m.device].badge == self.badge)
            .unwrap()
            .badge;
        let appended = self.service.append_dma(badge, memory, offset, length);
        if let Err(error) = &appended {
            horn_runtime::logging::warn!(
                "cannot attach {} bytes of DMA memory: {:?}",
                length,
                error
            );
        }
        match appended {
            Ok(_) => {
                // The single-shot path also owns publication: append defers the
                // IOTLB invalidation, so commit it before the caller can use the
                // device address.
                if let Err(error) = self.service.commit_dma(badge) {
                    horn_runtime::logging::warn!("cannot commit DMA mappings: {:?}", error);
                    let index = self
                        .service
                        .maps
                        .iter()
                        .position(|m| m.as_ref().is_some_and(|m| m.badge == badge))
                        .unwrap();
                    self.service.close_map(index);
                    self.service
                        .io()
                        .space()
                        .delete(grant.mapping.as_ref())
                        .unwrap();
                    return Err(error);
                }
                let map = self
                    .service
                    .maps
                    .iter_mut()
                    .flatten()
                    .find(|m| m.badge == badge)
                    .unwrap();
                map.committed = true;
                Ok(grant)
            }
            Err(e) => {
                let index = self
                    .service
                    .maps
                    .iter()
                    .position(|m| m.as_ref().is_some_and(|m| m.badge == badge))
                    .unwrap();
                self.service.close_map(index);
                self.service
                    .io()
                    .space()
                    .delete(grant.mapping.as_ref())
                    .unwrap();
                Err(e)
            }
        }
    }
    fn dma_limits(&mut self) -> Result<DeviceDmaLimits, DeviceError> {
        let d = self.service.device(self.badge)?;
        let p = self.service.devices[d].dma_policy;
        Ok(DeviceDmaLimits {
            bytes: p.bytes,
            objects: p.objects as u32,
            chunk_bytes: 65536,
        })
    }
    fn reserve_dma(&mut self, length: u64, access: u32) -> Result<DeviceDma, DeviceError> {
        self.service.reserve_dma(self.badge, length, access)
    }
    fn interrupt(&mut self) -> Result<Cap<Notification>, DeviceError> {
        let d = self.service.device(self.badge)?;
        if self.service.devices[d].irq.is_some() {
            return Err(DeviceError::Busy);
        }
        let sid = self.service.devices[d].sid;
        let domain = self.service.devices[d].domain;
        let io = self.service.io();
        let lease =
            VmFactoryClient::new(&mut io.platform.transport, io.ctx.cap("horn.vm").unwrap())
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
        let root = io.platform.transport.space.root;
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
        self.service.program_irq(d, route, irq.token);
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
        let root = self.service.io().platform.transport.space.root;
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
        self.service.program_irq(d, route, irq.token);
        self.service.devices[d].irq = Some(Irq {
            token: irq.token,
            route,
            notification,
            backing: None,
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
        if device.irq.is_none()
            || self
                .service
                .maps
                .iter()
                .flatten()
                .any(|m| m.device == d && !m.committed)
        {
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
        while !self.service.devices[d].mmios.is_empty() {
            let index = self.service.devices[d].mmios.len() - 1;
            self.service.close_mmio(d, index);
        }
        let device = &self.service.devices[d];
        let config = device.config;
        let domain = device.domain;
        let command = self.service.io().config(config + 4) as u16;
        self.service
            .io()
            .write(config + 4, 2, (command & !4) as u64);
        let caps = self.service.devices[d].capabilities;
        caps.disable(&mut PciConfig {
            io: self.service.io(),
            base: config,
        });
        if let Some(irq) = self.service.devices[d].irq.take() {
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
                for pin in map.pins {
                    self.service
                        .io()
                        .platform
                        .unpin(pin.token)
                        .expect("detached DMA unpin");
                }
                self.service.devices[d].dma_bytes -= map.length;
                self.service.retire(map.authority);
            }
        }
        if let Some(msix) = self.service.devices[d].msix.take() {
            self.service
                .io()
                .release_mmio(msix.region, msix.memory, msix.token);
        }
        let authority = self.service.devices[d].authority;
        self.service.devices[d].active = false;
        self.service.retire(authority);
        Ok(())
    }
}
impl DeviceMmio for Caller<'_> {
    fn memory(&mut self) -> Result<Cap<VmMemoryObjectTag>, DeviceError> {
        let (device, index) = self.service.mmio_index(self.badge)?;
        self.service.devices[device].mmios[index]
            .memory
            .take()
            .ok_or(DeviceError::NotFound)
    }
    fn close(&mut self) -> Result<(), DeviceError> {
        let (device, index) = self.service.mmio_index(self.badge)?;
        self.service.close_mmio(device, index);
        Ok(())
    }
}
impl DeviceMapping for Caller<'_> {
    fn append(
        &mut self,
        memory: Cap<VmMemoryObjectTag>,
        offset: u64,
        length: u64,
    ) -> Result<u64, DeviceError> {
        self.service.append_dma(self.badge, memory, offset, length)
    }
    fn finish(&mut self) -> Result<(), DeviceError> {
        let index = self
            .service
            .maps
            .iter()
            .position(|m| m.as_ref().is_some_and(|m| m.badge == self.badge))
            .ok_or(DeviceError::Denied)?;
        // Take the record out so the VT-d unit can be borrowed mutably; it is
        // always put back, including on failure.
        let mut map = self.service.maps[index].take().ok_or(DeviceError::Denied)?;
        let result = (|| {
            let reservation = map.reservation.as_mut().ok_or(DeviceError::Invalid)?;
            if !reservation.complete() {
                return Err(DeviceError::Busy);
            }
            // Publish every deferred translation now, before the caller is
            // allowed to enable bus mastering.
            self.service
                .unit()
                .commit_reserved(reservation)
                .map_err(failure)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                map.committed = true;
                self.service.maps[index] = Some(map);
                Ok(())
            }
            Err(error) => {
                self.service.maps[index] = Some(map);
                Err(error)
            }
        }
    }
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
