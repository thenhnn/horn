//! Capability-scoped launch and management; the root server remains sequential.
use crate::{Caller, Config, Error, Exit, Manager, NEXT_SELF_BADGE, ProcessId, State, protocol::*};
use core::ops::Range;
use core::sync::atomic::Ordering;
use horn_ipc::*;
use horn_vm::{
    protocol::VmError,
    sel4::{FramePool, PAGE_SIZE},
    service::Service as VmService,
};

#[derive(Clone)]
pub struct LaunchPolicy {
    /// Bounds retained records, including exited children until control.close().
    pub max_children: usize,
    pub max_ram: usize,
    pub vm_window: Range<usize>,
    /// Each child receives this fixed SC; max_children also bounds CPU authority.
    pub cpu_budget_us: u64,
    pub cpu_period_us: u64,
}
pub(crate) struct Grant {
    factory: Option<(usize, sel4::Word)>,
    observer: Option<(usize, sel4::Word)>,
    endpoint: sel4::cap::Endpoint,
    policy: LaunchPolicy,
}
fn error(e: Error) -> ProcessError {
    match e {
        Error::InvalidElf => ProcessError::InvalidElf,
        Error::InvalidAbi => ProcessError::InvalidAbi,
        Error::Denied => ProcessError::Denied,
        Error::InvalidState => ProcessError::State,
        Error::Stale => ProcessError::Stale,
        Error::Cleanup => ProcessError::Quarantined,
        Error::Kernel(_) => ProcessError::Backend,
        Error::Resource(horn_vm::Error::QuotaExceeded) => ProcessError::QuotaExceeded,
        Error::Resource(horn_vm::Error::InvalidSize) => ProcessError::Invalid,
        Error::Resource(horn_vm::Error::Backend) => ProcessError::Quarantined,
        Error::Resource(_) => ProcessError::Exhausted,
    }
}
fn vm_error(e: VmError) -> ProcessError {
    match e {
        VmError::Denied => ProcessError::Denied,
        VmError::Invalid => ProcessError::Invalid,
        VmError::QuotaExceeded => ProcessError::QuotaExceeded,
        VmError::Quarantined => ProcessError::Quarantined,
        _ => ProcessError::Backend,
    }
}
fn wire_info(p: &crate::Process) -> ProcessInfo {
    let usage = p.allocator.account().usage();
    let mut name = List::<u8, 64>::default();
    name.len = p.name.len() as u32;
    name.data[..p.name.len()].copy_from_slice(p.name.as_bytes());
    ProcessInfo {
        id: p.id.0,
        name,
        state: match p.state {
            State::Created => ProcessState::Created,
            State::Active => ProcessState::Active,
            State::Stopping => ProcessState::Stopping,
            State::Exited => ProcessState::Exited,
            State::Quarantined => ProcessState::Quarantined,
        },
        exit: p.exit.map(|exit| match exit {
            Exit::Code(code) => ProcessExit::Code(code),
            Exit::Killed => ProcessExit::Killed,
        }),
        ram: ProcessUsage {
            limit_bytes: usage.limit as _,
            charged_bytes: usage.charged as _,
            quarantined_bytes: usage.quarantined as _,
        },
    }
}
impl Manager {
    /// Trusted boot setup: prepare a current-VSpace reader window.
    /// # Safety
    /// The caller must reserve this unmapped range exclusively for the manager's
    /// lifetime; it must not overlap any Rust allocations or scratch mappings.
    pub unsafe fn enable_launch(&mut self, window: Range<usize>) -> Result<(), Error> {
        if self.launch_poisoned
            || self.launch_window.is_some()
            || window.start >= window.end
            || window.start % PAGE_SIZE != 0
            || window.end % PAGE_SIZE != 0
            || window.contains(&self.scratch)
            || window.len() > isize::MAX as usize
        {
            return Err(Error::Denied);
        }
        crate::loader::prepare_window(&mut self.allocator, self.caller_vspace, window.clone())?;
        self.launch_window = Some(window);
        Ok(())
    }
    pub fn attach_vm(&mut self, id: ProcessId, vm: VmService) -> Result<(), Error> {
        let p = self.process_mut(id)?;
        if p.vm.is_some() || p.state != State::Created {
            return Err(Error::InvalidState);
        }
        p.vm = Some(vm);
        Ok(())
    }
    fn mint(&mut self, endpoint: sel4::cap::Endpoint) -> Result<(usize, sel4::Word), Error> {
        let badge = NEXT_SELF_BADGE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| Error::Denied)?;
        let slot = self.allocator.reserve_slot()?;
        if let Err(e) = self
            .allocator
            .root()
            .absolute_cptr(sel4::CPtr::from_bits(slot as _))
            .mint(
                &self.allocator.root().absolute_cptr(endpoint.cptr()),
                sel4::CapRights::new(true, true, false, true),
                badge,
            )
        {
            self.allocator.release_slot(slot)?;
            return Err(e.into());
        }
        Ok((slot, badge))
    }
    fn release_authority(&mut self, slot: usize) -> Result<(), Error> {
        self.allocator
            .root()
            .absolute_cptr(sel4::CPtr::from_bits(slot as _))
            .revoke()?;
        self.allocator.release_slot(slot)?;
        Ok(())
    }
    pub fn provision_launch(
        &mut self,
        owner: ProcessId,
        endpoint: sel4::cap::Endpoint,
        policy: LaunchPolicy,
    ) -> Result<(sel4::cap::Endpoint, sel4::cap::Endpoint), Error> {
        if self.launch_window.is_none()
            || self.process(owner)?.launch.is_some()
            || !matches!(self.process(owner)?.state, State::Created | State::Active)
            || policy.max_children == 0
            || policy.max_children > 64
            || policy.max_ram == 0
            || policy.cpu_budget_us == 0
            || policy.cpu_budget_us > policy.cpu_period_us
        {
            return Err(Error::Denied);
        }
        let factory = self.mint(endpoint)?;
        let observer = match self.mint(endpoint) {
            Ok(value) => value,
            Err(e) => {
                self.release_authority(factory.0)?;
                return Err(e);
            }
        };
        self.process_mut(owner)?.launch = Some(Grant {
            factory: Some(factory),
            observer: Some(observer),
            endpoint,
            policy,
        });
        Ok((
            sel4::Cap::from_bits(factory.0 as _),
            sel4::Cap::from_bits(observer.0 as _),
        ))
    }
    pub(crate) fn close_grant(&mut self, id: ProcessId) -> Result<(), Error> {
        for factory in [true, false] {
            let grant = self.process(id)?.launch.as_ref();
            let authority = grant.and_then(|g| if factory { g.factory } else { g.observer });
            if let Some((slot, _)) = authority {
                self.release_authority(slot)?;
                let g = self.process_mut(id)?.launch.as_mut().unwrap();
                if factory {
                    g.factory = None;
                } else {
                    g.observer = None;
                }
            }
        }
        self.process_mut(id)?.launch = None;
        Ok(())
    }
    pub(crate) fn retire(&mut self, id: ProcessId) -> Result<(), Error> {
        if self.process(id)?.state != State::Exited {
            return Err(Error::InvalidState);
        }
        if let Some((slot, _)) = self.process(id)?.control {
            self.release_authority(slot)?;
            self.process_mut(id)?.control = None;
        }
        self.processes.retain(|p| p.id != id);
        Ok(())
    }
    fn export_control(&mut self, id: ProcessId) -> Result<Cap<ProcessObjectTag>, Error> {
        if self.outgoing.is_some() {
            return Err(Error::InvalidState);
        }
        let authority = self.process(id)?.control.ok_or(Error::Stale)?.0;
        let slot = self.allocator.reserve_slot()?;
        let root = self.allocator.root();
        if let Err(e) = root.absolute_cptr(sel4::CPtr::from_bits(slot as _)).copy(
            &root.absolute_cptr(sel4::CPtr::from_bits(authority as _)),
            sel4::CapRights::all(),
        ) {
            self.allocator.release_slot(slot)?;
            return Err(e.into());
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
    pub fn dispatch_vm(
        &mut self,
        request: horn_vm::protocol::VmHostRequest,
        badge: sel4::Word,
    ) -> Option<horn_vm::protocol::VmHostResponse> {
        self.processes
            .iter_mut()
            .filter_map(|p| p.vm.as_mut())
            .find(|vm| vm.handles(badge))
            .map(|vm| vm.dispatch(request, badge))
    }
    pub fn finish_reply(&mut self, server: &mut Sel4Server<Sel4CapSpace>) -> Result<(), RpcError> {
        if let Some(slot) = self.outgoing.take() {
            self.allocator
                .release_slot(slot)
                .map_err(|_| RpcError::Transport)?;
        }
        for vm in self.processes.iter_mut().filter_map(|p| p.vm.as_mut()) {
            vm.finish_exports()?;
        }
        server.recv_slot.delete().map_err(|_| RpcError::Transport)
    }
    fn bootstrap(
        &mut self,
        id: ProcessId,
        endpoint: sel4::cap::Endpoint,
        window: Range<usize>,
    ) -> Result<(), Error> {
        let mut allocator = self.allocator(id)?;
        let reply = allocator.allocate(sel4::ObjectBlueprint::Reply)?;
        let lifecycle = self.provision_self(id, endpoint)?;
        let vm = VmService::new(
            allocator.clone(),
            FramePool::for_account(allocator),
            endpoint,
            self.vspace(id)?,
            window,
            128,
        )
        .map_err(|_| Error::Cleanup)?;
        let caps = [
            (PROCESS_CAPS_SELF_SLOT, self.cnode(id)?.bits()),
            (PROCESS_CAPS_VM_SLOT, vm.factory().bits()),
            (PROCESS_CAPS_ADDRESS_SPACE_SLOT, vm.address_space().bits()),
            (PROCESS_CAPS_LIFECYCLE_SLOT, lifecycle.bits()),
            (PROCESS_CAPS_REPLY_SLOT, reply.bits()),
        ];
        self.attach_vm(id, vm)?;
        for (slot, source) in caps {
            self.cnode(id)?
                .absolute_cptr_from_bits_with_depth(slot as _, PROCESS_CAPS_SIZE_BITS as _)
                .copy(
                    &self
                        .allocator
                        .root()
                        .absolute_cptr(sel4::CPtr::from_bits(source)),
                    sel4::CapRights::all(),
                )?;
        }
        let control = self.mint(endpoint)?;
        self.process_mut(id)?.control = Some(control);
        Ok(())
    }
    fn discard_launch(&mut self, id: ProcessId) -> Result<(), Error> {
        self.stop(id, Exit::Killed)?;
        self.finish(id)?;
        self.retire(id)
    }
    fn launch(
        &mut self,
        owner: ProcessId,
        cap: CapRef,
        request: ProcessLaunch,
    ) -> Result<Cap<ProcessObjectTag>, ProcessError> {
        let grant = self
            .process(owner)
            .map_err(error)?
            .launch
            .as_ref()
            .ok_or(ProcessError::Denied)?;
        let policy = grant.policy.clone();
        let endpoint = grant.endpoint;
        let name = core::str::from_utf8(&request.name.data[..request.name.len as usize])
            .map_err(|_| ProcessError::Invalid)?;
        if name.is_empty() || name.as_bytes().contains(&0) {
            return Err(ProcessError::Invalid);
        }
        let ram = usize::try_from(request.ram_limit).map_err(|_| ProcessError::Invalid)?;
        if ram == 0 || ram > policy.max_ram {
            return Err(ProcessError::Denied);
        }
        if self
            .processes
            .iter()
            .filter(|p| p.parent == Some(owner))
            .count()
            >= policy.max_children
        {
            return Err(ProcessError::Exhausted);
        }
        let window = self
            .launch_window
            .clone()
            .ok_or(ProcessError::Quarantined)?;
        let source = self
            .processes
            .iter()
            .find(|p| p.vm.as_ref().is_some_and(|vm| vm.owns_memory(cap)))
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)?;
        let mut vm = self.process_mut(source).map_err(error)?.vm.take().unwrap();
        let mut created = None;
        let read = unsafe {
            vm.with_sealed_bytes(cap, self.caller_vspace, window, |bytes| {
                created = Some(self.create_in(
                    bytes,
                    Config {
                        name,
                        ram_limit: ram,
                        cnode_bits: PROCESS_CAPS_SIZE_BITS as _,
                        vm_window: policy.vm_window.clone(),
                        cpu_budget_us: policy.cpu_budget_us,
                        cpu_period_us: policy.cpu_period_us,
                    },
                    Some(owner),
                ));
            })
        };
        self.process_mut(source).map_err(error)?.vm = Some(vm);
        if let Err(e) = read {
            if e == VmError::Quarantined {
                self.launch_window = None;
                self.launch_poisoned = true;
            }
            if let Some(Ok(id)) = created {
                self.discard_launch(id).map_err(error)?;
            }
            return Err(vm_error(e));
        }
        let id = created.unwrap().map_err(|e| {
            if e == Error::Cleanup {
                self.launch_poisoned = true;
                self.launch_window = None;
            }
            error(e)
        })?;
        let provisioned = self
            .bootstrap(id, endpoint, policy.vm_window)
            .and_then(|()| self.export_control(id));
        match provisioned {
            Ok(cap) => Ok(cap),
            Err(e) => {
                self.discard_launch(id).map_err(error)?;
                Err(error(e))
            }
        }
    }
}
impl Caller<'_> {
    fn scope(&self, factory: bool) -> Result<ProcessId, ProcessError> {
        self.manager
            .processes
            .iter()
            .find(|p| {
                matches!(p.state, State::Created | State::Active)
                    && p.launch.as_ref().is_some_and(|g| {
                        (if factory { g.factory } else { g.observer })
                            .is_some_and(|(_, badge)| badge == self.badge)
                    })
            })
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)
    }
    fn control(&self) -> Result<ProcessId, ProcessError> {
        self.manager
            .processes
            .iter()
            .find(|p| p.control.is_some_and(|(_, badge)| badge == self.badge))
            .map(|p| p.id)
            .ok_or(ProcessError::Denied)
    }
}
impl ProcessFactory for Caller<'_> {
    fn launch(
        &mut self,
        elf: Cap<VmMemoryObjectTag>,
        request: ProcessLaunch,
    ) -> Result<Cap<ProcessObjectTag>, ProcessError> {
        self.manager
            .launch(self.scope(true)?, elf.as_ref(), request)
    }
    fn open(&mut self, id: u64) -> Result<Cap<ProcessObjectTag>, ProcessError> {
        let owner = self.scope(true)?;
        let id = ProcessId(id);
        if self.manager.process(id).map_err(error)?.parent != Some(owner) {
            return Err(ProcessError::Denied);
        }
        self.manager.export_control(id).map_err(error)
    }
}
impl ProcessObserver for Caller<'_> {
    fn list(&mut self, after: u64) -> Result<ProcessPage, ProcessError> {
        let owner = self.scope(false)?;
        let mut entries = List::default();
        let mut children = self
            .manager
            .processes
            .iter()
            .filter(|p| p.parent == Some(owner) && p.id.0 > after);
        for p in children.by_ref().take(4) {
            entries.data[entries.len as usize] = wire_info(p);
            entries.len += 1;
        }
        let next = if children.next().is_some() {
            entries.data[entries.len as usize - 1].id
        } else {
            0
        };
        Ok(ProcessPage { entries, next })
    }
}
impl ProcessControl for Caller<'_> {
    fn info(&mut self) -> Result<ProcessInfo, ProcessError> {
        Ok(wire_info(
            self.manager.process(self.control()?).map_err(error)?,
        ))
    }
    fn start(&mut self) -> Result<(), ProcessError> {
        self.manager.start(self.control()?).map_err(error)
    }
    fn set_limit(&mut self, limit: u64) -> Result<(), ProcessError> {
        let id = self.control()?;
        let parent = self
            .manager
            .process(id)
            .map_err(error)?
            .parent
            .ok_or(ProcessError::Denied)?;
        let max = self
            .manager
            .process(parent)
            .map_err(error)?
            .launch
            .as_ref()
            .ok_or(ProcessError::Denied)?
            .policy
            .max_ram;
        let limit = usize::try_from(limit).map_err(|_| ProcessError::Invalid)?;
        if limit > max {
            return Err(ProcessError::Denied);
        }
        self.manager.set_limit(id, limit).map_err(error)
    }
    fn terminate(&mut self) -> Result<(), ProcessError> {
        let id = self.control()?;
        self.manager.stop(id, Exit::Killed).map_err(error)?;
        self.manager.finish(id).map_err(error)
    }
    fn close(&mut self) -> Result<(), ProcessError> {
        self.manager.retire(self.control()?).map_err(error)
    }
}
