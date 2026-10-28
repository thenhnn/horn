//! Capability-backed driver resources, independent of any device protocol.
//! Explicit DMA close acknowledges invalidation/drain before copy-back or free.
//! No Drop releases hardware-accessible memory. RPC/cleanup failures are fatal:
//! callers must not catch a panic and reuse the session. Remaining VM grants and
//! pins stay retained; shutdown/reassignment policy belongs to devmgr/the driver.
#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use core::ptr::NonNull;
use horn_devmgr::protocol::*;
use horn_ipc::*;

const PAGE: usize = 4096;
const MAX_PAGES: usize = 16;

/// Permissions from the DEVICE's perspective, not CPU mapping protection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceAccess {
    Read = 1,
    Write = 2,
    ReadWrite = 3,
}
impl DeviceAccess {
    fn reads(self) -> bool {
        self as u32 & 1 != 0
    }
    fn writes(self) -> bool {
        self as u32 & 2 != 0
    }
}
#[derive(Clone, Copy)]
struct CpuMapping {
    address: usize,
    length: usize,
    region: Cap<VmRegionObjectTag>,
    memory: Cap<VmMemoryObjectTag>,
}
struct Buffer {
    cpu: CpuMapping,
    mapping: Cap<DeviceMappingObjectTag>,
    iova: u64,
    source: Option<(usize, usize, DeviceAccess)>,
}
struct Bar {
    physical: u64,
    cpu: CpuMapping,
}

/// One assigned function and the caller's own VM authority. Not a driver base
/// class: no globals, virtio knowledge, probing/matching, reset or supervisor.
pub struct DeviceSession {
    transport: Sel4Transport<Sel4CapSpace>,
    factory: Cap<VmFactoryTag>,
    space: Cap<VmAddressSpaceObjectTag>,
    device: Cap<DeviceObjectTag>,
    bars: Vec<Bar>,
    buffers: Vec<Buffer>,
    irq: Option<Cap<Notification>>,
}
impl DeviceSession {
    pub fn new(
        transport: Sel4Transport<Sel4CapSpace>,
        factory: Cap<VmFactoryTag>,
        space: Cap<VmAddressSpaceObjectTag>,
        device: Cap<DeviceObjectTag>,
    ) -> Self {
        Self {
            transport,
            factory,
            space,
            device,
            bars: Vec::new(),
            buffers: Vec::new(),
            irq: None,
        }
    }
    /// Caller-owned application IPC transport for additional scoped protocols.
    pub fn transport(&mut self) -> &mut Sel4Transport<Sel4CapSpace> { &mut self.transport }
    /// Scoped control only; never assignment, writable ECAM or RAM paddr access.
    pub fn control(&mut self) -> DeviceControlClient<&mut Sel4Transport<Sel4CapSpace>> {
        DeviceControlClient::new(&mut self.transport, Cap::from_ref(self.device.as_ref()))
    }
    fn map_cpu(&mut self, memory: Cap<VmMemoryObjectTag>, length: usize) -> CpuMapping {
        let region =
            VmAddressSpaceClient::new(&mut self.transport, Cap::from_ref(self.space.as_ref()))
                .reserve(VmRegionRequest {
                    address: 0,
                    length: length as _,
                    alignment: PAGE as _,
                    placement: VmPlacement::Anywhere,
                    ceiling: VmProtection(3),
                })
                .expect("CPU region");
        let address = VmRegionClient::new(&mut self.transport, Cap::from_ref(region.as_ref()))
            .info()
            .expect("CPU region info")
            .address
            .try_into()
            .expect("CPU address");
        let mut updates = List::default();
        updates.len = 2;
        updates.data[0] = VmUpdate::Prepare(VmRange {
            offset: 0,
            length: length as _,
        });
        updates.data[1] = VmUpdate::Map(VmMap {
            offset: 0,
            length: length as _,
            memory_offset: 0,
            protection: VmProtection(3),
        });
        VmRegionClient::new(&mut self.transport, Cap::from_ref(region.as_ref()))
            .apply(
                Some(Cap::from_ref(memory.as_ref())),
                Parcel::inline(updates),
            )
            .expect("CPU map");
        CpuMapping {
            address,
            length,
            region,
            memory,
        }
    }
    pub fn map_bars(&mut self, resources: &PciResources) {
        assert!(self.bars.is_empty(), "BARs already mapped");
        assert_eq!(resources.configuration.len, 64);
        assert_eq!(resources.bar_lengths.len, 6);
        self.bars.try_reserve(6).expect("BAR metadata");
        for index in 0..6 {
            let length = resources.bar_lengths.data[index];
            if length == 0 {
                continue;
            }
            let low = resources.configuration.data[4 + index];
            assert_eq!(low & 1, 0);
            let high = if low & 6 == 4 {
                assert!(index < 5);
                resources.configuration.data[5 + index]
            } else {
                0
            };
            let physical = ((high as u64) << 32) | (low as u64 & !15);
            let space = self.space;
            let memory = self.control().bar(index as _, &space).expect("BAR grant");
            let cpu = self.map_cpu(memory, length.try_into().expect("BAR size"));
            self.bars.push(Bar { physical, cpu });
        }
    }
    /// Resolve only within CPU-mapped BAR grants. The numeric bus address is
    /// a selector, not authority to map arbitrary physical memory.
    pub fn mmio(&self, physical: u64, length: usize) -> NonNull<u8> {
        for bar in &self.bars {
            if let Some(offset) = span(bar.physical, bar.cpu.length, physical, length) {
                let address = bar
                    .cpu
                    .address
                    .checked_add(offset)
                    .expect("MMIO address overflow");
                return NonNull::new(address as *mut u8).expect("MMIO address");
            }
        }
        panic!("MMIO outside owned BAR grants");
    }
    /// Retain the notification grant in this session; caller can wait, not signal.
    pub fn interrupt(&mut self) -> sel4::cap::Notification {
        if self.irq.is_none() {
            self.irq = Some(self.control().interrupt().expect("remapped IRQ"));
        }
        self.transport
            .space
            .cap(self.irq.unwrap().as_ref())
            .expect("IRQ notification")
    }
    /// Use a caller-owned notification; signal and receive rights may be split.
    pub fn interrupt_on(&mut self, signal: &Cap<Notification>, receive: Cap<Notification>) -> sel4::cap::Notification {
        assert!(self.irq.is_none());
        self.control().interrupt_to(signal).expect("remapped IRQ binding");
        self.irq = Some(receive);
        self.interrupt()
    }
    /// # Safety
    /// Device-specific reset/quiescence must have completed. Discard outstanding
    /// bounce output without dereferencing possibly retired source buffers.
    pub unsafe fn discard_shared(&mut self) {
        while let Some(index) = self.buffers.iter().position(|b| b.source.is_some()) {
            self.release(index, false);
        }
        assert!(self.buffers.is_empty(), "driver still owns DMA allocations");
    }
    pub fn activate(&mut self) {
        self.control().activate().expect("bus mastering");
    }
    pub fn ack_interrupt(&mut self) {
        self.control().ack().expect("IRQ rearm");
    }

