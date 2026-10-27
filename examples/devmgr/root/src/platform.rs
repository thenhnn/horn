//! Read-only test bootstrap broker. A single cached frame uses a private,
//! bounded CSlot pool. Eviction revokes the whole temporary derivation tree
//! before returning any slot. No source Untyped is shared with another user.
use alloc::vec::Vec;
use horn_devmgr::protocol::*;
use horn_ipc::{CapMode, CapRef, CapSpace, List, Rights, Sel4CapSpace};
use horn_vm::sel4::KernelAllocator;
use sel4::CapTypeForObjectOfFixedSize;

const LARGE_WINDOW: usize = 0x8000_0000;
pub const TEMP_SLOTS: usize = 128;
#[derive(Clone, Copy)]
struct Region {
    address: usize,
    bits: usize,
    cap: sel4::cap::Untyped,
}
#[derive(Clone, Copy)]
struct Frame {
    bits: usize,
    slot: usize,
}
struct Lease {
    source: sel4::cap::Untyped,
    base: usize,
    frame: Option<Frame>,
    slots: Vec<CapRef>,
}
pub struct Platform {
    root: sel4::cap::CNode,
    regions: Vec<Region>,
    slots: Sel4CapSpace,
    active: Option<Lease>,
    poisoned: bool,
    peak: usize,
    evictions: usize,
    scratch: usize,
    rsdp: List<u8, 36>,
    pub ready: bool,
}
impl Platform {
    /// # Safety
    /// Device Untypeds must be exclusive to this broker. `scratch` is an
    /// exclusive unmapped root page with translation tables. LARGE_WINDOW..+2MiB
    /// is exclusively reserved; the 128 slots starting at `first_slot` are empty
    /// and excluded from all other allocators for the broker's lifetime.
    pub unsafe fn new(
        boot: &sel4::BootInfoPtr,
        mut allocator: KernelAllocator,
        scratch: usize,
        first_slot: usize,
    ) -> Self {
        let regions = boot
            .untyped_list()
            .iter()
            .enumerate()
            .filter(|(_, d)| d.is_device() && d.size_bits() >= 12)
            .map(|(i, d)| Region {
                address: d.paddr(),
                bits: d.size_bits(),
                cap: boot.untyped().index(i).cap(),
            })
            .collect();
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
        let root = allocator.root();
        Self {
            root,
            regions,
            slots: Sel4CapSpace::new_local(root, first_slot as _, (first_slot + TEMP_SLOTS) as _),
            active: None,
            poisoned: false,
            peak: 0,
            evictions: 0,
            scratch,
            rsdp,
            ready: false,
        }
    }
    fn reserve(&mut self) -> Result<usize, DeviceError> {
        let slot = self
            .slots
            .reserve_slot()
            .map_err(|_| DeviceError::Exhausted)?
            .path()
            .bits() as usize;
        let lease = self.active.as_mut().unwrap();
        lease.slots.push(CapRef::new(
            self.root.bits() as _,
            slot as _,
            sel4::WORD_SIZE as _,
            0,
            CapMode::MOVE,
            Rights::ALL,
        ));
        self.peak = self.peak.max(lease.slots.len());
        Ok(slot)
    }
    fn evict(&mut self) -> Result<(), DeviceError> {
        if self.poisoned {
            return Err(DeviceError::Backend);
        }
        if let Some(lease) = self.active.as_mut() {
            // All reads unmap before reaching here. Revoke also removes every
            // derived frame/Untyped, including a partially constructed tree.
            // On failure retain the lease and slots: never reuse uncertain state.
            self.poisoned = true;
            self.root
                .absolute_cptr(lease.source.cptr())
                .revoke()
                .map_err(|_| DeviceError::Backend)?;
            while let Some(&cap) = lease.slots.last() {
                self.slots.delete(cap).map_err(|_| DeviceError::Backend)?;
                lease.slots.pop();
            }
            self.active = None;
            self.poisoned = false;
            self.evictions += 1;
        }
        Ok(())
    }
    fn frame(&mut self, address: usize) -> Result<(usize, Frame), DeviceError> {
        if self.poisoned {
            return Err(DeviceError::Backend);
        }
        if let Some(lease) = &self.active {
            if let Some(frame) = lease.frame {
                if address >= lease.base && address - lease.base < (1usize << frame.bits) {
                    return Ok((lease.base, frame));
                }
            }
        }
        self.evict()?;
        let mut region = *self
            .regions
            .iter()
            .find(|r| address >= r.address && address - r.address < (1usize << r.bits))
            .ok_or(DeviceError::Unsupported)?;
        let bits = if region.bits >= 21 { 21 } else { 12 };
        let page = address & !((1usize << bits) - 1);
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(TEMP_SLOTS)
            .map_err(|_| DeviceError::Exhausted)?;
        self.active = Some(Lease {
            source: region.cap,
            base: page,
            frame: None,
            slots,
        });
        let result = (|| {
            while region.bits > bits {
                let child_bits = region.bits - 1;
                let mut children = [sel4::cap::Untyped::from_bits(0); 2];
                for child in &mut children {
                    let slot = self.reserve()?;
                    region
                        .cap
                        .untyped_retype(
                            &sel4::ObjectBlueprint::Untyped {
                                size_bits: child_bits,
                            },
                            &self.root.absolute_cptr_for_self(),
                            slot,
                            1,
                        )
                        .map_err(|_| DeviceError::Backend)?;
                    *child = sel4::cap::Untyped::from_bits(slot as _);
                }
                let midpoint = region.address + (1usize << child_bits);
                region = if page < midpoint {
                    Region {
                        address: region.address,
                        bits: child_bits,
                        cap: children[0],
                    }
                } else {
                    Region {
                        address: midpoint,
                        bits: child_bits,
                        cap: children[1],
                    }
                };
            }
            assert_eq!(region.address, page);
            let slot = self.reserve()?;
            let blueprint = if bits == 21 {
                sel4::cap_type::LargePage::object_blueprint()
            } else {
                sel4::cap_type::Granule::object_blueprint()
            };
            region
                .cap
                .untyped_retype(&blueprint, &self.root.absolute_cptr_for_self(), slot, 1)
                .map_err(|_| DeviceError::Backend)?;
            Ok(Frame { bits, slot })
        })();
        match result {
            Ok(frame) => {
                self.active.as_mut().unwrap().frame = Some(frame);
                Ok((page, frame))
            }
            Err(error) => {
                self.evict()?;
                Err(error)
            }
        }
    }
    fn verify_returned_slots(&mut self) {
        assert!(self.active.is_none() && !self.poisoned);
        let mut slots = Vec::new();
        for _ in 0..TEMP_SLOTS {
            slots.push(self.slots.reserve_slot().expect("returned temporary slot"));
        }
        assert!(self.slots.reserve_slot().is_err());
        for slot in slots {
            self.slots
                .delete(CapRef::new(
                    self.root.bits() as _,
                    slot.path().bits() as _,
                    sel4::WORD_SIZE as _,
                    0,
                    CapMode::MOVE,
                    Rights::ALL,
                ))
                .unwrap();
        }
        sel4::debug_println!(
            "DEVMGR_PLATFORM_PASS bounded=true returned=true peak={} evictions={}",
            self.peak,
            self.evictions
        );
    }
}
impl DevicePlatform for Platform {
    fn rsdp(&mut self) -> Result<List<u8, 36>, DeviceError> {
        Ok(self.rsdp.clone())
    }
    fn read(&mut self, address: u64, length: u32) -> Result<List<u8, 256>, DeviceError> {
        if self.ready || length == 0 || length > 256 || address.checked_add(length as u64).is_none()
        {
            return Err(DeviceError::Invalid);
        }
        let address = usize::try_from(address).map_err(|_| DeviceError::Invalid)?;
        let mut output = List::default();
        let mut done = 0;
        while done < length as usize {
            let at = address + done;
            let (page, frame) = self.frame(at)?;
            let offset = at - page;
            let count = ((1usize << frame.bits) - offset).min(length as usize - done);
            let attrs = sel4::VmAttributes::CACHE_DISABLED | sel4::VmAttributes::EXECUTE_NEVER;
            let vspace = sel4::init_thread::slot::VSPACE.cap();
            let window = if frame.bits == 21 {
                LARGE_WINDOW
            } else {
                self.scratch
            };
            let mapped = if frame.bits == 21 {
                sel4::cap::LargePage::from_bits(frame.slot as _).frame_map(
                    vspace,
                    window,
                    sel4::CapRights::read_only(),
                    attrs,
                )
            } else {
                sel4::cap::Granule::from_bits(frame.slot as _).frame_map(
                    vspace,
                    window,
                    sel4::CapRights::read_only(),
                    attrs,
                )
            };
            if mapped.is_err() {
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
            let unmapped = if frame.bits == 21 {
                sel4::cap::LargePage::from_bits(frame.slot as _).frame_unmap()
            } else {
                sel4::cap::Granule::from_bits(frame.slot as _).frame_unmap()
            };
            if unmapped.is_err() {
                self.poisoned = true;
                return Err(DeviceError::Backend);
            }
            done += count;
        }
        output.len = length;
        Ok(output)
    }
    fn ready(&mut self) -> Result<(), DeviceError> {
        self.evict()?;
        self.verify_returned_slots();
        self.ready = true;
        Ok(())
    }
}
