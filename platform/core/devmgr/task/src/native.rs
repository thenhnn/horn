//! Native VM/physical bootstrap transport. Only devmgr sees physical addresses.
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use horn_devmgr::{
    protocol::*,
    vtd::{self, Hardware, Page},
};
use horn_ipc::*;
pub struct Mapping {
    pub address: usize,
    pub length: usize,
    pub region: OwnedCap<VmRegionObjectTag>,
    pub memory: OwnedCap<VmMemoryObjectTag>,
    pub token: u64,
}
pub struct Io {
    pub ctx: horn_runtime::Context,
    pub boot: horn_supervisor::client::Bootstrap,
    pub platform: Cap<DevicePlatformObjectTag>,
    pub transport: Sel4Transport<Sel4CapSpace>,
    pub registers: Option<Mapping>,
    pub base: u64,
    pages: Vec<Mapping>,
}
fn vm_error(error: CallError<VmError>) -> vtd::Error {
    match error {
        CallError::Remote(VmError::Exhausted | VmError::QuotaExceeded) => vtd::Error::Exhausted,
        _ => vtd::Error::Backend,
    }
}
impl Io {
    pub fn new(
        transport: Sel4Transport<Sel4CapSpace>,
        platform: Cap<DevicePlatformObjectTag>,
        ctx: horn_runtime::Context,
    ) -> Self {
        let boot = horn_supervisor::client::Bootstrap {
            cnode: transport.space.root.bits() as _,
            bits: ctx.cspace_bits(),
            vm: ctx.cap::<Endpoint>("horn.vm").unwrap().as_ref().slot,
            space: ctx
                .cap::<Endpoint>("horn.address-space")
                .unwrap()
                .as_ref()
                .slot,
        };
        Self {
            platform,
            transport,
            ctx,
            boot,
            registers: None,
            base: 0,
            pages: Vec::new(),
        }
    }
    pub fn space(&mut self) -> &mut Sel4CapSpace {
        &mut self.transport.space
    }
    pub fn map_cpu(
        &mut self,
        memory: OwnedCap<VmMemoryObjectTag>,
        length: usize,
        token: u64,
    ) -> vtd::Result<Mapping> {
        let region = self
            .ctx
            .object::<VmAddressSpaceObjectTag>("horn.address-space")
            .unwrap()
            .reserve(
                &mut self.transport,
                VmRegionRequest {
                    address: 0,
                    length: length as u64,
                    alignment: 4096,
                    placement: VmPlacement::Anywhere,
                    ceiling: VmProtection(3),
                },
            )
            .map_err(vm_error)?;
        let address = region.info(&mut self.transport).map_err(vm_error)?.address as usize;
        let mut updates = List::default();
        updates.len = 2;
        updates.data[0] = VmUpdate::Prepare(VmRange {
            offset: 0,
            length: length as u64,
        });
        updates.data[1] = VmUpdate::Map(VmMap {
            offset: 0,
            length: length as u64,
            memory_offset: 0,
            protection: VmProtection(3),
        });
        region
            .apply(
                &mut self.transport,
                Some(Cap::from_ref(memory.cap_ref())),
                Parcel::inline(updates),
            )
            .map_err(|_| vtd::Error::Backend)?;
        Ok(Mapping {
            address,
            length,
            region,
            memory,
            token,
        })
    }
    /// Release a quiesced, no-longer-used internal MMIO mapping and its lease.
    pub fn release_mmio(
        &mut self,
        region: OwnedCap<VmRegionObjectTag>,
        memory: OwnedCap<VmMemoryObjectTag>,
        token: u64,
    ) {
        region
            .destroy(&mut self.transport)
            .expect("internal MMIO CPU unmap");
        drop(region);
        memory
            .close(&mut self.transport)
            .expect("internal MMIO memory close");
        drop(memory);
        {
            let io = &mut *self;
            io.platform.revoke_mmio(&mut io.transport, token)
        }
        .expect("internal MMIO lease revoke");
    }
    pub fn mmio(&mut self, base: u64, length: usize) -> vtd::Result<Mapping> {
        let grant = {
            let io = &mut *self;
            io.platform.mmio(
                &mut io.transport,
                &io.ctx.cap("horn.address-space").unwrap(),
                base,
                length as u64,
            )
        }
        .map_err(|_| vtd::Error::Backend)?;
        self.map_cpu(grant.memory, length, grant.token)
    }
    pub fn initialize(&mut self, base: u64) {
        let cpu = unsafe { core::arch::x86_64::__cpuid(1) };
        assert!(
            cpu.edx & (1 << 19) != 0 && (cpu.ebx >> 8) & 255 == 8,
            "64-byte CLFLUSH support"
        );
        self.base = base;
        self.registers = Some(self.mmio(base, 65536).expect("IOMMU registers"));
    }
    fn pointer(&self, address: u64, width: usize) -> vtd::Result<*mut u8> {
        let map = self.registers.as_ref().ok_or(vtd::Error::Invalid)?;
        let offset = address.checked_sub(self.base).ok_or(vtd::Error::Invalid)? as usize;
        if offset.checked_add(width).is_none_or(|end| end > map.length)
            || address & (width as u64 - 1) != 0
        {
            return Err(vtd::Error::Invalid);
        }
        Ok((map.address + offset) as *mut u8)
    }
    pub fn config(&mut self, address: u64) -> u32 {
        let bytes = {
            let io = &mut *self;
            io.platform.read(&mut io.transport, address, 4)
        }
        .expect("PCI config read");
        assert_eq!(bytes.len, 4);
        u32::from_le_bytes(bytes.data[..4].try_into().unwrap())
    }
    pub fn write(&mut self, address: u64, width: u32, value: u64) {
        {
            let io = &mut *self;
            io.platform.write(&mut io.transport, address, width, value)
        }
        .expect("PCI config write");
    }
}
unsafe impl Hardware for Io {
    fn read32(&mut self, address: u64) -> vtd::Result<u32> {
        Ok(unsafe { self.pointer(address, 4)?.cast::<u32>().read_volatile() })
    }
    fn read64(&mut self, address: u64) -> vtd::Result<u64> {
        Ok(unsafe { self.pointer(address, 8)?.cast::<u64>().read_volatile() })
    }
    fn write32(&mut self, address: u64, value: u32) -> vtd::Result<()> {
        fence(Ordering::SeqCst);
        unsafe {
            self.pointer(address, 4)?
                .cast::<u32>()
                .write_volatile(value)
        };
        Ok(())
    }
    fn write64(&mut self, address: u64, value: u64) -> vtd::Result<()> {
        fence(Ordering::SeqCst);
        unsafe {
            self.pointer(address, 8)?
                .cast::<u64>()
                .write_volatile(value)
        };
        Ok(())
    }
    fn allocate_page(&mut self) -> vtd::Result<Page> {
        self.pages
            .try_reserve(1)
            .map_err(|_| vtd::Error::Exhausted)?;
        let memory = self
            .ctx
            .object::<VmServiceTag>("horn.vm")
            .unwrap()
            .allocate(
                &mut self.transport,
                4096,
                VmBacking::Resident,
                VmProtection(3),
            )
            .map_err(vm_error)?;
        let grant = {
            let io = &mut *self;
            io.platform.pin(&mut io.transport, &memory, 0, 4096, 3)
        }
        .map_err(|e| match e {
            CallError::Remote(DeviceError::Exhausted) => vtd::Error::Exhausted,
            _ => vtd::Error::Backend,
        })?;
        if grant.pages.len != 1 {
            return Err(vtd::Error::Backend);
        }
        let map = self.map_cpu(memory, 4096, grant.token)?;
        // The kernel zeroed RAM through a cached alias. Publish all cache lines
        // before exposing even an initially empty table to non-snooping VT-d.
        for offset in (0..4096).step_by(64) {
            unsafe {
                core::arch::x86_64::_mm_clflush((map.address + offset) as *const u8);
            }
        }
        fence(Ordering::SeqCst);
        let page = Page {
            cookie: self.pages.len() as u32,
            physical: grant.pages.data[0],
        };
        self.pages.push(map);
        Ok(page)
    }
    fn read_page(&mut self, page: Page, word: usize) -> vtd::Result<u64> {
        if word >= 512 {
            return Err(vtd::Error::Invalid);
        }
        let map = self
            .pages
            .get(page.cookie as usize)
            .ok_or(vtd::Error::Invalid)?;
        unsafe {
            core::arch::x86_64::_mm_clflush((map.address as *const u64).add(word).cast());
        }
        fence(Ordering::SeqCst);
        let value = unsafe { (map.address as *const u64).add(word).read_volatile() };
        fence(Ordering::Acquire);
        Ok(value)
    }
    fn write_page(&mut self, page: Page, word: usize, value: u64) -> vtd::Result<()> {
        if word >= 512 {
            return Err(vtd::Error::Invalid);
        }
        let map = self
            .pages
            .get(page.cookie as usize)
            .ok_or(vtd::Error::Invalid)?;
        unsafe {
            let pointer = (map.address as *mut u64).add(word);
            pointer.write_volatile(value);
            core::arch::x86_64::_mm_clflush(pointer.cast());
        }
        fence(Ordering::SeqCst);
        Ok(())
    }
    fn now(&self) -> u64 {
        unsafe { core::arch::x86_64::_rdtsc() }
    }
}
