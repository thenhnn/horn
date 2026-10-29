#![no_std]
#![no_main]
extern crate alloc;
mod boot;
mod catalog;
mod namespace;
use alloc::vec::Vec;
use horn_ipc::*;
use horn_supervisor::{
    client::{Bootstrap, Io, copy, mint},
    protocol::*,
    *,
};
const SLOTS: TaskSlots = TaskSlots::new(SUPERVISOR_CAPS_FIRST_FREE_SLOT, SUPERVISOR_CAPS_SIZE_BITS);
const BOOT: Bootstrap = Bootstrap {
    cnode: SUPERVISOR_CAPS_SELF_SLOT,
    bits: SUPERVISOR_CAPS_SIZE_BITS,
    vm: SUPERVISOR_CAPS_VM_SLOT,
    space: SUPERVISOR_CAPS_ADDRESS_SPACE_SLOT,
};
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{cnode:SUPERVISOR_CAPS_SELF_SLOT,factory:SUPERVISOR_CAPS_VM_SLOT,address_space:SUPERVISOR_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS});
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    BOOT.cap(slot)
}
fn root() -> sel4::cap::CNode {
    sel4::cap::CNode::from_bits(SUPERVISOR_CAPS_SELF_SLOT as _)
}
fn now() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}
fn owner_badge(i: usize) -> u64 {
    1024 + i as u64
}
fn fault_badge(i: usize) -> u64 {
    2048 + i as u64
}
fn client_badge(i: usize) -> u64 {
    3072 + i as u64
}
fn ready_bit(i: usize) -> u64 {
    1 << (16 + 2 * i)
}
fn stopped_bit(i: usize) -> u64 {
    1 << (17 + 2 * i)
}
struct Job {
    process: Cap<ProcessObjectTag>,
    // Keep the owner capability's derivation anchor for the instance lifetime.
    _authority: Cap<Endpoint>,
    lifecycle: Lifecycle,
    owner: Option<Cap<Notification>>,
    stop: Option<Cap<Notification>>,
    deadline: u64,
    process_exported: bool,
    cooperative: bool,
    internal: bool,
    dependencies: Vec<u64>,
    publications: usize,
}
struct Manager {
    io: Io,
    jobs: Vec<Job>,
    ticks: u64,
    namespace: namespace::Namespace,
    launched: [bool; 8],
    wait_logged: [bool; 8],
}
impl Manager {
    fn process(&mut self, i: usize) -> ProcessControlClient<&mut Io> {
        ProcessControlClient::new(&mut self.io, Cap::from_ref(self.jobs[i].process.as_ref()))
    }
    fn stopped(&mut self, i: usize) {
        if matches!(
            self.jobs[i].lifecycle.state,
            ServiceStatus::AwaitingCleanup | ServiceStatus::Exited
        ) {
            return;
        }
        let info = self.process(i).info().unwrap();
        if info.state != ProcessState::Stopping {
            self.process(i)
                .suspend()
                .expect("stop execution, retain VM");
        }
        self.jobs[i].lifecycle.cpu_stopped();
        self.namespace.remove(&mut self.io, (i + 1) as u64);
        // Stop bound consumers before the external owner may reclaim hardware.
        // Namespace discovery alone does not establish a lifetime dependency.
        for dependent in 0..self.jobs.len() {
            if self.jobs[dependent]
                .dependencies
                .contains(&((i + 1) as u64))
            {
                self.stopped(dependent);
            }
        }
        if self.jobs[i].internal {
            Caller {
                manager: self,
                badge: owner_badge(i),
            }
            .cleanup_done()
            .unwrap();
            return;
        }
        sel4::debug_println!("SUPERVISOR_CPU_STOPPED job={} cleanup=pending", i);
        if let Some(note) = self.jobs[i].owner {
            self.io
                .space
                .cap::<sel4::cap_type::Notification>(note.as_ref())
                .unwrap()
                .signal();
        }
    }
    fn update(&mut self, event: u64) {
        for i in 0..self.jobs.len() {
            if event & ready_bit(i) != 0 && self.jobs[i].lifecycle.ready() {
                self.namespace.tree.ready((i + 1) as u64);
                sel4::debug_println!("SUPERVISOR_SERVICE_READY job={}", i);
                if let Some(note) = self.jobs[i].owner {
                    self.io
                        .space
                        .cap::<sel4::cap_type::Notification>(note.as_ref())
                        .unwrap()
                        .signal();
                }
            }
            if event == fault_badge(i) {
                self.stopped(i);
            }
            if event & CHILD_EXIT != 0
                && !matches!(
                    self.jobs[i].lifecycle.state,
                    ServiceStatus::Created | ServiceStatus::AwaitingCleanup | ServiceStatus::Exited
                )
            {
                if self.process(i).info().unwrap().state != ProcessState::Active {
                    self.stopped(i);
                }
            }
            if event & stopped_bit(i) != 0
                && self.jobs[i].lifecycle.state == ServiceStatus::Stopping
            {
                self.jobs[i].cooperative = true;
                self.stopped(i);
            }
            if let Some(mode) = self.jobs[i].lifecycle.take_stop() {
                if mode == StopMode::Graceful && self.jobs[i].stop.is_some() {
                    let note = self.jobs[i].stop.unwrap();
                    self.io
                        .space
                        .cap::<sel4::cap_type::Notification>(note.as_ref())
                        .unwrap()
                        .signal();
                    self.jobs[i].lifecycle.stopping();
                    self.jobs[i].deadline = now().wrapping_add(self.ticks * 1_000_000);
                } else {
                    self.stopped(i);
                }
            }
            if matches!(
                self.jobs[i].lifecycle.state,
                ServiceStatus::Starting | ServiceStatus::Stopping
            ) && now().wrapping_sub(self.jobs[i].deadline) < (1 << 63)
            {
                sel4::debug_println!(
                    "SUPERVISOR_TIMEOUT job={} state={:?}",
                    i,
                    self.jobs[i].lifecycle.state
                );
                self.stopped(i);
            }
        }
    }
    fn polling(&self) -> bool {
        self.jobs.iter().any(|j| {
            matches!(
                j.lifecycle.state,
                ServiceStatus::Starting | ServiceStatus::Stopping
            )
        })
    }
}
struct Caller<'a> {
    manager: &'a mut Manager,
    badge: u64,
}
impl Caller<'_> {
    fn owner(&self) -> Result<usize, SupervisorError> {
        self.manager
            .jobs
            .iter()
            .enumerate()
            .find(|(i, _)| owner_badge(*i) == self.badge)
            .map(|(i, _)| i)
            .ok_or(SupervisorError::Denied)
    }
    fn runtime(&self) -> Result<usize, SupervisorError> {
        self.manager
            .jobs
            .iter()
            .enumerate()
            .find(|(i, _)| owner_badge(*i) == self.badge || client_badge(*i) == self.badge)
            .map(|(i, _)| i)
            .ok_or(SupervisorError::Denied)
    }
    fn retain(&mut self, cap: Cap<Notification>) -> Result<Cap<Notification>, SupervisorError> {
        if cap.as_ref().unwrapped_badge().is_some() {
            return Err(SupervisorError::Denied);
        }
        Ok(copy(&mut self.manager.io, root(), cap))
    }
}
impl SupervisorFactory for Caller<'_> {
    fn prepare(
        &mut self,
        elf: Cap<VmMemoryObjectTag>,
        request: ProcessLaunch,
        bootstrap: ProcessBootstrap,
    ) -> Result<Cap<SupervisedJobTag>, SupervisorError> {
        if self.badge != FACTORY_BADGE {
            return Err(SupervisorError::Denied);
        }
        if self.manager.jobs.len() >= MAX_JOBS {
            return Err(SupervisorError::Exhausted);
        }
        self.manager
            .jobs
            .try_reserve(1)
            .expect("bounded job metadata");
        let process =
            ProcessFactoryClient::new(&mut self.manager.io, cap(SUPERVISOR_CAPS_FACTORY_SLOT))
                .prepare(&elf, request, bootstrap)
                .map_err(|e| match e {
                    CallError::Remote(ProcessError::Exhausted | ProcessError::QuotaExceeded) => {
                        SupervisorError::Exhausted
                    }
                    CallError::Remote(
                        ProcessError::Invalid | ProcessError::InvalidElf | ProcessError::InvalidAbi,
                    ) => SupervisorError::Invalid,
                    CallError::Remote(ProcessError::Denied) => SupervisorError::Denied,
                    _ => SupervisorError::Backend,
                })?;
        let i = self.manager.jobs.len();
        let fault = mint(
            &mut self.manager.io,
            root(),
            cap::<Endpoint>(SUPERVISOR_CAPS_SERVICE_SLOT),
            fault_badge(i),
            Rights::ALL,
        );
        ProcessControlClient::new(&mut self.manager.io, Cap::from_ref(process.as_ref()))
            .fault_handler(
                &cap(SUPERVISOR_CAPS_SELF_SLOT),
                SUPERVISOR_CAPS_SIZE_BITS as _,
                fault.as_ref().slot,
            )
            .unwrap();
        self.manager.io.space.delete(fault.as_ref()).unwrap();
        let authority = mint(
            &mut self.manager.io,
            root(),
            cap::<Endpoint>(SUPERVISOR_CAPS_SERVICE_SLOT),
            owner_badge(i),
            Rights::ALL,
        );
        let exported = copy(&mut self.manager.io, root(), authority);
        self.manager.jobs.push(Job {
            process,
            _authority: authority,
            lifecycle: Lifecycle::new(),
            owner: None,
            stop: None,
            deadline: 0,
            process_exported: false,
            cooperative: false,
            internal: false,
            dependencies: Vec::new(),
            publications: 0,
        });
        Ok(Cap::from_ref(exported.as_ref()))
    }
}
impl SupervisorJob for Caller<'_> {
    fn publish(
        &mut self,
        path: List<u8, 96>,
        endpoint: Cap<Endpoint>,
    ) -> Result<(), SupervisorError> {
        let i = self.owner()?;
        if self.manager.jobs[i].lifecycle.state != ServiceStatus::Created
            || self.manager.jobs[i].publications >= 4
        {
            return Err(SupervisorError::Busy);
        }
        if endpoint.as_ref().unwrapped_badge().is_some() {
            return Err(SupervisorError::Denied);
        }
        let path = core::str::from_utf8(&path.data[..path.len as usize])
            .map_err(|_| SupervisorError::Invalid)?;
        if !catalog::core().publish.iter().any(|prefix| {
            path.strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            return Err(SupervisorError::Denied);
        }
        let retained = copy(&mut self.manager.io, root(), endpoint);
        if self
            .manager
            .namespace
            .tree
            .declare(path, (i + 1) as u64, retained)
            .is_err()
        {
            self.manager.io.space.delete(retained.as_ref()).unwrap();
            return Err(SupervisorError::Invalid);
        }
        self.manager.jobs[i].publications += 1;
        Ok(())
    }
    fn client(&mut self) -> Result<Cap<SupervisedClientTag>, SupervisorError> {
        let i = self.owner()?;
        let cap = mint(
            &mut self.manager.io,
            root(),
            cap::<Endpoint>(SUPERVISOR_CAPS_SERVICE_SLOT),
            client_badge(i),
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        Ok(Cap::from_ref(cap.as_ref()))
    }
    fn process(&mut self) -> Result<Cap<ProcessObjectTag>, SupervisorError> {
        let i = self.owner()?;
        if self.manager.jobs[i].process_exported {
            return Err(SupervisorError::Busy);
        }
        self.manager.jobs[i].process_exported = true;
        Ok(copy(
            &mut self.manager.io,
            root(),
            self.manager.jobs[i].process,
        ))
    }
    fn signal(&mut self, kind: ServiceSignal) -> Result<Cap<Notification>, SupervisorError> {
        let i = self.owner()?;
        if self.manager.jobs[i].lifecycle.state != ServiceStatus::Created {
            return Err(SupervisorError::Busy);
        }
        let bit = match kind {
            ServiceSignal::Ready => ready_bit(i),
            ServiceSignal::Stopped => stopped_bit(i),
        };
        Ok(mint(
            &mut self.manager.io,
            root(),
            cap(SUPERVISOR_CAPS_EVENTS_SLOT),
            bit,
            Rights::WRITE,
        ))
    }
    fn watch(&mut self, owner: Cap<Notification>) -> Result<(), SupervisorError> {
        let i = self.owner()?;
        if self.manager.jobs[i].owner.is_some()
            || self.manager.jobs[i].lifecycle.state != ServiceStatus::Created
        {
            return Err(SupervisorError::Busy);
        }
        self.manager.jobs[i].owner = Some(self.retain(owner)?);
        Ok(())
    }
    fn stop_signal(&mut self, stop: Cap<Notification>) -> Result<(), SupervisorError> {
        let i = self.owner()?;
        if self.manager.jobs[i].stop.is_some()
            || self.manager.jobs[i].lifecycle.state != ServiceStatus::Created
        {
            return Err(SupervisorError::Busy);
        }
        self.manager.jobs[i].stop = Some(self.retain(stop)?);
        Ok(())
    }
    fn start(&mut self) -> Result<(), SupervisorError> {
        let i = self.owner()?;
        if self.manager.jobs[i].owner.is_none() {
            return Err(SupervisorError::Denied);
        }
        self.manager.jobs[i].lifecycle.start()?;
        self.manager.jobs[i].deadline = now().wrapping_add(self.manager.ticks * 30_000_000);
        self.manager.process(i).start().expect("start process");
        Ok(())
    }
    fn status(&mut self) -> Result<ServiceStatus, SupervisorError> {
        let i = self.owner()?;
        Ok(self.manager.jobs[i].lifecycle.state)
    }
    fn request_stop(&mut self, mode: StopMode) -> Result<(), SupervisorError> {
        let i = self.owner()?;
        self.manager.jobs[i].lifecycle.request_stop(mode)
    }
    fn cooperative(&mut self) -> Result<bool, SupervisorError> {
        let i = self.owner()?;
        Ok(self.manager.jobs[i].cooperative)
    }
    fn cleanup_done(&mut self) -> Result<(), SupervisorError> {
        let i = self.owner()?;
        if !self.manager.jobs[i].lifecycle.can_release() {
            return Err(SupervisorError::Busy);
        }
        self.manager
            .process(i)
            .terminate()
            .expect("owner ACK before backing release");
        #[cfg(feature = "boot-tests")]
        {
            let info = self.manager.process(i).info().unwrap();
            assert_eq!(info.state, ProcessState::Exited);
            assert_eq!(info.ram.charged_bytes, 0);
            assert_eq!(info.ram.quarantined_bytes, 0);
        }
        self.manager.process(i).close().unwrap();
        self.manager
            .io
            .space
            .delete(self.manager.jobs[i].process.as_ref())
            .unwrap();
        if let Some(note) = self.manager.jobs[i].stop.take() {
            self.manager.io.space.delete(note.as_ref()).unwrap();
        }
        if let Some(note) = self.manager.jobs[i].owner.take() {
            self.manager.io.space.delete(note.as_ref()).unwrap();
        }
        self.manager.jobs[i].lifecycle.released();
        #[cfg(feature = "boot-tests")]
        sel4::debug_println!(
            "SUPERVISOR_RELEASE_PASS job={} ram_returned=true owner_ack=true",
            i
        );
        Ok(())
    }
}
impl SupervisorRuntime for Caller<'_> {
    fn status(&mut self) -> Result<ServiceStatus, SupervisorError> {
        let i = self.runtime()?;
        Ok(self.manager.jobs[i].lifecycle.state)
    }
    fn request_stop(&mut self, mode: StopMode) -> Result<(), SupervisorError> {
        let i = self.runtime()?;
        self.manager.jobs[i].lifecycle.request_stop(mode)?;
        sel4::debug_println!("SUPERVISOR_STOP_REQUEST job={} accepted=true", i);
        Ok(())
    }
}
fn main() -> ! {
    let mut io = horn_runtime::transport(root(), SLOTS.application()).unwrap();
    let ticks = SystemControlClient::new(&mut io, cap(SUPERVISOR_CAPS_SYSTEM_SLOT))
        .ticks_per_us()
        .unwrap();
    assert!(ticks != 0);
    let exits = mint(
        &mut io,
        root(),
        cap::<Notification>(SUPERVISOR_CAPS_EVENTS_SLOT),
        CHILD_EXIT,
        Rights::WRITE,
    );
    ProcessFactoryClient::new(&mut io, cap(SUPERVISOR_CAPS_FACTORY_SLOT))
        .watch(&exits)
        .unwrap();
    io.space.delete(exits.as_ref()).unwrap();
    sel4::debug_println!("SUPERVISOR_START policy=generic cleanup=owner-ack");
    let namespace = namespace::Namespace::new(&mut io);
    let mut manager = Manager {
        io,
        jobs: Vec::new(),
        ticks,
        namespace,
        launched: [false; 8],
        wait_logged: [false; 8],
    };
    boot::start(&mut manager);
    let endpoint = sel4::cap::Endpoint::from_bits(SUPERVISOR_CAPS_SERVICE_SLOT as _);
    let reply = sel4::cap::Reply::from_bits(SUPERVISOR_CAPS_REPLY_SLOT as _);
    let receive = root().absolute_cptr(sel4::CPtr::from_bits(SLOTS.public_receive() as _));
    loop {
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive));
        let (info, badge) = if manager.polling() {
            endpoint.nb_recv(reply)
        } else {
            endpoint.recv(reply)
        };
        if let Some((i, _)) = manager
            .jobs
            .iter()
            .enumerate()
            .find(|(i, _)| badge == fault_badge(*i))
        {
            let fault = sel4::with_ipc_buffer(|ipc| sel4::Fault::new(ipc, &info));
            sel4::debug_println!("SUPERVISOR_FAULT job={} fault={:?}", i, fault);
        }
        if badge & CORE_READY != 0 {
            manager.namespace.tree.ready(0);
            SystemControlClient::new(&mut manager.io, cap(SUPERVISOR_CAPS_SYSTEM_SLOT))
                .ready()
                .unwrap();
        }
        if manager.namespace.recognizes(badge) {
            let request = {
                let mut server = Sel4Server::new(root(), endpoint, receive, &mut manager.io.space);
                server.decode_received_dynamic(
                    info,
                    badge,
                    reply,
                    horn_registry::protocol::registry_node_request_layout,
                    horn_registry::protocol::RegistryNodeRequest::decode,
                )
            };
            if let Ok((_, request, reply)) = request {
                let response = horn_registry::protocol::registry_node_dispatch(
                    request,
                    &mut namespace::DirectoryCaller {
                        manager: &mut manager,
                        badge,
                    },
                );
                horn_registry::protocol::registry_node_reply(
                    reply,
                    &mut manager.io.space,
                    &response,
                )
                .unwrap();
            }
        } else if badge == FACTORY_BADGE
            || manager
                .jobs
                .iter()
                .enumerate()
                .any(|(i, _)| badge == owner_badge(i) || badge == client_badge(i))
        {
            let request = {
                let mut server = Sel4Server::new(root(), endpoint, receive, &mut manager.io.space);
                server.decode_received_dynamic(
                    info,
                    badge,
                    reply,
                    supervisor_host_request_layout,
                    SupervisorHostRequest::decode,
                )
            };
            if let Ok((_, request, reply)) = request {
                let response = supervisor_host_dispatch(
                    request,
                    &mut Caller {
                        manager: &mut manager,
                        badge,
                    },
                );
                supervisor_host_reply(reply, &mut manager.io.space, &response).unwrap();
            }
        }
        receive.delete().unwrap();
        manager.update(badge);
        manager.activate();
        manager.namespace.changed(&mut manager.io);
        if manager.polling() {
            sel4::r#yield();
        }
    }
}
