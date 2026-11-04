//! Generic process/thread objects. Control is full target authority; views are not.
//! A scheduling-context capability spends a fixed budget, never one budget per TCB.
use super::*;
use crate::protocol::*;
use crate::service::error;
use horn_ipc::*;

pub(crate) const IPC_BASE: usize = 0x7fff_ff00_0000;
const MAX_THREADS: usize = 64;
const USER_END: u64 = 1 << 47;

pub(crate) struct Book {
    next: u64,
    entries: Vec<Thread>,
    observer: Option<(usize, sel4::Word)>,
    context: Option<(usize, sel4::Word)>,
    active: Option<(ProcessId, u64)>,
}
struct Thread {
    id: u64,
    tcb: sel4::cap::Tcb,
    allocator: Option<KernelAllocator>,
    control: Option<(usize, sel4::Word)>,
    observer: Option<(usize, sel4::Word)>,
    fault: Option<(usize, sel4::Word)>,
    wake: Option<usize>,
    state: ThreadState,
    reason: Option<(ExecutionReason, u64, u64)>,
    context: Option<ProcessId>,
    ipc: Option<(usize, Option<sel4::cap::Granule>)>,
    ipc_slot: Option<usize>,
    fp_frame: Option<sel4::cap::Granule>,
    fp_cursor: usize,
    fp_snapshot: bool,
    fp_broken: bool,
}
impl Thread {
    fn new(id: u64, tcb: sel4::cap::Tcb) -> Self {
        Self {
            id,
            tcb,
            allocator: None,
            control: None,
            observer: None,
            fault: None,
            wake: None,
            state: ThreadState::Stopped,
            reason: None,
            context: None,
            ipc: None,
            ipc_slot: None,
            fp_frame: None,
            fp_cursor: 0,
            fp_snapshot: false,
            fp_broken: false,
        }
    }
    fn info(&self) -> ThreadInfo {
        ThreadInfo {
            id: self.id,
            state: self.state,
            reason: self.reason.map(|r| r.0),
        }
    }
}
impl Book {
    pub(crate) fn new(owner: ProcessId, tcb: Option<sel4::cap::Tcb>, ipc: Option<usize>) -> Self {
        let mut entries = Vec::new();
        if let Some(tcb) = tcb {
            let mut thread = Thread::new(1, tcb);
            thread.ipc = ipc.map(|address| (address, None));
            thread.context = Some(owner);
            entries.push(thread);
        }
        Self {
            next: 2,
            entries,
            observer: None,
            context: None,
            active: tcb.map(|_| (owner, 1)),
        }
    }
    pub(crate) fn can_start(&self, owner: ProcessId) -> bool {
        self.active == Some((owner, 1))
    }
    pub(crate) fn started(&mut self, owner: ProcessId) {
        if let Some(thread) = self.entries.iter_mut().find(|t| t.id == 1) {
            thread.state = ThreadState::Running;
            thread.context = Some(owner);
        }
        self.active = Some((owner, 1));
    }
}
fn valid_registers(r: &ExecutionRegisters) -> bool {
    r.ip >= PAGE_SIZE as u64
        && r.ip < USER_END
        && r.sp < USER_END
        && r.fs < USER_END
        && r.gs < USER_END
}
fn fp_info() -> ExecutionFpInfo {
    let (lo, hi): (u32, u32);
    unsafe {
        core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi,
        options(nomem, nostack));
    }
    ExecutionFpInfo {
        bytes: unsafe { core::arch::x86_64::__cpuid_count(13, 0) }.ebx,
        features: ((hi as u64) << 32) | lo as u64,
    }
}
impl Manager {
    fn thread(&self, owner: ProcessId, id: u64) -> Result<&Thread, Error> {
        self.process(owner)?
            .threads
            .entries
            .iter()
            .find(|t| t.id == id)
            .ok_or(Error::Stale)
    }
    fn thread_mut(&mut self, owner: ProcessId, id: u64) -> Result<&mut Thread, Error> {
        self.process_mut(owner)?
            .threads
            .entries
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(Error::Stale)
    }
    pub(crate) fn export_object<T: CapType>(&mut self, authority: usize) -> Result<Cap<T>, Error> {
        if self.outgoing.is_some() {
            return Err(Error::InvalidState);
        }
        let slot = self.allocator.reserve_slot()?;
        let root = self.allocator.root();
        if let Err(err) = root.absolute_cptr(sel4::CPtr::from_bits(slot as _)).copy(
            &root.absolute_cptr(sel4::CPtr::from_bits(authority as _)),
            sel4::CapRights::all(),
        ) {
            self.allocator.release_slot(slot)?;
            return Err(err.into());
        }
        self.outgoing = Some(slot);
        Ok(Cap::new(
            root.bits() as _,
            slot as _,
            sel4::WORD_SIZE as _,
            0,
            CapMode::MOVE,
            Rights::ALL,
        ))
    }
    fn thread_authority(
        &mut self,
        owner: ProcessId,
        id: u64,
        observe: bool,
    ) -> Result<usize, Error> {
        let thread = self.thread(owner, id)?;
        if let Some((slot, _)) = if observe {
            thread.observer
        } else {
            thread.control
        } {
            return Ok(slot);
        }
        let endpoint = self.live(owner)?.lifecycle_source.ok_or(Error::Denied)?;
        let authority = self.mint(endpoint)?;
        let thread = self.thread_mut(owner, id)?;
        if observe {
            thread.observer = Some(authority);
        } else {
            thread.control = Some(authority);
        }
        Ok(authority.0)
    }
    fn stop_thread(&mut self, owner: ProcessId, id: u64) -> Result<(), Error> {
        self.thread(owner, id)?.tcb.tcb_suspend()?;
        if let Some(context) = self.thread(owner, id)?.context {
            self.process(context)?.sc.unbind()?;
            self.process_mut(context)?.threads.active = None;
            self.thread_mut(owner, id)?.context = None;
        }
        self.thread_mut(owner, id)?.state = ThreadState::Stopped;
        Ok(())
    }
    fn stop_context(&mut self, owner: ProcessId) -> Result<(), Error> {
        if let Some((process, thread)) = self.process(owner)?.threads.active {
            self.stop_thread(process, thread)?;
        }
        Ok(())
    }
    pub(crate) fn stop_threads(&mut self, owner: ProcessId) -> Result<(), Error> {
        self.stop_context(owner)?;
        let ids: Vec<_> = self
            .process(owner)?
            .threads
            .entries
            .iter()
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.stop_thread(owner, id)?;
        }
        Ok(())
    }
    fn destroy_thread(&mut self, owner: ProcessId, id: u64) -> Result<(), Error> {
        let result = self.destroy_thread_inner(owner, id);
        if result.is_err() {
            let _ = self.stop_threads(owner);
            self.process_mut(owner)?.state = State::Quarantined;
            if let Ok(thread) = self.thread_mut(owner, id) {
                thread.state = ThreadState::Quarantined;
            }
            return Err(Error::Cleanup);
        }
        result
    }
    fn destroy_thread_inner(&mut self, owner: ProcessId, id: u64) -> Result<(), Error> {
        self.stop_thread(owner, id)?;
        for which in 0..3 {
            let t = self.thread(owner, id)?;
            let authority = match which {
                0 => t.control,
                1 => t.observer,
                _ => t.fault,
            };
            if let Some((slot, _)) = authority {
                self.release_authority(slot)?;
                let t = self.thread_mut(owner, id)?;
                match which {
                    0 => t.control = None,
                    1 => t.observer = None,
                    _ => t.fault = None,
                }
            }
        }
        let mut allocator = self
            .thread(owner, id)?
            .allocator
            .clone()
            .unwrap_or(self.process(owner)?.allocator.clone());
        if let Some(slot) = self.thread(owner, id)?.wake {
            allocator.release_slot(slot)?;
            self.thread_mut(owner, id)?.wake = None;
        }
        if let Some((address, Some(frame))) = self.thread(owner, id)?.ipc {
            frame.frame_unmap()?;
            // Keep the frame in the scoped account until TCB teardown completes.
            self.thread_mut(owner, id)?.ipc = Some((address, None));
        }
        if self.thread(owner, id)?.allocator.is_some() {
            allocator.destroy_account()?;
        } else {
            let tcb = self.thread(owner, id)?.tcb;
            allocator.release_slot(tcb.bits() as usize)?;
            self.process_mut(owner)?.tcb = None;
            if let Some(frame) = self.thread(owner, id)?.fp_frame {
                allocator.release_slot(frame.bits() as usize)?;
            }
        }
        self.process_mut(owner)?
            .threads
            .entries
            .retain(|t| t.id != id);
        Ok(())
    }
    pub(crate) fn cleanup_threads(&mut self, owner: ProcessId) -> Result<(), Error> {
        while let Some(id) = self.process(owner)?.threads.entries.first().map(|t| t.id) {
            self.destroy_thread(owner, id)?;
        }
        for context in [false, true] {
            let book = &self.process(owner)?.threads;
            if let Some((slot, _)) = if context { book.context } else { book.observer } {
                self.release_authority(slot)?;
                let book = &mut self.process_mut(owner)?.threads;
                if context {
                    book.context = None;
                } else {
                    book.observer = None;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn thread_fault(
        &mut self,
        info: &sel4::MessageInfo,
        badge: sel4::Word,
    ) -> Result<bool, Error> {
        let target = self.processes.iter().find_map(|p| {
            p.threads
                .entries
                .iter()
                .find(|t| t.fault.is_some_and(|(_, b)| b == badge))
                .map(|t| (p.id, t.id))
        });
        let Some((owner, id)) = target else {
            return Ok(false);
        };
        let fault = sel4::with_ipc_buffer(|ipc| sel4::Fault::new(ipc, info));
        let reason = match fault {
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
        self.stop_thread(owner, id)?;
        let t = self.thread_mut(owner, id)?;
        t.reason = Some(reason);
        t.fp_snapshot = false;
        if let Some(wake) = t.wake {
            sel4::cap::Notification::from_bits(wake as _).signal();
        }
        Ok(true)
    }
    fn create_thread(
        &mut self,
        owner: ProcessId,
        request: ThreadCreate,
    ) -> Result<Cap<ThreadObjectTag>, Error> {
        if !valid_registers(&request.registers) {
            return Err(Error::Denied);
        }
        let p = self.live(owner)?;
        if !matches!(p.state, State::Created | State::Active)
            || p.threads.entries.len() >= MAX_THREADS
        {
            return Err(Error::InvalidState);
        }
        let endpoint = p.lifecycle_source.ok_or(Error::Denied)?;
        let id = p.threads.next;
        let next = id.checked_add(1).ok_or(Error::Denied)?;
        let mut allocator = p
            .allocator
            .for_account(p.allocator.account().child(2 * 1024 * 1024)?);
        let vspace = p.loaded.vspace;
        let cnode = p.cnode;
        let bits = p.cnode_bits;
        self.process_mut(owner)?
            .threads
            .entries
            .try_reserve(1)
            .map_err(|_| Error::Resource(horn_vm::Error::MetadataExhausted))?;
        let fault = self.mint(endpoint)?;
        let result = (|| -> Result<Thread, Error> {
            let tcb = allocator.allocate_fixed_sized::<sel4::cap_type::Tcb>()?;
            let mut thread = Thread::new(id, tcb);
            tcb.tcb_configure(
                cnode,
                sel4::CNodeCapData::new(0, sel4::WORD_SIZE - bits),
                vspace,
                0,
                sel4::cap::Granule::from_bits(0),
            )?;
            let fault_cap = sel4::cap::Endpoint::from_bits(fault.0 as _);
            tcb.tcb_set_sched_params(
                sel4::init_thread::slot::TCB.cap(),
                0,
                0,
                sel4::cap::SchedContext::from_bits(0),
                fault_cap,
            )?;
            tcb.tcb_set_timeout_endpoint(fault_cap)?;
            if request.abi == ThreadAbi::TrappedSyscalls && tcb.tcb_set_flags(0, 2)? & 2 == 0 {
                return Err(Error::Denied);
            }
            let mut registers = crate::execution::decode_context(&request.registers);
            tcb.tcb_write_all_registers(false, &mut registers)?;
            thread.fault = Some(fault);
            thread.allocator = Some(allocator.clone());
            Ok(thread)
        })();
        let thread = match result {
            Ok(t) => t,
            Err(err) => {
                allocator.destroy_account().map_err(|_| Error::Cleanup)?;
                self.release_authority(fault.0)?;
                return Err(err);
            }
        };
        self.process_mut(owner)?.threads.next = next;
        self.process_mut(owner)?.threads.entries.push(thread);
        if request.allocate_ipc_buffer {
            if let Err(err) = self.allocate_thread_ipc(owner, id) {
                if self.process(owner)?.state != State::Quarantined {
                    self.destroy_thread(owner, id)?;
                }
                return Err(err);
            }
        }
        let slot = self.thread_authority(owner, id, false)?;
        self.export_object(slot)
    }
    fn allocate_thread_ipc(&mut self, owner: ProcessId, id: u64) -> Result<u64, Error> {
        let p = self.live(owner)?;
        if !matches!(p.state, State::Created | State::Active) {
            return Err(Error::InvalidState);
        }
        let thread = self.thread(owner, id)?;
        if thread.state != ThreadState::Stopped {
            return Err(Error::InvalidState);
        }
        if let Some((address, _)) = thread.ipc {
            return Ok(address as u64);
        }
        let tcb = thread.tcb;
        let mut allocator = thread
            .allocator
            .clone()
            .unwrap_or_else(|| p.allocator.clone());
        let (vspace, cnode, bits) = (p.loaded.vspace, p.cnode, p.cnode_bits);
        let slot = (0..MAX_THREADS)
            .find(|slot| !p.threads.entries.iter().any(|t| t.ipc_slot == Some(*slot)))
            .ok_or(Error::Denied)?;
        let address = IPC_BASE + slot * PAGE_SIZE;
        crate::loader::prepare_window(
            &mut self.process_mut(owner)?.allocator,
            vspace,
            address..address + PAGE_SIZE,
        )?;
        let frame = allocator.allocate_fixed_sized::<sel4::cap_type::Granule>()?;
        let thread = self.thread_mut(owner, id)?;
        thread.ipc = Some((address, Some(frame)));
        thread.ipc_slot = Some(slot);
        let result = (|| -> Result<(), Error> {
            frame.frame_map(
                vspace,
                address,
                sel4::CapRights::read_write(),
                sel4::VmAttributes::EXECUTE_NEVER,
            )?;
            tcb.tcb_configure(
                cnode,
                sel4::CNodeCapData::new(0, sel4::WORD_SIZE - bits),
                vspace,
                address as _,
                frame,
            )?;
            Ok(())
        })();
        if let Err(err) = result {
            if frame.frame_unmap().is_err()
                || allocator.release_slot(frame.bits() as usize).is_err()
            {
                let _ = self.stop_threads(owner);
                self.process_mut(owner)?.state = State::Quarantined;
                return Err(Error::Cleanup);
            }
            let thread = self.thread_mut(owner, id)?;
            thread.ipc = None;
            thread.ipc_slot = None;
            return Err(err);
        }
        Ok(address as u64)
    }
    pub(crate) fn process_observer(
        &mut self,
        owner: ProcessId,
    ) -> Result<Cap<ProcessObserverObjectTag>, Error> {
        let p = self.live(owner)?;
        let authority = if let Some(a) = p.threads.observer {
            a
        } else {
            let endpoint = p.lifecycle_source.ok_or(Error::Denied)?;
            let a = self.mint(endpoint)?;
            self.process_mut(owner)?.threads.observer = Some(a);
            a
        };
        self.export_object(authority.0)
    }
    fn thread_page(&self, owner: ProcessId, after: Option<u64>) -> Result<ThreadPage, Error> {
        let mut entries = List::default();
        let mut threads = self
            .process(owner)?
            .threads
            .entries
            .iter()
            .filter(|t| after.is_none_or(|id| t.id > id));
        for thread in threads.by_ref().take(8) {
            entries.data[entries.len as usize] = thread.info();
            entries.len += 1;
        }
        let next = threads
            .next()
            .map(|_| entries.data[entries.len as usize - 1].id);
        Ok(ThreadPage { entries, next })
    }
    fn thread_fp_access<R>(
        &mut self,
        owner: ProcessId,
        id: u64,
        f: impl FnOnce(&mut [u8], sel4::cap::Granule) -> Result<R, Error>,
    ) -> Result<R, Error> {
        if self.scratch_poisoned || self.thread(owner, id)?.fp_broken {
            return Err(Error::Cleanup);
        }
        if self.thread(owner, id)?.state != ThreadState::Stopped {
            return Err(Error::InvalidState);
        }
        let frame = if let Some(frame) = self.thread(owner, id)?.fp_frame {
            frame
        } else {
            let mut allocator = self
                .thread(owner, id)?
                .allocator
                .clone()
                .unwrap_or(self.process(owner)?.allocator.clone());
            let frame = allocator.allocate_fixed_sized::<sel4::cap_type::Granule>()?;
            self.thread_mut(owner, id)?.fp_frame = Some(frame);
            frame
        };
        if frame
            .frame_map(
                self.caller_vspace,
                self.scratch,
                sel4::CapRights::read_write(),
                sel4::VmAttributes::EXECUTE_NEVER,
            )
            .is_err()
        {
            self.scratch_poisoned = true;
            self.thread_mut(owner, id)?.fp_broken = true;
            return Err(Error::Cleanup);
        }
        let result = f(
            unsafe { core::slice::from_raw_parts_mut(self.scratch as *mut u8, PAGE_SIZE) },
            frame,
        );
        if frame.frame_unmap().is_err() {
            self.scratch_poisoned = true;
            self.thread_mut(owner, id)?.fp_broken = true;
            return Err(Error::Cleanup);
        }
        result
    }
}
impl Caller<'_> {
    fn thread_owner(&self) -> Result<ProcessId, ProcessError> {
        self.control().or_else(|_| self.id())
    }
    fn thread_target(&self, observer: bool) -> Result<(ProcessId, u64), ProcessError> {
        self.manager
            .processes
            .iter()
            .find_map(|p| {
                p.threads
                    .entries
                    .iter()
                    .find(|t| {
                        t.control.is_some_and(|(_, badge)| badge == self.badge)
                            || observer && t.observer.is_some_and(|(_, badge)| badge == self.badge)
                    })
                    .map(|t| (p.id, t.id))
            })
            .ok_or(ProcessError::Denied)
    }
    fn process_view(&self) -> Result<ProcessId, ProcessError> {
        self.manager
            .processes
            .iter()
            .find(|p| p.threads.observer.is_some_and(|(_, b)| b == self.badge))
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)
    }
}
impl ProcessThreadsServer for Caller<'_> {
    fn create_thread(
        &mut self,
        request: ThreadCreate,
    ) -> Result<Cap<ThreadObjectTag>, ProcessError> {
        let owner = self.thread_owner()?;
        self.manager.create_thread(owner, request).map_err(error)
    }
    fn list_threads(&mut self, after: Option<u64>) -> Result<ThreadPage, ProcessError> {
        self.manager
            .thread_page(self.thread_owner()?, after)
            .map_err(error)
    }
    fn open_thread(&mut self, id: u64) -> Result<Cap<ThreadObjectTag>, ProcessError> {
        let owner = self.thread_owner()?;
        let slot = self
            .manager
            .thread_authority(owner, id, false)
            .map_err(error)?;
        self.manager.export_object(slot).map_err(error)
    }
    fn execution_context(&mut self) -> Result<Cap<ExecutionContextObjectTag>, ProcessError> {
        let owner = self.thread_owner()?;
        let p = self.manager.live(owner).map_err(error)?;
        if p.execution.is_some() {
            return Err(ProcessError::State);
        }
        let authority = if let Some(a) = p.threads.context {
            a
        } else {
            let endpoint = p.lifecycle_source.ok_or(ProcessError::Denied)?;
            let a = self.manager.mint(endpoint).map_err(error)?;
            self.manager
                .process_mut(owner)
                .map_err(error)?
                .threads
                .context = Some(a);
            a
        };
        self.manager.export_object(authority.0).map_err(error)
    }
}
impl ProcessViewServer for Caller<'_> {
    fn process_info(&mut self) -> Result<ProcessInfo, ProcessError> {
        Ok(crate::service::wire_info(
            self.manager.process(self.process_view()?).map_err(error)?,
        ))
    }
    fn list_threads(&mut self, after: Option<u64>) -> Result<ThreadPage, ProcessError> {
        self.manager
            .thread_page(self.process_view()?, after)
            .map_err(error)
    }
}
impl ThreadObserverServer for Caller<'_> {
    fn thread_info(&mut self) -> Result<ThreadInfo, ProcessError> {
        let (owner, id) = self.thread_target(true)?;
        Ok(self.manager.thread(owner, id).map_err(error)?.info())
    }
}
impl ThreadControlServer for Caller<'_> {
    fn info(&mut self) -> Result<ThreadInfo, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        Ok(self.manager.thread(owner, id).map_err(error)?.info())
    }
    fn observe(&mut self) -> Result<Cap<ThreadObserverObjectTag>, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        let slot = self
            .manager
            .thread_authority(owner, id, true)
            .map_err(error)?;
        self.manager.export_object(slot).map_err(error)
    }
    fn watch(&mut self, events: Cap<Notification>) -> Result<(), ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        let t = self.manager.thread(owner, id).map_err(error)?;
        if t.state != ThreadState::Stopped
            || t.wake.is_some()
            || events.as_ref().unwrapped_badge().is_some()
        {
            return Err(ProcessError::State);
        }
        let mut allocator = self.manager.allocator(owner).map_err(error)?;
        let slot = allocator.reserve_slot().map_err(|e| error(e.into()))?;
        let root = allocator.root();
        if let Err(err) = root.absolute_cptr(sel4::CPtr::from_bits(slot as _)).copy(
            &root.absolute_cptr(sel4::CPtr::from_bits(events.as_ref().slot as _)),
            sel4::CapRights::write_only(),
        ) {
            allocator.release_slot(slot).map_err(|e| error(e.into()))?;
            return Err(error(err.into()));
        }
        self.manager.thread_mut(owner, id).map_err(error)?.wake = Some(slot);
        Ok(())
    }
    fn registers(&mut self) -> Result<ExecutionRegisters, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        let t = self.manager.thread(owner, id).map_err(error)?;
        if t.state != ThreadState::Stopped {
            return Err(ProcessError::State);
        }
        Ok(crate::execution::encode_context(
            &t.tcb
                .tcb_read_all_registers(false)
                .map_err(|e| error(e.into()))?,
        ))
    }
    fn set_registers(&mut self, registers: ExecutionRegisters) -> Result<(), ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        if !valid_registers(&registers) {
            return Err(ProcessError::Invalid);
        }
        let t = self.manager.thread_mut(owner, id).map_err(error)?;
        if t.state != ThreadState::Stopped {
            return Err(ProcessError::State);
        }
        t.tcb
            .tcb_write_all_registers(false, &mut crate::execution::decode_context(&registers))
            .map_err(|e| error(e.into()))?;
        t.reason = None;
        Ok(())
    }
    fn ipc_buffer(&mut self) -> Result<Option<u64>, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        Ok(self
            .manager
            .thread(owner, id)
            .map_err(error)?
            .ipc
            .map(|(address, _)| address as u64))
    }
    fn allocate_ipc_buffer(&mut self) -> Result<u64, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        self.manager.allocate_thread_ipc(owner, id).map_err(error)
    }
    fn stop(&mut self) -> Result<ExecutionExit, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        self.manager.stop_thread(owner, id).map_err(error)?;
        let t = self.manager.thread(owner, id).map_err(error)?;
        let (reason, address, code) = t.reason.unwrap_or((ExecutionReason::Interrupted, 0, 0));
        Ok(ExecutionExit {
            reason,
            address,
            code,
            registers: crate::execution::encode_context(
                &t.tcb
                    .tcb_read_all_registers(false)
                    .map_err(|e| error(e.into()))?,
            ),
        })
    }
    fn fp_info(&mut self) -> Result<ExecutionFpInfo, ProcessError> {
        self.thread_target(false)?;
        Ok(fp_info())
    }
    fn fp_read(&mut self, offset: u32, length: u32) -> Result<List<u8, 256>, ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        let info = fp_info();
        let (offset, length) = (offset as usize, length as usize);
        if length == 0
            || length > 256
            || offset + length > info.bytes as usize
            || info.bytes as usize > PAGE_SIZE
        {
            return Err(ProcessError::Invalid);
        }
        let t = self.manager.thread(owner, id).map_err(error)?;
        if t.fp_cursor != 0 {
            return Err(ProcessError::State);
        }
        let tcb = t.tcb;
        let snapshot = t.fp_snapshot;
        let result = self
            .manager
            .thread_fp_access(owner, id, |bytes, frame| {
                if !snapshot {
                    tcb.tcb_read_fpu(frame, 0)?;
                }
                let mut result = List::default();
                result.len = length as u32;
                result.data[..length].copy_from_slice(&bytes[offset..offset + length]);
                Ok(result)
            })
            .map_err(error)?;
        self.manager
            .thread_mut(owner, id)
            .map_err(error)?
            .fp_snapshot = true;
        Ok(result)
    }
    fn fp_write(
        &mut self,
        offset: u32,
        data: List<u8, 256>,
        commit: bool,
    ) -> Result<(), ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        let bytes = fp_info().bytes as usize;
        let (offset, length) = (offset as usize, data.len as usize);
        let t = self.manager.thread(owner, id).map_err(error)?;
        if length == 0
            || length > 256
            || offset + length > bytes
            || bytes > PAGE_SIZE
            || offset != 0 && offset != t.fp_cursor
            || commit && offset + length != bytes
        {
            return Err(ProcessError::Invalid);
        }
        let tcb = t.tcb;
        // Staging changes the cached frame even if kernel validation rejects commit.
        self.manager
            .thread_mut(owner, id)
            .map_err(error)?
            .fp_snapshot = false;
        let result = self.manager.thread_fp_access(owner, id, |bytes, frame| {
            if offset == 0 {
                bytes.fill(0);
            }
            bytes[offset..offset + length].copy_from_slice(&data.data[..length]);
            if commit {
                tcb.tcb_write_fpu(frame, 0)?;
            }
            Ok(())
        });
        let t = self.manager.thread_mut(owner, id).map_err(error)?;
        if commit || result.is_ok() {
            t.fp_cursor = if commit { 0 } else { offset + length };
        }
        t.fp_snapshot = false;
        result.map_err(error)
    }
    fn destroy(&mut self) -> Result<(), ProcessError> {
        let (owner, id) = self.thread_target(false)?;
        self.manager.destroy_thread(owner, id).map_err(error)
    }
}
impl ExecutionContextServer for Caller<'_> {
    fn run(&mut self, thread: Cap<ThreadObjectTag>) -> Result<(), ProcessError> {
        let owner = self
            .manager
            .processes
            .iter()
            .find(|p| p.threads.context.is_some_and(|(_, b)| b == self.badge))
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)?;
        let badge = thread
            .as_ref()
            .unwrapped_badge()
            .ok_or(ProcessError::Denied)? as sel4::Word;
        let (target, id) = self
            .manager
            .processes
            .iter()
            .find_map(|p| {
                p.threads
                    .entries
                    .iter()
                    .find(|t| t.control.is_some_and(|(_, b)| b == badge))
                    .map(|t| (p.id, t.id))
            })
            .ok_or(ProcessError::Denied)?;
        let p = self.manager.live(owner).map_err(error)?;
        if p.threads.active.is_some() || !matches!(p.state, State::Created | State::Active) {
            return Err(ProcessError::State);
        }
        let sc = p.sc;
        let t = self.manager.thread(target, id).map_err(error)?;
        if t.state != ThreadState::Stopped || t.context.is_some() || t.fp_cursor != 0 || t.fp_broken
        {
            return Err(ProcessError::State);
        }
        let tcb = t.tcb;
        let fault = if let Some(fault) = t.fault {
            fault
        } else {
            let endpoint = self
                .manager
                .live(target)
                .map_err(error)?
                .lifecycle_source
                .ok_or(ProcessError::Denied)?;
            let fault = self.manager.mint(endpoint).map_err(error)?;
            self.manager.thread_mut(target, id).map_err(error)?.fault = Some(fault);
            tcb.tcb_set_timeout_endpoint(sel4::cap::Endpoint::from_bits(fault.0 as _))
                .map_err(|e| error(e.into()))?;
            fault
        };
        let fault = sel4::cap::Endpoint::from_bits(fault.0 as _);
        if !matches!(
            self.manager.live(target).map_err(error)?.state,
            State::Created | State::Active
        ) {
            return Err(ProcessError::State);
        }
        tcb.tcb_set_sched_params(sel4::init_thread::slot::TCB.cap(), 0, 0, sc, fault)
            .map_err(|e| error(e.into()))?;
        // Record the binding before resume so a failed resume cannot lose ownership.
        self.manager
            .process_mut(owner)
            .map_err(error)?
            .threads
            .active = Some((target, id));
        let t = self.manager.thread_mut(target, id).map_err(error)?;
        t.context = Some(owner);
        t.reason = None;
        t.fp_snapshot = false;
        tcb.tcb_resume().map_err(|e| error(e.into()))?;
        self.manager.thread_mut(target, id).map_err(error)?.state = ThreadState::Running;
        self.manager.process_mut(target).map_err(error)?.state = State::Active;
        Ok(())
    }
    fn stop(&mut self) -> Result<(), ProcessError> {
        let owner = self
            .manager
            .processes
            .iter()
            .find(|p| p.threads.context.is_some_and(|(_, b)| b == self.badge))
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)?;
        self.manager.stop_context(owner).map_err(error)
    }
}
