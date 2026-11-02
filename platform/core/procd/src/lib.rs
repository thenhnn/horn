//! Root-hosted process lifecycle. No raw VSpace or scheduling authority is exported.
#![no_std]
extern crate alloc;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};
use horn_vm::{
    account::Usage,
    sel4::{KernelAllocator, PAGE_SIZE},
};

pub mod elf;
mod execution;
mod loader;
mod placement;
mod service;
mod startup;
pub use service::LaunchPolicy;
#[cfg(test)]
mod tests;

#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

static NEXT_SELF_BADGE: AtomicU64 = AtomicU64::new(1 << 32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidElf,
    InvalidAbi,
    Denied,
    InvalidState,
    Stale,
    Resource(horn_vm::Error),
    Kernel(sel4::Error),
    Cleanup,
}
impl From<horn_vm::Error> for Error {
    fn from(value: horn_vm::Error) -> Self {
        Self::Resource(value)
    }
}
impl From<sel4::Error> for Error {
    fn from(value: sel4::Error) -> Self {
        Self::Kernel(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Created,
    Active,
    Stopping,
    Exited,
    Quarantined,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(u32),
    Killed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessId(u64);

pub struct Config<'a> {
    pub name: &'a str,
    pub ram_limit: usize,
    pub cnode_bits: usize,
    pub vm_window: Range<usize>,
    pub cpu_budget_us: u64,
    pub cpu_period_us: u64,
}
struct Process {
    id: ProcessId,
    parent: Option<ProcessId>,
    vm: Option<horn_vm::service::Service>,
    control: Option<(usize, sel4::Word)>,
    launch: Option<service::Grant>,
    name: String,
    cpu: usize,
    state: State,
    exit: Option<Exit>,
    allocator: KernelAllocator,
    cnode: sel4::cap::CNode,
    tcb: sel4::cap::Tcb,
    loaded: loader::Loaded,
    entry: usize,
    abi: u8,
    ipc_address: usize,
    cnode_bits: usize,
    bootstrap_end: u32,
    startup: Option<(usize, usize)>,
    startup_attempted: bool,
    resident_limit: usize,
    lifecycle: Option<(usize, sel4::Word)>,
    exit_signal: Option<usize>,
    fault_endpoint: Option<usize>,
    lifecycle_source: Option<sel4::cap::Endpoint>,
    execution: Option<execution::Execution>,
    execution_poisoned: bool,
    resident_end: usize,
    sc: sel4::cap::SchedContext,
    budget_us: u64,
    period_us: u64,
}
pub struct Info<'a> {
    pub id: ProcessId,
    pub name: &'a str,
    pub state: State,
    pub exit: Option<Exit>,
    pub ram: Usage,
}

/// Trusted IOMMU broker token; dropping it does not release device-accessible RAM.
#[must_use]
pub struct DmaPin {
    process: ProcessId,
    pin: horn_vm::dma::Pin,
}
impl DmaPin {
    pub fn pages(&self) -> &[u64] {
        self.pin.pages()
    }
}

#[must_use]
pub struct MmioLease {
    process: ProcessId,
    lease: horn_vm::service::MmioLease,
}

pub struct Manager {
    allocator: KernelAllocator,
    caller_vspace: sel4::cap::VSpace,
    scratch: usize,
    scratch_poisoned: bool,
    asid_pool: sel4::cap::AsidPool,
    schedulers: Vec<sel4::cap::SchedControl>,
    host_cpu: usize,
    processes: Vec<Process>,
    pending_exit: Option<ProcessId>,
    next_id: u64,
    launch_window: Option<Range<usize>>,
    launch_poisoned: bool,
    outgoing: Option<usize>,
}
impl Manager {
    /// # Safety
    /// `caller_vspace` must remain current while using this manager. `scratch`
    /// must be an exclusively reserved, unmapped page with translation tables
    /// for the manager's lifetime; it must not overlap Rust allocations.
    /// `schedulers[cpu]` must be that logical CPU's boot SchedControl capability.
    /// The host remains on `host_cpu`; all manager/allocator access stays serialized.
    pub unsafe fn new(
        allocator: KernelAllocator,
        caller_vspace: sel4::cap::VSpace,
        scratch: usize,
        asid_pool: sel4::cap::AsidPool,
        schedulers: Vec<sel4::cap::SchedControl>,
        host_cpu: usize,
    ) -> Self {
        assert!(
            (1..=placement::MAX_CPUS).contains(&schedulers.len()) && host_cpu < schedulers.len()
        );
        Self {
            allocator,
            caller_vspace,
            scratch,
            scratch_poisoned: false,
            asid_pool,
            schedulers,
            host_cpu,
            processes: Vec::new(),
            pending_exit: None,
            next_id: 1,
            launch_window: None,
            launch_poisoned: false,
            outgoing: None,
        }
    }
    pub fn cpu_count(&self) -> usize {
        self.schedulers.len()
    }
    /// Read-only initial placement; neither this number nor a PID grants authority.
    pub fn cpu(&self, id: ProcessId) -> Result<usize, Error> {
        Ok(self.process(id)?.cpu)
    }
    /// Build a suspended process. The trusted launcher installs bootstrap caps, then calls start.
    pub fn create(&mut self, bytes: &[u8], config: Config<'_>) -> Result<ProcessId, Error> {
        self.create_in(bytes, config, None)
    }
    fn create_in(
        &mut self,
        bytes: &[u8],
        config: Config<'_>,
        parent: Option<ProcessId>,
    ) -> Result<ProcessId, Error> {
        if self.scratch_poisoned {
            return Err(Error::Cleanup);
        }
        if self.processes.len() >= 64 {
            return Err(Error::Resource(horn_vm::Error::MetadataExhausted));
        }
        let id = ProcessId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Denied)?;
        self.processes
            .try_reserve(1)
            .map_err(|_| Error::Resource(horn_vm::Error::MetadataExhausted))?;
        if config.name.is_empty()
            || config.name.len() > 64
            || config.name.as_bytes().contains(&0)
            || config.cnode_bits == 0
            || config.cnode_bits > 16
            || config.cpu_budget_us == 0
            || config.cpu_budget_us > config.cpu_period_us
            || config.vm_window.start < PAGE_SIZE
            || config.vm_window.start >= config.vm_window.end
            || config.vm_window.start % PAGE_SIZE != 0
            || config.vm_window.end % PAGE_SIZE != 0
        {
            return Err(Error::Denied);
        }
        // Process ownership does not imply RAM funding: all process accounts
        // are peers under the system allocator, never under the launcher's quota.
        let account = self.allocator.account().child(config.ram_limit)?;
        let image = elf::Image::parse(
            bytes,
            PAGE_SIZE..config.vm_window.start,
            config.ram_limit / PAGE_SIZE,
        )?;
        let mut allocator = self.allocator.for_account(account);
        let cpu = placement::initial_cpu(
            self.cpu_count(),
            self.host_cpu,
            self.processes
                .iter()
                .filter(|p| matches!(p.state, State::Created | State::Active))
                .map(|p| p.cpu),
        );
        let result = (|| {
            let loaded = loader::load(
                &mut allocator,
                &image,
                self.caller_vspace,
                self.scratch,
                self.asid_pool,
                config.vm_window.clone(),
            )?;
            let cnode =
                allocator.allocate_variable_sized::<sel4::cap_type::CNode>(config.cnode_bits)?;
            let tcb = allocator.allocate_fixed_sized::<sel4::cap_type::Tcb>()?;
            let sc = allocator.allocate_variable_sized::<sel4::cap_type::SchedContext>(
                sel4::sys::seL4_MinSchedContextBits as usize,
            )?;
            self.schedulers[cpu].sched_control_configure_flags(
                sc,
                config.cpu_budget_us,
                config.cpu_period_us,
                0,
                0,
                0,
            )?;
            tcb.tcb_set_sched_params(
                sel4::init_thread::slot::TCB.cap(),
                0,
                0,
                sc,
                sel4::init_thread::slot::NULL
                    .cast::<sel4::cap_type::Endpoint>()
                    .cap(),
            )?;
            tcb.tcb_configure(
                cnode,
                sel4::CNodeCapData::new(0, sel4::WORD_SIZE - config.cnode_bits),
                loaded.vspace,
                image.ipc_address as sel4::Word,
                loaded.ipc,
            )?;
            sel4::sel4_cfg_if! {
                if #[sel4_cfg(DEBUG_BUILD)] { tcb.debug_name(config.name.as_bytes()); }
            }
            Ok::<_, Error>((loaded, cnode, tcb, sc))
        })();
        let (loaded, cnode, tcb, sc) = match result {
            Ok(value) => value,
            Err(error) => {
                allocator.destroy_account().map_err(|_| Error::Cleanup)?;
                return Err(error);
            }
        };
        self.processes.push(Process {
            id,
            parent,
            vm: None,
            control: None,
            launch: None,
            name: config.name.to_string(),
            cpu,
            state: State::Created,
            exit: None,
            allocator,
            cnode,
            tcb,
            loaded,
            entry: image.entry,
            abi: image.abi,
            ipc_address: image.ipc_address,
            cnode_bits: config.cnode_bits,
            bootstrap_end: 2,
            startup: None,
            startup_attempted: false,
            resident_limit: config.vm_window.start,
            lifecycle: None,
            exit_signal: None,
            fault_endpoint: None,
            lifecycle_source: None,
            execution: None,
            execution_poisoned: false,
            resident_end: image.ipc_address + PAGE_SIZE,
            sc,
            budget_us: config.cpu_budget_us,
            period_us: config.cpu_period_us,
        });
        Ok(id)
    }
    fn process(&self, id: ProcessId) -> Result<&Process, Error> {
        self.processes
            .iter()
            .find(|p| p.id == id)
            .ok_or(Error::Stale)
    }
    fn live(&self, id: ProcessId) -> Result<&Process, Error> {
        let p = self.process(id)?;
        if matches!(p.state, State::Exited | State::Quarantined) {
            return Err(Error::InvalidState);
        }
        Ok(p)
    }
    fn process_mut(&mut self, id: ProcessId) -> Result<&mut Process, Error> {
        self.processes
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(Error::Stale)
    }
    pub fn info(&self, id: ProcessId) -> Result<Info<'_>, Error> {
        let p = self.process(id)?;
        Ok(Info {
            id,
            name: &p.name,
            state: p.state,
            exit: p.exit,
            ram: p.allocator.account().usage(),
        })
    }
    pub fn list(&self) -> impl Iterator<Item = Info<'_>> {
        self.processes.iter().map(|p| Info {
            id: p.id,
            name: &p.name,
            state: p.state,
            exit: p.exit,
            ram: p.allocator.account().usage(),
        })
    }
    pub fn set_limit(&mut self, id: ProcessId, limit: usize) -> Result<(), Error> {
        let p = self.process(id)?;
        if matches!(
            p.state,
            State::Stopping | State::Exited | State::Quarantined
        ) {
            return Err(Error::InvalidState);
        }
        p.allocator.account().set_limit(limit).map_err(Into::into)
    }
    /// Trusted in-root access for bootstrap and VM; never sent to the child.
    pub fn allocator(&self, id: ProcessId) -> Result<KernelAllocator, Error> {
        Ok(self.live(id)?.allocator.clone())
    }
    pub fn cnode(&self, id: ProcessId) -> Result<sel4::cap::CNode, Error> {
        Ok(self.live(id)?.cnode)
    }
    pub fn tcb(&self, id: ProcessId) -> Result<sel4::cap::Tcb, Error> {
        Ok(self.live(id)?.tcb)
    }
    pub fn vspace(&self, id: ProcessId) -> Result<sel4::cap::VSpace, Error> {
        Ok(self.live(id)?.loaded.vspace)
    }
    /// Root routing only: `memory` must come from the kernel receive path on
    /// the shared VM endpoint, never from caller-supplied numeric badge data.
    pub fn pin_dma(
        &mut self,
        memory: horn_ipc::CapRef,
        offset: u64,
        length: u64,
        access: horn_vm::protocol::VmProtection,
    ) -> Result<DmaPin, horn_vm::protocol::VmError> {
        for process in &mut self.processes {
            if !matches!(process.state, State::Created | State::Active) {
                continue;
            }
            if let Some(vm) = &mut process.vm {
                if vm.owns_memory(memory) {
                    return Ok(DmaPin {
                        process: process.id,
                        pin: vm.pin_dma(memory, offset, length, access)?,
                    });
                }
            }
        }
        Err(horn_vm::protocol::VmError::Denied)
    }
    /// Trusted device broker only. `space` must be kernel-unwrapped on the VM
    /// endpoint. Ordinary clients never select a destination by numeric ID.
    pub fn import_mmio(
        &mut self,
        space: horn_ipc::CapRef,
        bank: &mut horn_vm::sel4::device::DeviceBank,
        address: usize,
        length: usize,
        ceiling: horn_vm::protocol::VmProtection,
    ) -> Result<
        (
            MmioLease,
            horn_ipc::Cap<horn_vm::protocol::VmMemoryObjectTag>,
        ),
        horn_vm::protocol::VmError,
    > {
        for process in &mut self.processes {
            if !matches!(process.state, State::Created | State::Active) {
                continue;
            }
            if let Some(vm) = &mut process.vm {
                if vm.owns_address_space(space) {
                    let backing = bank.memory(address, length).map_err(|e| match e {
                        horn_vm::Error::InvalidSize => horn_vm::protocol::VmError::Invalid,
                        horn_vm::Error::Backend => horn_vm::protocol::VmError::Quarantined,
                        _ => horn_vm::protocol::VmError::Exhausted,
                    })?;
                    let (lease, cap) = vm.import_mmio(backing, ceiling)?;
                    return Ok((
                        MmioLease {
                            process: process.id,
                            lease,
                        },
                        cap,
                    ));
                }
            }
        }
        Err(horn_vm::protocol::VmError::Denied)
    }
    pub fn revoke_mmio(&mut self, token: MmioLease) -> Result<(), horn_vm::protocol::VmError> {
        let process = self
            .process_mut(token.process)
            .map_err(|_| horn_vm::protocol::VmError::Stale)?;
        process
            .vm
            .as_mut()
            .ok_or(horn_vm::protocol::VmError::Stale)?
            .revoke_mmio(token.lease)
    }
    /// # Safety
    /// All translations must be removed and DMA drained before releasing this
    /// token. A stopping/quarantined process intentionally retains its VM.
    pub unsafe fn unpin_dma(&mut self, token: DmaPin) -> Result<(), horn_vm::protocol::VmError> {
        let process = self
            .process_mut(token.process)
            .map_err(|_| horn_vm::protocol::VmError::Stale)?;
        let vm = process
            .vm
            .as_mut()
            .ok_or(horn_vm::protocol::VmError::Stale)?;
        unsafe { vm.unpin_dma(token.pin) }
    }
    pub fn provision_self(
        &mut self,
        id: ProcessId,
        endpoint: sel4::cap::Endpoint,
    ) -> Result<sel4::cap::Endpoint, Error> {
        let p = self.process_mut(id)?;
        if p.state != State::Created || p.lifecycle.is_some() {
            return Err(Error::InvalidState);
        }
        let badge = NEXT_SELF_BADGE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1).filter(|next| *next < (1 << 48))
            })
            .map_err(|_| Error::Denied)?;
        let slot = p.allocator.reserve_slot()?;
        let root = p.allocator.root();
        if let Err(error) = root.absolute_cptr(sel4::CPtr::from_bits(slot as _)).mint(
            &root.absolute_cptr(endpoint.cptr()),
            sel4::CapRights::new(true, true, false, true),
            badge,
        ) {
            p.allocator.release_slot(slot)?;
            return Err(error.into());
        }
        p.lifecycle = Some((slot, badge));
        p.lifecycle_source = Some(endpoint);
        Ok(sel4::Cap::from_bits(slot as _))
    }
    pub fn dispatch(
        &mut self,
        request: protocol::ProcessHostRequest,
        badge: sel4::Word,
    ) -> protocol::ProcessHostResponse {
        protocol::process_host_dispatch(
            request,
            &mut Caller {
                manager: self,
                badge,
            },
        )
    }
    pub fn take_exit(&mut self) -> Option<ProcessId> {
        self.pending_exit.take()
    }
    pub fn start(&mut self, id: ProcessId) -> Result<(), Error> {
        let p = self.process_mut(id)?;
        if p.state != State::Created {
            return Err(Error::InvalidState);
        }
        let mut regs = sel4::UserContext::default();
        *regs.pc_mut() = p.entry as sel4::Word;
        if p.abi == 2 {
            let (address, length) = p.startup.ok_or(Error::InvalidState)?;
            regs.inner_mut().rdi = address as _;
            regs.inner_mut().rsi = length as _;
        }
        p.tcb.tcb_write_all_registers(true, &mut regs)?;
        p.state = State::Active;
        Ok(())
    }
    /// Revoke exported execution authority and stop the entire owned subtree.
    /// No memory is reclaimed until finish has observed successful stop.
    pub fn stop(&mut self, id: ProcessId, exit: Exit) -> Result<(), Error> {
        let p = self.process_mut(id)?;
        if !matches!(p.state, State::Created | State::Active | State::Stopping) {
            return Err(Error::InvalidState);
        }
        if let Some(execution) = &mut p.execution {
            execution.stop_user()?;
        }
        // Raw bootstrap TCB copies must not permit a concurrent resume.
        p.allocator.root().absolute_cptr(p.tcb.cptr()).revoke()?;
        p.tcb.tcb_suspend()?;
        p.state = State::Stopping;
        p.exit = Some(exit);
        p.allocator.account().close();
        let children: Vec<_> = self
            .processes
            .iter()
            .filter(|p| {
                p.parent == Some(id)
                    && matches!(p.state, State::Created | State::Active | State::Stopping)
            })
            .map(|p| p.id)
            .collect();
        let mut failed = false;
        for child in children {
            failed |= self.stop(child, Exit::Killed).is_err();
        }
        if failed {
            return Err(Error::Cleanup);
        }
        Ok(())
    }
    pub fn finish(&mut self, id: ProcessId) -> Result<(), Error> {
        if self.process(id)?.state != State::Stopping {
            return Err(Error::InvalidState);
        }
        let children: Vec<_> = self
            .processes
            .iter()
            .filter(|p| p.parent == Some(id))
            .map(|p| p.id)
            .collect();
        for child in children {
            if self.process(child)?.state != State::Exited {
                self.finish(child)?;
            }
            self.retire(child)?;
        }
        let p = self.process_mut(id)?;
        if let Some(vm) = &mut p.vm {
            if vm.shutdown().is_err() {
                p.state = State::Quarantined;
                return Err(Error::Cleanup);
            }
        }
        p.vm = None;
        if let Some(execution) = &mut p.execution {
            if execution.cleanup().is_err() {
                p.state = State::Quarantined;
                return Err(Error::Cleanup);
            }
        }
        p.execution = None;
        for slot in [p.exit_signal.take(), p.fault_endpoint.take()]
            .into_iter()
            .flatten()
        {
            p.allocator.release_slot(slot)?;
        }
        self.close_grant(id)?;
        let p = self.process_mut(id)?;
        if let Some((slot, _)) = p.lifecycle {
            p.allocator
                .root()
                .absolute_cptr(sel4::CPtr::from_bits(slot as _))
                .revoke()?;
            p.allocator.release_slot(slot)?;
            p.lifecycle = None;
        }
        if p.allocator.destroy_account().is_err() {
            p.state = State::Quarantined;
            return Err(Error::Cleanup);
        }
        p.state = State::Exited;
        Ok(())
    }
}

