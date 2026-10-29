//! Bounded one-vCPU execution domains for a hosted kernel.
//! No raw TCB/VSpace/frame/SC authority leaves root. PFNs and IDs select only
//! within the caller's domain; RAM and all kernel objects use its account.
use super::*;
use crate::protocol::*;
use crate::service::error;
use alloc::collections::{BTreeMap, BTreeSet};
use horn_ipc::{Cap, List, Notification};

const PAGE: usize = PAGE_SIZE;
const MAX_RAM: usize = 64 * 1024 * 1024;
const USER_END: usize = 0x1000_0000;
const VM_START: usize = 0x4000_0000;
const VM_END: usize = 0x4400_0000;
const MAX_SPACES: usize = 8;
const MAX_MAPS: usize = 16384;
const READ: u32 = 1;
const WRITE: u32 = 2;
const EXEC: u32 = 4;

struct Frame {
    cap: sel4::cap::Granule,
    exec: usize,
}
struct Mapping {
    cap: sel4::cap::Granule,
    pfn: usize,
    rights: u32,
}
struct Space {
    vspace: sel4::cap::VSpace,
    maps: BTreeMap<usize, Mapping>,
}
pub(crate) struct Execution {
    allocator: KernelAllocator,
    copied_slots: BTreeSet<usize>,
    kernel: sel4::cap::VSpace,
    base: usize,
    first: usize,
    ram_pages: usize,
    frames: Vec<Frame>,
    spaces: BTreeMap<u32, Space>,
    next_space: u32,
    kernel_maps: BTreeMap<usize, Mapping>,
    user: sel4::cap::Tcb,
    fp_frame: sel4::cap::Granule,
    fp_bytes: usize,
    fp_features: u64,
    fp_snapshot: bool,
    fp_cursor: usize,
    fp_active: bool,
    fp_broken: bool,
    native_sc: sel4::cap::SchedContext,
    scheduler: sel4::cap::SchedControl,
    budget_us: u64,
    period_us: u64,
    split: bool,
    cnode: sel4::cap::CNode,
    fault_cap: sel4::cap::Endpoint,
    fault_badge: sel4::Word,
    wake: sel4::cap::Notification,
    running: bool,
    exit: Option<ExecutionExit>,
}
fn rights(bits: u32) -> sel4::CapRights {
    sel4::CapRights::new(false, false, bits & READ != 0, bits & WRITE != 0)
}
fn attrs(bits: u32) -> sel4::VmAttributes {
    if bits & EXEC != 0 {
        sel4::VmAttributes::DEFAULT
    } else {
        sel4::VmAttributes::EXECUTE_NEVER
    }
}
fn encode_context(c: &sel4::UserContext) -> ExecutionRegisters {
    let r = c.inner();
    ExecutionRegisters {
        ip: r.rip,
        sp: r.rsp,
        flags: r.rflags,
        ax: r.rax,
        bx: r.rbx,
        cx: r.rcx,
        dx: r.rdx,
        si: r.rsi,
        di: r.rdi,
        bp: r.rbp,
        r8: r.r8,
        r9: r.r9,
        r10: r.r10,
        r11: r.r11,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        r15: r.r15,
        fs: r.fs_base,
        gs: r.gs_base,
    }
}
fn decode_context(r: &ExecutionRegisters) -> sel4::UserContext {
    let mut c = sel4::UserContext::default();
    let d = c.inner_mut();
    d.rip = r.ip;
    d.rsp = r.sp;
    d.rflags = r.flags;
    d.rax = r.ax;
    d.rbx = r.bx;
    d.rcx = r.cx;
    d.rdx = r.dx;
    d.rsi = r.si;
    d.rdi = r.di;
    d.rbp = r.bp;
    d.r8 = r.r8;
    d.r9 = r.r9;
    d.r10 = r.r10;
    d.r11 = r.r11;
    d.r12 = r.r12;
    d.r13 = r.r13;
    d.r14 = r.r14;
    d.r15 = r.r15;
    d.fs_base = r.fs;
    d.gs_base = r.gs;
    c
}
impl Execution {
    pub(crate) fn stop_user(&mut self) -> Result<(), Error> {
        self.user.tcb_suspend()?;
        self.running = false;
        Ok(())
    }
    pub(crate) fn cleanup(&mut self) -> Result<(), Error> {
        self.stop_user()?;
        if self.fp_active {
            self.fp_frame.frame_unmap()?;
            self.fp_active = false;
        }
        while let Some(slot) = self.copied_slots.first().copied() {
            self.allocator.release_slot(slot)?;
            self.copied_slots.remove(&slot);
        }
        Ok(())
    }
    fn fp_access<T>(
        &mut self,
        root: sel4::cap::VSpace,
        scratch: usize,
        f: impl FnOnce(&mut [u8]) -> T,
    ) -> Result<T, Error> {
        if self.fp_broken || self.fp_active {
            return Err(Error::Cleanup);
        }
        if let Err(err) = self.fp_frame.frame_map(
            root,
            scratch,
            sel4::CapRights::read_write(),
            sel4::VmAttributes::EXECUTE_NEVER,
        ) {
            self.fp_broken = true;
            return Err(err.into());
        }
        self.fp_active = true;
        // Manager's exclusive scratch page, serialized with loader/other callers.
        let result = f(unsafe { core::slice::from_raw_parts_mut(scratch as *mut u8, PAGE) });
        if let Err(err) = self.fp_frame.frame_unmap() {
            self.fp_broken = true;
            return Err(err.into());
        }
        self.fp_active = false;
        Ok(result)
    }
    fn frame(&self, pfn: usize) -> Result<&Frame, Error> {
        self.frames
            .get(pfn.checked_sub(self.first).ok_or(Error::Denied)?)
            .ok_or(Error::Denied)
    }
    fn kernel_protection(&mut self, pfn: usize, writable: bool) -> Result<(), Error> {
        let cap = self.frame(pfn)?.cap;
        cap.frame_unmap()?;
        cap.frame_map(
            self.kernel,
            self.base + pfn * PAGE,
            if writable {
                sel4::CapRights::read_write()
            } else {
                sel4::CapRights::read_only()
            },
            sel4::VmAttributes::EXECUTE_NEVER,
        )?;
        Ok(())
    }
    fn remove(&mut self, space: u32, address: usize) -> Result<(), Error> {
        let map = if space == 0 {
            self.kernel_maps.remove(&address)
        } else {
            self.spaces
                .get_mut(&space)
                .ok_or(Error::Stale)?
                .maps
                .remove(&address)
        };
        let Some(map) = map else { return Ok(()) };
        // Only publish the removal after confirmed unmap/release; on any error
        // the process must be stopped rather than reusing this domain.
        map.cap.frame_unmap()?;
        self.allocator.release_slot(map.cap.bits() as usize)?;
        self.copied_slots.remove(&(map.cap.bits() as usize));
        if map.rights & EXEC != 0 {
            self.frames[map.pfn - self.first].exec -= 1;
            if self.frame(map.pfn)?.exec == 0 {
                self.kernel_protection(map.pfn, true)?;
            }
        }
        Ok(())
    }
    fn map(&mut self, space: u32, address: usize, pfn: usize, bits: u32) -> Result<(), Error> {
        if self.running
            || address % PAGE != 0
            || bits & !7 != 0
            || bits & READ == 0
            || bits & (WRITE | EXEC) == WRITE | EXEC
        {
            return Err(Error::Denied);
        }
        let target = if space == 0 {
            if address < VM_START || address >= VM_END || bits & EXEC != 0 {
                return Err(Error::Denied);
            }
            self.kernel
        } else {
            if address < PAGE || address >= USER_END {
                return Err(Error::Denied);
            }
            self.spaces.get(&space).ok_or(Error::Stale)?.vspace
        };
        self.frame(pfn)?;
        self.remove(space, address)?;
        if self.spaces.values().map(|s| s.maps.len()).sum::<usize>() + self.kernel_maps.len()
            >= MAX_MAPS
        {
            return Err(Error::Denied);
        }
        let conflicting = self
            .spaces
            .values()
            .flat_map(|s| s.maps.values())
            .chain(self.kernel_maps.values())
            .any(|m| {
                m.pfn == pfn
                    && (bits & EXEC != 0 && m.rights & WRITE != 0
                        || bits & WRITE != 0 && m.rights & EXEC != 0)
            });
        if conflicting || bits & WRITE != 0 && self.frame(pfn)?.exec != 0 {
            return Err(Error::Denied);
        }
        let slot = self.allocator.reserve_slot()?;
        self.copied_slots.insert(slot);
        let cap = sel4::cap::Granule::from_bits(slot as _);
        let root = self.allocator.root();
        if let Err(e) = root.absolute_cptr(cap.cptr()).copy(
            &root.absolute_cptr(self.frame(pfn)?.cap.cptr()),
            rights(bits),
        ) {
            self.allocator.release_slot(cap.bits() as usize)?;
            self.copied_slots.remove(&(cap.bits() as usize));
            return Err(e.into());
        }
        if bits & EXEC != 0 && self.frame(pfn)?.exec == 0 {
            self.kernel_protection(pfn, false)?;
        }
        cap.frame_map(target, address, rights(bits), attrs(bits))?;
        if bits & EXEC != 0 {
            self.frames[pfn - self.first].exec += 1;
        }
        let mapping = Mapping {
            cap,
            pfn,
            rights: bits,
        };
        if space == 0 {
            self.kernel_maps.insert(address, mapping);
        } else {
            self.spaces
                .get_mut(&space)
                .unwrap()
                .maps
                .insert(address, mapping);
        }
        Ok(())
    }
}
impl Manager {
    /// Raw fault routing, before the IDL decoder consumes the IPC buffer.
    /// Cancelling this foreign fault is safe: no Linux syscall has executed,
    /// and foreign user TCBs have no native seL4 authority.
    pub fn execution_fault(
        &mut self,
        info: &sel4::MessageInfo,
        badge: sel4::Word,
    ) -> Result<bool, Error> {
        let Some(exec) = self
            .processes
            .iter_mut()
            .filter_map(|p| p.execution.as_mut())
            .find(|e| e.fault_badge == badge)
        else {
            return Ok(false);
        };
        let fault = sel4::with_ipc_buffer(|ipc| sel4::Fault::new(ipc, info));
        let (reason, address, code) = match fault {
            sel4::Fault::UnknownSyscall(f) => {
                (ExecutionReason::Syscall, 0, f.inner().get_Syscall())
            }
            sel4::Fault::VmFault(f) => (
                ExecutionReason::PageFault,
                f.inner().get_Addr(),
                f.inner().get_FSR(),
            ),
            sel4::Fault::Timeout(_) => (ExecutionReason::Budget, 0, 0),
            sel4::Fault::UserException(f) => (
                ExecutionReason::Exception,
                f.inner().get_Number(),
                f.inner().get_Code(),
            ),
            _ => return Err(Error::Denied),
        };
        exec.stop_user()?;
        exec.exit = Some(ExecutionExit {
            reason,
            address,
            code,
            registers: encode_context(&exec.user.tcb_read_all_registers(false)?),
        });
        exec.wake.signal();
        Ok(true)
    }
}
impl ProcessExecution for Caller<'_> {
    fn execution_configure(
        &mut self,
        base: u64,
        bytes: u64,
        wake: Cap<Notification>,
    ) -> Result<(), ProcessError> {
        let id = self.id()?;
        let p = self.manager.process(id).map_err(error)?;
        if p.execution.is_some()
            || p.execution_poisoned
            || base % PAGE as u64 != 0
            || bytes < 16 * 1024 * 1024
            || bytes > MAX_RAM as u64
            || bytes % PAGE as u64 != 0
            || base
                .checked_add(bytes)
                .is_none_or(|end| end > VM_START as u64)
            || base > p.entry as u64
            || p.resident_end as u64 >= base + bytes
            || wake.as_ref().unwrapped_badge().is_some()
            || p.budget_us < 2
        {
            return Err(ProcessError::Invalid);
        }
        let cpu = p.cpu;
        let scheduler = self.manager.schedulers[cpu];
        let p = self.manager.process_mut(id).map_err(error)?;
        // Failed attach is fail-closed and cannot be retried. Partial objects
        // remain charged to this process until ordinary process teardown.
        p.execution_poisoned = true;
        let mut copied_slots = BTreeSet::new();
        let result = (|| -> Result<Execution, Error> {
            let mut a = p.allocator.clone();
            let base = base as usize;
            let first = (p.resident_end - base) / PAGE;
            let wake_slot = a.reserve_slot()?;
            copied_slots.insert(wake_slot);
            a.root()
                .absolute_cptr(sel4::CPtr::from_bits(wake_slot as _))
                .copy(
                    &a.root()
                        .absolute_cptr(sel4::CPtr::from_bits(wake.as_ref().slot as _)),
                    sel4::CapRights::write_only(),
                )?;
            let badge = NEXT_SELF_BADGE
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    v.checked_add(1).filter(|n| *n < (1 << 48))
                })
                .map_err(|_| Error::Denied)?;
            let fault = a.reserve_slot()?;
            copied_slots.insert(fault);
            a.root()
                .absolute_cptr(sel4::CPtr::from_bits(fault as _))
                .mint(
                    &a.root()
                        .absolute_cptr(p.lifecycle_source.ok_or(Error::Denied)?.cptr()),
                    sel4::CapRights::all(),
                    badge,
                )?;
            let user = a.allocate_fixed_sized::<sel4::cap_type::Tcb>()?;
            sel4::sel4_cfg_if! {
                if #[sel4_cfg(DEBUG_BUILD)] { user.debug_name(alloc::format!("{}:user",p.name).as_bytes()); }
            }
            let cnode = a.allocate_variable_sized::<sel4::cap_type::CNode>(4)?;
            let sc = a.allocate_variable_sized::<sel4::cap_type::SchedContext>(
                sel4::sys::seL4_MinSchedContextBits as usize,
            )?;
            scheduler.sched_control_configure_flags(sc, p.budget_us / 2, p.period_us, 0, 0, 1)?;
            let fault_cap = sel4::cap::Endpoint::from_bits(fault as _);
            user.tcb_configure(
                cnode,
                sel4::CNodeCapData::new(0, sel4::WORD_SIZE - 4),
                p.loaded.vspace,
                0,
                sel4::cap::Granule::from_bits(0),
            )?;
            user.tcb_set_sched_params(sel4::init_thread::slot::TCB.cap(), 0, 0, sc, fault_cap)?;
            user.tcb_set_timeout_endpoint(fault_cap)?;
            // bindgen intentionally omits this kernel enum; verify support, never
            // allow an unmarked application TCB to enter native dispatch.
            if user.tcb_set_flags(0, 2)? & 2 == 0 {
                return Err(Error::Denied);
            }
            crate::loader::prepare_window(
                &mut a,
                p.loaded.vspace,
                base + first * PAGE..base + bytes as usize,
            )?;
            let fp_frame = a.allocate_fixed_sized::<sel4::cap_type::Granule>()?;
            let fp_bytes = unsafe { core::arch::x86_64::__cpuid_count(13, 0) }.ebx as usize;
            let (lo, hi): (u32, u32);
            unsafe {
                core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi,
                options(nomem, nostack));
            }
            let fp_features = ((hi as u64) << 32) | lo as u64;
            if fp_bytes < 576
                || fp_bytes > PAGE
                || fp_bytes > sel4::sel4_cfg_usize!(XSAVE_SIZE)
                || fp_features & !0xe7 != 0
                || fp_features & 7 != 7
            {
                return Err(Error::Denied);
            }
            let frames = Vec::new();
            Ok(Execution {
                allocator: a,
                copied_slots: core::mem::take(&mut copied_slots),
                kernel: p.loaded.vspace,
                base,
                first,
                ram_pages: bytes as usize / PAGE,
                frames,
                spaces: BTreeMap::new(),
                next_space: 1,
                kernel_maps: BTreeMap::new(),
                user,
                fp_frame,
                fp_bytes,
                fp_features,
                fp_snapshot: false,
                fp_cursor: 0,
                fp_active: false,
                fp_broken: false,
                native_sc: p.sc,
                scheduler,
                budget_us: p.budget_us,
                period_us: p.period_us,
                split: false,
                cnode,
                fault_cap,
                fault_badge: badge,
                wake: sel4::cap::Notification::from_bits(wake_slot as _),
                running: false,
                exit: None,
            })
        })();
        match result {
            Ok(exec) => {
                p.execution = Some(exec);
                p.execution_poisoned = false;
                Ok(())
            }
            Err(e) => {
                for slot in copied_slots {
                    p.allocator
                        .release_slot(slot)
                        .map_err(|_| ProcessError::Quarantined)?;
                }
                Err(error(e))
            }
        }
    }
    fn execution_admit(&mut self) -> Result<u32, ProcessError> {
        let id = self.id()?;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        if e.running {
            return Err(ProcessError::State);
        }
        let start = e.first + e.frames.len();
        let end = (start + 64).min(e.ram_pages);
        for pfn in start..end {
            let cap = e
                .allocator
                .allocate_fixed_sized::<sel4::cap_type::Granule>()
                .map_err(|x| {
                    sel4::debug_println!("ADMIT_ALLOC_FAIL pfn={} error={:?}", pfn, x);
                    error(x.into())
                })?;
            cap.frame_map(
                e.kernel,
                e.base + pfn * PAGE,
                sel4::CapRights::read_write(),
                sel4::VmAttributes::EXECUTE_NEVER,
            )
            .map_err(|x| {
                sel4::debug_println!("ADMIT_MAP_FAIL pfn={} error={:?}", pfn, x);
                error(x.into())
            })?;
            e.frames.push(Frame { cap, exec: 0 });
        }
        Ok(end as u32)
    }
    fn execution_space(&mut self) -> Result<u32, ProcessError> {
        let id = self.id()?;
        let asid = self.manager.asid_pool;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        if e.running || e.spaces.len() >= MAX_SPACES || e.next_space == u32::MAX {
            return Err(ProcessError::Exhausted);
        }
        let vspace = e
            .allocator
            .allocate_fixed_sized::<sel4::cap_type::VSpace>()
            .map_err(|x| error(x.into()))?;
        asid.asid_pool_assign(vspace).map_err(|x| error(x.into()))?;
        crate::loader::prepare_window(&mut e.allocator, vspace, PAGE..USER_END).map_err(error)?;
        let n = e.next_space;
        e.next_space += 1;
        e.spaces.insert(
            n,
            Space {
                vspace,
                maps: BTreeMap::new(),
            },
        );
        Ok(n)
    }
    fn execution_destroy(&mut self, space: u32) -> Result<(), ProcessError> {
        let id = self.id()?;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        if e.running {
            return Err(ProcessError::State);
        }
        let keys: Vec<_> = e
            .spaces
            .get(&space)
            .ok_or(ProcessError::Stale)?
            .maps
            .keys()
            .copied()
            .collect();
        for key in keys {
            e.remove(space, key).map_err(error)?;
        }
        // Translation backing stays charged until the process is torn down.
        // Do not falsely report it reclaimed or recycle its ASID/ID.
        let s = e.spaces.remove(&space).unwrap();
        e.allocator
            .release_slot(s.vspace.bits() as usize)
            .map_err(|x| error(x.into()))?;
        Ok(())
    }
    fn execution_map(
        &mut self,
        space: u32,
        address: u64,
        pfn: u64,
        rights: u32,
    ) -> Result<(), ProcessError> {
        let id = self.id()?;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        e.map(space, address as usize, pfn as usize, rights)
            .map_err(|err| {
                sel4::debug_println!(
                    "EXECUTION_MAP_FAIL space={} va={:#x} pfn={} rights={} error={:?}",
                    space,
                    address,
                    pfn,
                    rights,
                    err
                );
                error(err)
            })
    }
    fn execution_unmap(
        &mut self,
        space: u32,
        address: u64,
        bytes: u64,
    ) -> Result<(), ProcessError> {
        let id = self.id()?;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        if e.running
            || address % PAGE as u64 != 0
            || bytes % PAGE as u64 != 0
            || address.checked_add(bytes).is_none()
        {
            return Err(ProcessError::Invalid);
        }
        let maps = if space == 0 {
            &e.kernel_maps
        } else {
            &e.spaces.get(&space).ok_or(ProcessError::Stale)?.maps
        };
        let keys: Vec<_> = maps
            .range(address as usize..(address + bytes) as usize)
            .map(|(&k, _)| k)
            .collect();
        for key in keys {
            e.remove(space, key).map_err(error)?;
        }
        Ok(())
    }
    fn execution_fp_info(&mut self) -> Result<ExecutionFpInfo, ProcessError> {
        let id = self.id()?;
        let e = self
            .manager
            .process(id)
            .map_err(error)?
            .execution
            .as_ref()
            .ok_or(ProcessError::State)?;
        Ok(ExecutionFpInfo {
            bytes: e.fp_bytes as u32,
            features: e.fp_features,
        })
    }
    fn execution_fp_read(
        &mut self,
        offset: u32,
        length: u32,
    ) -> Result<List<u8, 256>, ProcessError> {
        if self.manager.scratch_poisoned {
            return Err(ProcessError::Quarantined);
        }
        let id = self.id()?;
        let (root, scratch) = (self.manager.caller_vspace, self.manager.scratch);
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        let (offset, length) = (offset as usize, length as usize);
        if e.running || e.fp_cursor != 0 {
            return Err(ProcessError::State);
        }
        if length == 0 || length > 256 || offset > e.fp_bytes || length > e.fp_bytes - offset {
            return Err(ProcessError::Invalid);
        }
        if !e.fp_snapshot {
            e.user
                .tcb_read_fpu(e.fp_frame, 0)
                .map_err(|err| error(err.into()))?;
            e.fp_snapshot = true;
        }
        let result = e
            .fp_access(root, scratch, |bytes| {
                let mut output = List::default();
                output.len = length as u32;
                output.data[..length].copy_from_slice(&bytes[offset..offset + length]);
                output
            })
            .map_err(error);
        if e.fp_broken {
            self.manager.scratch_poisoned = true;
        }
        result
    }
    fn execution_fp_write(
        &mut self,
        offset: u32,
        data: List<u8, 256>,
        commit: bool,
    ) -> Result<(), ProcessError> {
        if self.manager.scratch_poisoned {
            return Err(ProcessError::Quarantined);
        }
        let id = self.id()?;
        let (root, scratch) = (self.manager.caller_vspace, self.manager.scratch);
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        let offset = offset as usize;
        let length = data.len as usize;
        if e.running {
            return Err(ProcessError::State);
        }
        if length == 0
            || length > 256
            || (offset != 0 && offset != e.fp_cursor)
            || offset > e.fp_bytes
            || length > e.fp_bytes - offset
            || (commit && offset + length != e.fp_bytes)
        {
            return Err(ProcessError::Invalid);
        }
        let result = e
            .fp_access(root, scratch, |bytes| {
                if offset == 0 {
                    bytes.fill(0);
                }
                bytes[offset..offset + length].copy_from_slice(&data.data[..length]);
            })
            .map_err(error);
        if e.fp_broken {
            self.manager.scratch_poisoned = true;
            return result;
        }
        result?;
        e.fp_snapshot = false;
        e.fp_cursor = offset + length;
        if commit {
            e.fp_cursor = 0;
            e.user
                .tcb_write_fpu(e.fp_frame, 0)
                .map_err(|err| error(err.into()))?;
        }
        Ok(())
    }
    fn execution_start(
        &mut self,
        space: u32,
        registers: ExecutionRegisters,
    ) -> Result<(), ProcessError> {
        if self.manager.scratch_poisoned {
            return Err(ProcessError::Quarantined);
        }
        let id = self.id()?;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        if e.running
            || e.fp_cursor != 0
            || e.fp_broken
            || e.exit.is_some()
            || registers.ip < PAGE as u64
            || registers.ip >= USER_END as u64
            || registers.sp >= USER_END as u64
            || registers.fs >= USER_END as u64
            || registers.gs >= USER_END as u64
        {
            return Err(ProcessError::State);
        }
        let vspace = e.spaces.get(&space).ok_or(ProcessError::Stale)?.vspace;
        e.user
            .tcb_set_space(
                e.fault_cap.cptr(),
                e.cnode,
                sel4::CNodeCapData::new(0, sel4::WORD_SIZE - 4),
                vspace,
            )
            .map_err(|x| error(x.into()))?;
        if !e.split {
            e.scheduler
                .sched_control_configure_flags(
                    e.native_sc,
                    e.budget_us - e.budget_us / 2,
                    e.period_us,
                    0,
                    0,
                    1,
                )
                .map_err(|x| error(x.into()))?;
            e.split = true;
        }
        let mut c = decode_context(&registers);
        e.user
            .tcb_write_all_registers(true, &mut c)
            .map_err(|x| error(x.into()))?;
        e.fp_snapshot = false;
        e.running = true;
        Ok(())
    }
    fn execution_stop(&mut self) -> Result<ExecutionExit, ProcessError> {
        let id = self.id()?;
        let e = self
            .manager
            .process_mut(id)
            .map_err(error)?
            .execution
            .as_mut()
            .ok_or(ProcessError::State)?;
        e.stop_user().map_err(error)?;
        if let Some(exit) = e.exit.take() {
            return Ok(exit);
        }
        Ok(ExecutionExit {
            reason: ExecutionReason::Interrupted,
            address: 0,
            code: 0,
            registers: encode_context(
                &e.user
                    .tcb_read_all_registers(false)
                    .map_err(|x| error(x.into()))?,
            ),
        })
    }
}
