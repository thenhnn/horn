//! Test bootstrap mechanics. Temporary firmware/ECAM reads share a reclaimable
//! device-Untyped bank with future retained MMIO mappings; eviction never revokes
//! a source tree containing someone else's live BAR pages.
use horn_devmgr::protocol::*;
use horn_ipc::List;
use horn_vm::sel4::{
    KernelAllocator,
    device::{DeviceBank, DevicePage},
};

const LARGE_WINDOW: usize = 0x8000_0000;
pub const TEMP_SLOTS: usize = 128;
pub struct Platform {
    pub manager: horn_procd::Manager,
    enabled: bool,
    frequency: u64,
    irq_allocator: KernelAllocator,
    pins: alloc::vec::Vec<(u64, horn_procd::DmaPin)>,
    mmios: alloc::vec::Vec<(u64, horn_procd::MmioLease)>,
    next_token: u64,
    irqs: alloc::vec::Vec<Option<sel4::cap::IrqHandler>>,
    bank: DeviceBank,
    active: Option<(usize, DevicePage)>,
    poisoned: bool,
    evictions: usize,
    scratch: usize,
    rsdp: List<u8, 36>,
    pub ready: bool,
    device_ready: bool,
}
impl Platform {
    pub fn ticks_per_us(&self)->u64 {self.frequency}
    /// # Safety
    /// Device Untypeds must be unused and exclusive to this bank. `scratch` is
    /// an exclusive unmapped root page with translation tables. LARGE_WINDOW..
    /// +2MiB is reserved; the 128 slots from `first_slot` are empty and excluded
    /// from all other allocators for the bank's lifetime.
    pub unsafe fn new(
        boot: &sel4::BootInfoPtr,
        mut allocator: KernelAllocator,
        scratch: usize,
        first_slot: usize,
        manager: horn_procd::Manager,
        enabled: bool,
        frequency: u64,
    ) -> Self {
        // Exercise seL4's kernel-pointer/physical alignment difference above
        // the x86 direct-map wrap, without issuing reads to the physical hole.
        let test_page = boot
            .untyped_list()
            .iter()
            .find(|d| {
                d.is_device()
                    && d.size_bits() >= 21
                    && d.size_bits() > d.paddr().trailing_zeros() as usize
                    && d.paddr() & ((1 << 21) - 1) == 0
            })
            .or_else(|| {
                boot.untyped_list()
                    .iter()
                    .find(|d| d.is_device() && d.size_bits() >= 21)
            })
            .unwrap()
            .paddr();
        let sources = boot
            .untyped_list()
            .iter()
            .enumerate()
            .filter(|(_, d)| d.is_device() && d.size_bits() >= 12)
            .map(|(i, d)| (d.paddr(), d.size_bits(), boot.untyped().index(i).cap()));
        let bank = unsafe {
            DeviceBank::new(
                allocator.root(),
                sources,
                first_slot..first_slot + TEMP_SLOTS,
            )
        }
        .unwrap();
        allocator
            .allocate_fixed_sized::<sel4::cap_type::PageDirectory>()
            .unwrap()
            .page_directory_map(
                sel4::init_thread::slot::VSPACE.cap(),
                LARGE_WINDOW,
                sel4::VmAttributes::DEFAULT,
            )
            .unwrap();
        let content = boot
            .extra()
            .find(|e| e.id == sel4::BootInfoExtraId::X86AcpiRsdp)
            .expect("ACPI RSDP");
        let bytes = content.content();
        assert!(bytes.len() >= 20);
        let length = if bytes[15] == 0 { 20 } else { 36 };
        let mut rsdp = List::default();
        rsdp.data[..length].copy_from_slice(&bytes[..length]);
        rsdp.len = length as u32;
        // Root-owned IRQ infrastructure has root lifetime, not a restartable task's.
        let irq_allocator = allocator.clone();
        let mut this = Self {
            manager,
            enabled,
            frequency,
            irq_allocator,
            pins: alloc::vec::Vec::new(),
            mmios: alloc::vec::Vec::new(),
            next_token: 1,
            irqs: alloc::vec::Vec::new(),
            bank,
            active: None,
            poisoned: false,
            evictions: 0,
            scratch,
            rsdp,
            ready: false,
            device_ready: false,
        };
        this.verify_shared_bank(test_page, allocator);
        this
    }
    fn verify_shared_bank(&mut self, address: usize, allocator: KernelAllocator) {
        let mut pool = horn_vm::sel4::FramePool::for_account(allocator);
        let before = pool.account().usage().charged;
        let memory = self.bank.memory(address, 8192).unwrap();
        assert_eq!(memory.resident_size(), 0);
        assert_eq!(pool.account().usage().charged, before);
        let frame = pool.cap(&memory.frames()[0]).unwrap();
        frame
            .frame_map(
                sel4::init_thread::slot::VSPACE.cap(),
                self.scratch,
                sel4::CapRights::read_only(),
                sel4::VmAttributes::CACHE_DISABLED | sel4::VmAttributes::EXECUTE_NEVER,
            )
            .unwrap();
        let sibling = self.bank.page(address + 3 * 4096).unwrap();
        sibling.release().unwrap();
        let alias = self.bank.page(address).unwrap();
        alias.release().unwrap();
        assert_eq!(frame.frame_get_address().unwrap(), address);
        assert!(self.bank.large_page(address).is_err());
        assert!(!self.bank.poisoned());
        // Kernel mapping is still present: a broken parent revoke would have
        // deleted this original cap as well. No MMIO register reads are needed.
        frame.frame_unmap().unwrap();
        memory.release(&mut pool).unwrap();
        assert_eq!(self.bank.available_slots(), TEMP_SLOTS);
        self.bank.large_page(address).unwrap().release().unwrap();
        assert_eq!(self.bank.available_slots(), TEMP_SLOTS);
        sel4::debug_println!(
            "DEVMGR_BANK_PASS retained=true shared=true coalesced=true mmio_charge=0"
        );
    }
    fn token(&mut self) -> Result<u64, DeviceError> {
        let token = self.next_token;
        self.next_token = token.checked_add(1).ok_or(DeviceError::Exhausted)?;
        Ok(token)
    }
    fn irq_cap(&self, token: u64) -> Result<sel4::cap::IrqHandler, DeviceError> {
        if !self.enabled {
            return Err(DeviceError::Denied);
        }
        self.irqs
            .get(token.checked_sub(1).ok_or(DeviceError::Invalid)? as usize)
            .and_then(|value| *value)
            .ok_or(DeviceError::NotFound)
    }
    fn evict(&mut self) -> Result<(), DeviceError> {
        if self.poisoned {
            return Err(DeviceError::Backend);
        }
        if let Some((_, frame)) = self.active.take() {
            if frame.release().is_err() {
                self.poisoned = true;
                return Err(DeviceError::Backend);
            }
            self.evictions += 1;
        }
        Ok(())
    }
    fn frame(&mut self, address: usize) -> Result<(usize, sel4::cap::Granule, usize), DeviceError> {
        if self.poisoned {
            return Err(DeviceError::Backend);
        }
        if let Some((base, frame)) = &self.active {
            if address >= *base && address - base < 1usize << frame.size_bits() {
                return Ok((*base, frame.cap(), frame.size_bits()));
            }
        }
        self.evict()?;
        let large = address & !((1 << 21) - 1);
        let (base, frame) = match self.bank.large_page(large) {
            Ok(frame) => (large, frame),
            Err(horn_vm::Error::Backend) => {
                self.poisoned = true;
                return Err(DeviceError::Backend);
            }
            Err(_) => {
                let base = address & !4095;
                (
                    base,
                    self.bank.page(base).map_err(|e| match e {
                        horn_vm::Error::Backend => DeviceError::Backend,
                        horn_vm::Error::InvalidSize => DeviceError::Unsupported,
                        _ => DeviceError::Exhausted,
                    })?,
                )
            }
        };
        let result = (base, frame.cap(), frame.size_bits());
        self.active = Some((base, frame));
        Ok(result)
    }
    fn verify_returned_slots(&self) {
        assert!(self.active.is_none() && !self.poisoned && !self.bank.poisoned());
        assert_eq!(self.bank.available_slots(), TEMP_SLOTS);
        sel4::debug_println!(
            "DEVMGR_PLATFORM_PASS bounded=true returned=true peak={} evictions={}",
            self.bank.peak_slots(),
            self.evictions
        );
    }
}
impl DevicePlatform for Platform {
    fn rsdp(&mut self) -> Result<List<u8, 36>, DeviceError> {
        Ok(self.rsdp.clone())
    }
    fn read(&mut self, address: u64, length: u32) -> Result<List<u8, 256>, DeviceError> {
        if self.device_ready && !self.enabled
            || length == 0
            || length > 256
            || address.checked_add(length as u64).is_none()
        {
            return Err(DeviceError::Invalid);
        }
        let address = usize::try_from(address).map_err(|_| DeviceError::Invalid)?;
        let mut output = List::default();
        let mut done = 0;
        while done < length as usize {
            let at = address + done;
            let (page, cap, bits) = self.frame(at)?;
            let offset = at - page;
            let count = ((1usize << bits) - offset).min(length as usize - done);
            let attrs = sel4::VmAttributes::CACHE_DISABLED | sel4::VmAttributes::EXECUTE_NEVER;
            let window = if bits == 21 {
                LARGE_WINDOW
            } else {
                self.scratch
            };
            if cap
                .frame_map(
                    sel4::init_thread::slot::VSPACE.cap(),
                    window,
                    sel4::CapRights::read_only(),
                    attrs,
                )
                .is_err()
            {
                self.poisoned = true;
                return Err(DeviceError::Backend);
            }
            let pointer = (window + offset) as *const u8;
            if count == 4 && at % 4 == 0 {
                output.data[done..done + 4].copy_from_slice(
                    &unsafe { pointer.cast::<u32>().read_volatile() }.to_le_bytes(),
                );
            } else {
                for index in 0..count {
                    output.data[done + index] = unsafe { pointer.add(index).read_volatile() };
                }
            }
            if cap.frame_unmap().is_err() {
                self.poisoned = true;
                return Err(DeviceError::Backend);
            }
            done += count;
        }
        output.len = length;
        Ok(output)
    }
    fn policy(&mut self) -> Result<PlatformInfo, DeviceError> {
        Ok(PlatformInfo {
            vtd: self.enabled,
            ticks_per_us: self.frequency,
        })
    }
    fn write(&mut self, address: u64, width: u32, value: u64) -> Result<(), DeviceError> {
        if !self.enabled || !matches!(width, 1 | 2 | 4 | 8) || address & (width as u64 - 1) != 0 {
            return Err(DeviceError::Denied);
        }
        let address = usize::try_from(address).map_err(|_| DeviceError::Invalid)?;
        let (base, cap, bits) = self.frame(address)?;
        let offset = address - base;
        if offset + width as usize > 1usize << bits {
            return Err(DeviceError::Invalid);
        }
        let window = if bits == 21 {
            LARGE_WINDOW
        } else {
            self.scratch
        };
        cap.frame_map(
            sel4::init_thread::slot::VSPACE.cap(),
            window,
            sel4::CapRights::read_write(),
            sel4::VmAttributes::CACHE_DISABLED | sel4::VmAttributes::EXECUTE_NEVER,
        )
        .map_err(|_| DeviceError::Quarantined)?;
        let pointer = (window + offset) as *mut u8;
        unsafe {
            match width {
                1 => pointer.write_volatile(value as u8),
                2 => pointer.cast::<u16>().write_volatile(value as u16),
                4 => pointer.cast::<u32>().write_volatile(value as u32),
                8 => pointer.cast::<u64>().write_volatile(value),
                _ => unreachable!(),
            }
        }
        cap.frame_unmap().map_err(|_| {
            self.poisoned = true;
            DeviceError::Quarantined
        })?;
        Ok(())
    }
    fn mmio(
        &mut self,
        space: horn_ipc::Cap<VmAddressSpaceObjectTag>,
        address: u64,
        length: u64,
    ) -> Result<MmioGrant, DeviceError> {
        if !self.enabled {
            return Err(DeviceError::Denied);
        }
        if self.mmios.len() >= 64 {
            return Err(DeviceError::Exhausted);
        }
        self.evict()?;
        self.mmios
            .try_reserve(1)
            .map_err(|_| DeviceError::Exhausted)?;
        let token = self.token()?;
        let (lease, memory) = self
            .manager
            .import_mmio(
                space.as_ref(),
                &mut self.bank,
                address.try_into().map_err(|_| DeviceError::Invalid)?,
                length.try_into().map_err(|_| DeviceError::Invalid)?,
                horn_vm::protocol::VmProtection(3),
            )
            .map_err(vm_error)?;
        self.mmios.push((token, lease));
        Ok(MmioGrant {
            token,
            memory: horn_ipc::Cap::from_ref(memory.as_ref()),
        })
    }
    fn revoke_mmio(&mut self, token: u64) -> Result<(), DeviceError> {
        if !self.enabled {
            return Err(DeviceError::Denied);
        }
        let index = self
            .mmios
            .iter()
            .position(|(id, _)| *id == token)
            .ok_or(DeviceError::NotFound)?;
        let (_, lease) = self.mmios.swap_remove(index);
        self.manager.revoke_mmio(lease).map_err(vm_error)
    }
    fn pin(
        &mut self,
        memory: horn_ipc::Cap<VmMemoryObjectTag>,
        offset: u64,
        length: u64,
        access: u32,
    ) -> Result<DmaBacking, DeviceError> {
        if !self.enabled {
            return Err(DeviceError::Denied);
        }
        if self.pins.len() >= 128 {
            return Err(DeviceError::Exhausted);
        }
        self.pins
            .try_reserve(1)
            .map_err(|_| DeviceError::Exhausted)?;
        let token = self.token()?;
        let pin = self
            .manager
            .pin_dma(
                memory.as_ref(),
                offset,
                length,
                horn_vm::protocol::VmProtection(access),
            )
            .map_err(vm_error)?;
        let mut pages = List::default();
        pages.len = pin.pages().len() as u32;
        pages.data[..pin.pages().len()].copy_from_slice(pin.pages());
        self.pins.push((token, pin));
        Ok(DmaBacking { token, pages })
    }
    fn unpin(&mut self, token: u64) -> Result<(), DeviceError> {
        if !self.enabled {
            return Err(DeviceError::Denied);
        }
        let index = self
            .pins
            .iter()
            .position(|(id, _)| *id == token)
            .ok_or(DeviceError::NotFound)?;
        let (_, pin) = self.pins.swap_remove(index);
        // Private devmgr authority attests invalidation AND read/write drain.
        unsafe { self.manager.unpin_dma(pin) }.map_err(vm_error)
    }
    fn irq(&mut self, sid: u32) -> Result<IrqRoute, DeviceError> {
        if !self.enabled || sid > 255 {
            return Err(DeviceError::Denied);
        }
        if self.irqs.len() >= 32 {
            return Err(DeviceError::Exhausted);
        }
        let slot = self
            .irq_allocator
            .reserve_slot()
            .map_err(|_| DeviceError::Exhausted)?;
        let index = 0x50 + self.irqs.len();
        // pc99 IOAPIC ABI: vector = IRQ_INT_OFFSET(32)+irq_user_min(16)+index.
        if sel4::init_thread::slot::IRQ_CONTROL
            .cap()
            .irq_control_get_msi(
                0,
                ((sid >> 3) & 31) as _,
                (sid & 7) as _,
                0,
                index as _,
                &self
                    .irq_allocator
                    .root()
                    .absolute_cptr(sel4::CPtr::from_bits(slot as _)),
            )
            .is_err()
        {
            self.irq_allocator
                .release_slot(slot)
                .map_err(|_| DeviceError::Quarantined)?;
            return Err(DeviceError::Backend);
        }
        self.irqs
            .push(Some(sel4::cap::IrqHandler::from_bits(slot as _)));
        Ok(IrqRoute {
            token: self.irqs.len() as u64,
            vector: (48 + index) as u32,
        })
    }
    fn bind_irq(
        &mut self,
        token: u64,
        notification: horn_ipc::Cap<horn_ipc::Notification>,
    ) -> Result<(), DeviceError> {
        self.irq_cap(token)?
            .irq_handler_set_notification(sel4::cap::Notification::from_bits(
                notification.as_ref().slot as _,
            ))
            .map_err(|_| DeviceError::Backend)
    }
    fn ack_irq(&mut self, token: u64) -> Result<(), DeviceError> {
        self.irq_cap(token)?
            .irq_handler_ack()
            .map_err(|_| DeviceError::Backend)
    }
    fn close_irq(&mut self, token: u64) -> Result<(), DeviceError> {
        let cap = self
            .irqs
            .get_mut(token.checked_sub(1).ok_or(DeviceError::Invalid)? as usize)
            .and_then(Option::take)
            .ok_or(DeviceError::NotFound)?;
        self.irq_allocator
            .root()
            .absolute_cptr(cap.cptr())
            .revoke()
            .map_err(|_| DeviceError::Quarantined)?;
        self.irq_allocator
            .release_slot(cap.bits() as usize)
            .map_err(|_| DeviceError::Quarantined)
    }
    fn ready(&mut self) -> Result<(), DeviceError> {
        self.evict()?;
        if !self.enabled {
            self.verify_returned_slots();
        }
        self.device_ready = true;
        Ok(())
    }
}
fn vm_error(error: horn_vm::protocol::VmError) -> DeviceError {
    use horn_vm::protocol::VmError as V;
    match error {
        V::Denied => DeviceError::Denied,
        V::Busy => DeviceError::Busy,
        V::Exhausted | V::QuotaExceeded => DeviceError::Exhausted,
        V::Invalid => DeviceError::Invalid,
        V::Quarantined | V::Backend => DeviceError::Quarantined,
        _ => DeviceError::NotFound,
    }
}