struct Caller<'a> {
    manager: &'a mut Manager,
    badge: sel4::Word,
}
impl Caller<'_> {
    fn id(&self) -> Result<ProcessId, protocol::ProcessError> {
        self.manager
            .processes
            .iter()
            .find(|p| p.lifecycle.is_some_and(|(_, badge)| badge == self.badge))
            .map(|p| p.id)
            .ok_or(protocol::ProcessError::Denied)
    }
}
impl protocol::ProcessLifecycle for Caller<'_> {
    fn placement(&mut self) -> Result<protocol::ProcessPlacement, protocol::ProcessError> {
        let cpu = self
            .manager
            .cpu(self.id()?)
            .map_err(|_| protocol::ProcessError::State)?;
        Ok(protocol::ProcessPlacement {
            cpu: cpu as u32,
            cpu_count: self.manager.cpu_count() as u32,
        })
    }
    fn info(&mut self) -> Result<protocol::ProcessUsage, protocol::ProcessError> {
        let usage = self
            .manager
            .info(self.id()?)
            .map_err(|_| protocol::ProcessError::State)?
            .ram;
        Ok(protocol::ProcessUsage {
            limit_bytes: usage.limit as u64,
            charged_bytes: usage.charged as u64,
            quarantined_bytes: usage.quarantined as u64,
        })
    }
    fn exit(&mut self, status: u32) -> Result<(), protocol::ProcessError> {
        let id = self.id()?;
        self.manager
            .stop(id, Exit::Code(status))
            .map_err(|_| protocol::ProcessError::Backend)?;
        self.manager.pending_exit = Some(id);
        Ok(())
    }
}