    fn allocate(
        &mut self,
        pages: usize,
        access: DeviceAccess,
        source: Option<(usize, usize, DeviceAccess)>,
    ) -> (u64, NonNull<u8>) {
        let length = allocation_length(pages);
        self.buffers.try_reserve(1).expect("DMA metadata");
        let memory = VmFactoryClient::new(&mut self.transport, self.factory)
            .allocate(length as _, VmBacking::Resident, VmProtection(3))
            .expect("resident DMA backing");
        let cpu = self.map_cpu(memory, length);
        let dma = self
            .control()
            .map(
                &Cap::from_ref(cpu.memory.as_ref()),
                0,
                length as _,
                access as u32,
            )
            .expect("DMA map");
        let pointer = NonNull::new(cpu.address as *mut u8).expect("DMA CPU address");
        self.buffers.push(Buffer {
            cpu,
            mapping: dma.mapping,
            iova: dma.iova,
            source,
        });
        (dma.iova, pointer)
    }
    /// Fresh zeroed resident pages. The returned bus address is an IOVA, NEVER
    /// a RAM physical address. DMA pins do not charge resident backing twice.
    pub fn dma_alloc(&mut self, pages: usize, access: DeviceAccess) -> (u64, NonNull<u8>) {
        self.allocate(pages, access, None)
    }
    /// # Safety
    /// No CPU references to these pages may survive this call. Arguments must
    /// match a live dma_alloc result; hardware access is removed/drained here.
    pub unsafe fn dma_dealloc(&mut self, iova: u64, pointer: NonNull<u8>, pages: usize) {
        let index = self.find(iova);
        let b = &self.buffers[index];
        assert!(b.source.is_none());
        assert_eq!(b.cpu.address, pointer.as_ptr() as usize);
        assert_eq!(b.cpu.length, allocation_length(pages));
        self.release(index, false);
    }
    /// Independent directional bounce, not direct pinning of caller stack/heap.
    /// # Safety
    /// buffer must remain live and CPU-exclusive for its full length until the
    /// matching unshare. Device-writable bytes must be writable by the caller.
    pub unsafe fn share(&mut self, buffer: NonNull<[u8]>, access: DeviceAccess) -> u64 {
        let length = buffer.len();
        assert!(length != 0 && length <= MAX_PAGES * PAGE);
        let source = buffer.as_ptr() as *mut u8;
        let (iova, bounce) = self.allocate(
            length.div_ceil(PAGE),
            access,
            Some((source as usize, length, access)),
        );
        if access.reads() {
            unsafe {
                core::ptr::copy_nonoverlapping(source, bounce.as_ptr(), length);
            }
        }
        iova
    }
    /// # Safety
    /// Must match a live share, including original pointer, length and access.
    /// The source remains exclusive until this returns. Close/drain completes
    /// BEFORE write copy-back, followed by CPU unmap and backing release.
    pub unsafe fn unshare(&mut self, iova: u64, buffer: NonNull<[u8]>, access: DeviceAccess) {
        let index = self.find(iova);
        assert_eq!(
            self.buffers[index].source,
            Some((buffer.as_ptr() as *mut u8 as usize, buffer.len(), access))
        );
        self.release(index, true);
    }
    fn find(&self, iova: u64) -> usize {
        self.buffers
            .iter()
            .position(|b| b.iova == iova)
            .expect("live DMA allocation")
    }
    fn release(&mut self, index: usize, copy_back: bool) {
        let buffer = &self.buffers[index];
        // Leave the record and all CPU backing retained on uncertain close.
        DeviceMappingClient::new(&mut self.transport, Cap::from_ref(buffer.mapping.as_ref()))
            .close()
            .expect("DMA invalidate/drain");
        self.transport
            .space
            .delete(buffer.mapping.as_ref())
            .unwrap();
        if copy_back {
            let (source, length, access) = buffer.source.expect("shared buffer");
            if access.writes() {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        buffer.cpu.address as *const u8,
                        source as *mut u8,
                        length,
                    );
                }
            }
        }
        let cpu = buffer.cpu;
        VmRegionClient::new(&mut self.transport, Cap::from_ref(cpu.region.as_ref()))
            .destroy()
            .expect("CPU unmap");
        self.transport.space.delete(cpu.region.as_ref()).unwrap();
        VmMemoryClient::new(&mut self.transport, Cap::from_ref(cpu.memory.as_ref()))
            .close()
            .expect("RAM authority close");
        self.transport.space.delete(cpu.memory.as_ref()).unwrap();
        self.buffers.swap_remove(index);
    }
}
fn allocation_length(pages: usize) -> usize {
    assert!(
        pages != 0 && pages <= MAX_PAGES,
        "DMA allocation exceeds bounded pin range"
    );
    pages * PAGE
}
fn span(base: u64, length: usize, address: u64, size: usize) -> Option<usize> {
    if size == 0 {
        return None;
    }
    base.checked_add(u64::try_from(length).ok()?)?;
    let offset = usize::try_from(address.checked_sub(base)?).ok()?;
    offset
        .checked_add(size)
        .filter(|end| *end <= length)
        .map(|_| offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_permissions_are_not_cpu_direction() {
        assert!(DeviceAccess::Read.reads());
        assert!(!DeviceAccess::Read.writes());
        assert!(!DeviceAccess::Write.reads());
        assert!(DeviceAccess::Write.writes());
        assert!(DeviceAccess::ReadWrite.reads() && DeviceAccess::ReadWrite.writes());
        assert_eq!(DeviceAccess::Read as u32, 1);
        assert_eq!(DeviceAccess::Write as u32, 2);
        assert_eq!(DeviceAccess::ReadWrite as u32, 3);
    }
    #[test]
    fn mmio_bounds_include_64_bit_bars_and_reject_overflow() {
        let base = 0x1_0000_0000;
        assert_eq!(span(base, 4096, base + 4092, 4), Some(4092));
        assert_eq!(span(base, 4096, base - 1, 1), None);
        assert_eq!(span(base, 4096, base, 0), None);
        assert_eq!(span(base, 4096, base + 4096, 1), None);
        assert_eq!(span(base, 4096, base + 1, usize::MAX), None);
        assert_eq!(span(base, 4096, u64::MAX, 2), None);
        assert_eq!(span(u64::MAX, 4096, u64::MAX, 1), None);
    }
    #[test]
    fn dma_length_matches_the_bounded_pin_contract() {
        assert_eq!(allocation_length(1), 4096);
        assert_eq!(allocation_length(16), 65536);
    }
    #[test]
    #[should_panic]
    fn no_zero_page_dma() {
        allocation_length(0);
    }
    #[test]
    #[should_panic]
    fn no_oversized_dma() {
        allocation_length(usize::MAX);
    }
}
