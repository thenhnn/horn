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
mod loader;
mod service;
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
    state: State,
    exit: Option<Exit>,
    allocator: KernelAllocator,
    cnode: sel4::cap::CNode,
    tcb: sel4::cap::Tcb,
    loaded: loader::Loaded,
    entry: usize,
    lifecycle: Option<(usize, sel4::Word)>,
}
pub struct Info<'a> {
    pub id: ProcessId,
    pub name: &'a str,
    pub state: State,
    pub exit: Option<Exit>,
    pub ram: Usage,
}

pub struct Manager {
    allocator: KernelAllocator,
    caller_vspace: sel4::cap::VSpace,
    scratch: usize,
    asid_pool: sel4::cap::AsidPool,
    scheduler: sel4::cap::SchedControl,
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
    pub unsafe fn new(
        allocator: KernelAllocator,
        caller_vspace: sel4::cap::VSpace,
        scratch: usize,
        asid_pool: sel4::cap::AsidPool,
        scheduler: sel4::cap::SchedControl,
    ) -> Self {
        Self {
            allocator,
            caller_vspace,
            scratch,
            asid_pool,
            scheduler,
            processes: Vec::new(),
            pending_exit: None,
            next_id: 1,
            launch_window: None,
            launch_poisoned: false,
            outgoing: None,
        }
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
        let result = (|| {
            let loaded = loader::load(
                &mut allocator,
                &image,
                self.caller_vspace,
                self.scratch,
                self.asid_pool,
                config.vm_window,
            )?;
            let cnode =
                allocator.allocate_variable_sized::<sel4::cap_type::CNode>(config.cnode_bits)?;
            let tcb = allocator.allocate_fixed_sized::<sel4::cap_type::Tcb>()?;
            let sc = allocator.allocate_variable_sized::<sel4::cap_type::SchedContext>(
                sel4::sys::seL4_MinSchedContextBits as usize,
            )?;
            self.scheduler.sched_control_configure_flags(
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
            Ok::<_, Error>((loaded, cnode, tcb))
        })();
        let (loaded, cnode, tcb) = match result {
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
            state: State::Created,
            exit: None,
            allocator,
            cnode,
            tcb,
            loaded,
            entry: image.entry,
            lifecycle: None,
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
                value.checked_add(1)
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
