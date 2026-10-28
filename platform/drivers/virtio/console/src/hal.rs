//! Single-device capability HAL. RAM PhysAddr means IOVA, never a RAM paddr.
use alloc::vec::Vec;
use core::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};
use horn_devmgr::protocol::*;
use horn_ipc::*;
use horn_virtio::{BufferDirection, Hal, PhysAddr};

pub struct CpuMapping {
    pub address: usize,
    pub length: usize,
    region: Cap<VmRegionObjectTag>,
    memory: Cap<VmMemoryObjectTag>,
}
struct Buffer {
    cpu: CpuMapping,
    mapping: Cap<DeviceMappingObjectTag>,
    iova: u64,
    source: Option<(usize, usize, BufferDirection)>,
}
struct Bar {
    physical: u64,
    cpu: CpuMapping,
}
pub struct State {
    pub transport: Sel4Transport<Sel4CapSpace>,
    bars: Vec<Bar>,
    buffers: Vec<Buffer>,
    #[cfg(feature = "dma-probe")]
    stale: Option<u64>,
    #[cfg(feature = "dma-probe")]
    injected: bool,
}
pub fn target<T: CapType>(slot: u32) -> Cap<T> {
    Cap::new(
        DEVICE_DRIVER_CAPS_SELF_SLOT as _,
        slot,
        sel4::WORD_SIZE as _,
        0,
        CapMode::COPY,
        Rights::ALL,
    )
}
impl State {
    pub fn new(transport: Sel4Transport<Sel4CapSpace>) -> Self {
        Self {
            transport,
            bars: Vec::new(),
            buffers: Vec::new(),
            #[cfg(feature = "dma-probe")]
            stale: None,
            #[cfg(feature = "dma-probe")]
            injected: false,
        }
    }
    pub fn device(&mut self) -> DeviceControlClient<&mut Sel4Transport<Sel4CapSpace>> {
        DeviceControlClient::new(&mut self.transport, target(DEVICE_DRIVER_CAPS_DEVICE_SLOT))
    }
    fn map_cpu(&mut self, memory: Cap<VmMemoryObjectTag>, length: usize) -> CpuMapping {
        let region = VmAddressSpaceClient::new(
            &mut self.transport,
            target(DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT),
        )
        .reserve(VmRegionRequest {
            address: 0,
            length: length as _,
            alignment: 4096,
            placement: VmPlacement::Anywhere,
            ceiling: VmProtection(3),
        })
        .expect("CPU region");
        let address = VmRegionClient::new(&mut self.transport, Cap::from_ref(region.as_ref()))
            .info()
            .expect("CPU region info")
            .address as usize;
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
        assert_eq!(resources.configuration.len, 64);
        assert_eq!(resources.bar_lengths.len, 6);
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
            let memory = self
                .device()
                .bar(index as _, &target(DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT))
                .expect("BAR grant");
            let cpu = self.map_cpu(memory, length.try_into().expect("BAR size"));
            self.bars.push(Bar { physical, cpu });
        }
    }
    pub fn mmio(&self, physical: u64, length: usize) -> NonNull<u8> {
        assert!(length != 0);
        for bar in &self.bars {
            if let Some(offset) = physical
                .checked_sub(bar.physical)
                .and_then(|o| usize::try_from(o).ok())
            {
                if offset
                    .checked_add(length)
                    .is_some_and(|end| end <= bar.cpu.length)
                {
                    return NonNull::new((bar.cpu.address + offset) as *mut u8).unwrap();
                }
            }
        }
        panic!("MMIO outside owned BAR grants");
    }
    fn allocate(
        &mut self,
        pages: usize,
        direction: BufferDirection,
        source: Option<(usize, usize, BufferDirection)>,
    ) -> (PhysAddr, NonNull<u8>) {
        assert!(
            pages != 0 && pages <= 16,
            "DMA allocation exceeds bounded pin range"
        );
        self.buffers.try_reserve(1).expect("DMA metadata");
        let length = pages * 4096;
        let memory = VmFactoryClient::new(&mut self.transport, target(DEVICE_DRIVER_CAPS_VM_SLOT))
            .allocate(length as _, VmBacking::Resident, VmProtection(3))
            .expect("resident DMA backing");
        let cpu = self.map_cpu(memory, length);
        let access = match direction {
            BufferDirection::DriverToDevice => 1,
            BufferDirection::DeviceToDriver => 2,
            BufferDirection::Both => 3,
        };
        let dma = self
            .device()
            .map(&Cap::from_ref(cpu.memory.as_ref()), 0, length as _, access)
            .expect("DMA map");
        let pointer = NonNull::new(cpu.address as *mut u8).unwrap();
        self.buffers.push(Buffer {
            cpu,
            mapping: dma.mapping,
            iova: dma.iova,
            source,
        });
        (dma.iova, pointer)
    }
    fn unmap(&mut self, iova: u64) -> Buffer {
        let index = self
            .buffers
            .iter()
            .position(|b| b.iova == iova)
            .expect("live DMA allocation");
        let buffer = &self.buffers[index];
        // Failure panics with backing still retained. Never free uncertain DMA.
        DeviceMappingClient::new(&mut self.transport, Cap::from_ref(buffer.mapping.as_ref()))
            .close()
            .expect("DMA invalidate/drain");
        self.transport
            .space
            .delete(buffer.mapping.as_ref())
            .unwrap();
        self.buffers.swap_remove(index)
    }
    fn free_cpu(&mut self, cpu: CpuMapping) {
        VmRegionClient::new(&mut self.transport, Cap::from_ref(cpu.region.as_ref()))
            .destroy()
            .expect("CPU unmap");
        self.transport.space.delete(cpu.region.as_ref()).unwrap();
        VmMemoryClient::new(&mut self.transport, Cap::from_ref(cpu.memory.as_ref()))
            .close()
            .expect("RAM authority close");
        self.transport.space.delete(cpu.memory.as_ref()).unwrap();
    }
}
struct Storage(UnsafeCell<Option<State>>);
// Access is serialized by BUSY; install/driver runtime are single-threaded.
unsafe impl Sync for Storage {}
static STATE: Storage = Storage(UnsafeCell::new(None));
static BUSY: AtomicBool = AtomicBool::new(false);
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}
pub fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    assert!(
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok(),
        "HAL reentry"
    );
    let _guard = Guard;
    f(unsafe { (&mut *STATE.0.get()).as_mut().expect("HAL installed") })
}
pub fn install(state: State) {
    assert!(
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    );
    let _guard = Guard;
    let slot = unsafe { &mut *STATE.0.get() };
    assert!(slot.is_none());
    *slot = Some(state);
}
pub struct CapabilityHal;
// SAFETY: fresh resident VM backing is page-aligned, zeroed, CPU-disjoint and
// pinned with device-direction permissions. IOVAs are never reused. Explicit
// unmap acknowledges invalidation/drain before any backing is freed. MMIO is
// bounded to owned BARs. Ordinary buffers use independent directional bounces.
unsafe impl Hal for CapabilityHal {
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        with(|state| state.allocate(pages, direction, None))
    }
    unsafe fn dma_dealloc(iova: PhysAddr, pointer: NonNull<u8>, pages: usize) -> i32 {
        with(|state| {
            let b = state
                .buffers
                .iter()
                .find(|b| b.iova == iova)
                .expect("DMA allocation");
            assert!(b.source.is_none());
            assert_eq!(b.cpu.address, pointer.as_ptr() as usize);
            assert_eq!(b.cpu.length, pages * 4096);
            let b = state.unmap(iova);
            state.free_cpu(b.cpu);
            0
        })
    }
    unsafe fn mmio_phys_to_virt(physical: PhysAddr, size: usize) -> NonNull<u8> {
        with(|state| state.mmio(physical, size))
    }
    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
        let length = buffer.len();
        assert!(length != 0 && length <= 16 * 4096);
        with(|state| {
            let source = buffer.as_ptr() as *mut u8;
            let (iova, bounce) = state.allocate(
                length.div_ceil(4096),
                direction,
                Some((source as usize, length, direction)),
            );
            if direction != BufferDirection::DeviceToDriver {
                unsafe { core::ptr::copy_nonoverlapping(source, bounce.as_ptr(), length) };
            }
            #[cfg(feature = "dma-probe")]
            if direction == BufferDirection::DriverToDevice {
                match option_env!("HORN_DMA_PROBE") {
                    Some("outside") => {
                        sel4::debug_println!("DMA_PROBE outside=true address=dead0000");
                        state.injected = true;
                        return 0xdead_0000;
                    }
                    Some("stale") => {
                        if let Some(stale) = state.stale {
                            state.injected = true;
                            sel4::debug_println!(
                                "DMA_PROBE stale=true drained=true warmed=true address={:x}",
                                stale
                            );
                            return stale;
                        }
                    }
                    _ => {}
                }
            }
            iova
        })
    }
    unsafe fn unshare(iova: PhysAddr, buffer: NonNull<[u8]>, direction: BufferDirection) {
        // An intentionally rejected descriptor is not a valid completion. The
        // isolated negative-test task stops here without freeing further backing
        // or interpreting device error completion data as a normal operation.
        with(|state| {
            #[cfg(feature = "dma-probe")]
            if state.injected {
                loop {
                    sel4::r#yield();
                }
            }
            let b = state
                .buffers
                .iter()
                .find(|b| b.iova == iova)
                .expect("shared DMA allocation");
            assert_eq!(
                b.source,
                Some((buffer.as_ptr() as *mut u8 as usize, buffer.len(), direction))
            );
            let b = state.unmap(iova);
            if direction != BufferDirection::DriverToDevice {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        b.cpu.address as *const u8,
                        buffer.as_ptr() as *mut u8,
                        buffer.len(),
                    )
                };
            }
            state.free_cpu(b.cpu);
            #[cfg(feature = "dma-probe")]
            if direction == BufferDirection::DriverToDevice
                && option_env!("HORN_DMA_PROBE") == Some("stale")
            {
                state.stale = Some(iova);
            }
        })
    }
}
