//! Trusted bootstrap and VM/IRQ mechanics. Service policy runs in supervisor.
#![no_std]
#![no_main]
#![feature(const_index)]
#![feature(const_trait_impl)]
#![allow(incomplete_features)]
extern crate alloc;
mod platform;
use horn_devmgr::protocol::*;
use horn_ipc::*;
use horn_procd::{Config, LaunchPolicy, Manager};
use horn_supervisor::protocol::*;
use horn_vm::sel4::KernelAllocator;
use sel4_root_task::{Never, root_task};
static ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/supervisor.elf"));
const PLATFORM_BADGE: u64 = 1 << 48;
const SYSTEM_BADGE: u64 = PLATFORM_BADGE + 1;
const FAULT_BADGE: u64 = PLATFORM_BADGE + 2;
const CRITICAL_BADGE: u64 = PLATFORM_BADGE + 3;

enum Request {
    Platform(DevicePlatformObjectRequest),
    Vm(horn_vm::protocol::VmHostRequest),
    Process(horn_procd::protocol::ProcessHostRequest),
    System(SystemHostRequest),
}
fn layout(label: u64) -> Result<&'static CapLayout, RpcError> {
    device_platform_object_request_layout(label)
        .or_else(|_| horn_vm::protocol::vm_host_request_layout(label))
        .or_else(|_| horn_procd::protocol::process_host_request_layout(label))
        .or_else(|_| system_host_request_layout(label))
}
fn decode(label: u64, input: &mut Decoder<'_>) -> Result<Request, RpcError> {
    if device_platform_object_request_layout(label).is_ok() {
        Ok(Request::Platform(DevicePlatformObjectRequest::decode(
            label, input,
        )?))
    } else if horn_vm::protocol::vm_host_request_layout(label).is_ok() {
        Ok(Request::Vm(horn_vm::protocol::VmHostRequest::decode(
            label, input,
        )?))
    } else if system_host_request_layout(label).is_ok() {
        Ok(Request::System(SystemHostRequest::decode(label, input)?))
    } else {
        Ok(Request::Process(
            horn_procd::protocol::ProcessHostRequest::decode(label, input)?,
        ))
    }
}
struct System<'a> {
    platform: &'a mut platform::Platform,
    reset: usize,
}
impl SystemControl for System<'_> {
    fn ticks_per_us(&mut self) -> Result<u64, SystemError> {
        Ok(self.platform.ticks_per_us())
    }
    fn ready(&mut self) -> Result<(), SystemError> {
        self.platform.ready = true;
        sel4::debug_println!("SUPERVISOR_READY policy=userspace");
        Ok(())
    }
    fn fail(&mut self, _: SystemFailure) -> Result<(), SystemError> {
        fatal(self.reset, "devmgr stopped or faulted")
    }
}
fn serve(
    platform: &mut platform::Platform,
    server: &mut Sel4Server<Sel4CapSpace>,
    reply: sel4::cap::Reply,
    reset: usize,
) {
    sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&server.recv_slot));
    let (info, badge) = if platform.ready {
        server.endpoint.recv(reply)
    } else {
        server.endpoint.nb_recv(reply)
    };
    if badge == 0 {
        server.recv_slot.delete().unwrap();
        sel4::r#yield();
        return;
    }
    if platform
        .manager
        .execution_fault(&info, badge)
        .expect("hosted execution fault")
    {
        server.recv_slot.delete().unwrap();
        return;
    }
    if badge == FAULT_BADGE {
        fatal(reset, "supervisor stopped or faulted");
    }
    if badge == CRITICAL_BADGE {
        fatal(reset, "critical service stopped or faulted");
    }
    let Ok((_, request, reply)) =
        server.decode_received_dynamic(info, badge, reply, layout, decode)
    else {
        server.recv_slot.delete().unwrap();
        if badge == PLATFORM_BADGE || badge == SYSTEM_BADGE {
            fatal(reset, "bootstrap protocol");
        }
        return;
    };
    match request {
        Request::Platform(request) if badge == PLATFORM_BADGE => {
            let response = device_platform_object_dispatch(request, platform);
            device_platform_object_reply(reply, &mut server.space, &response).unwrap();
        }
        Request::System(request) if badge == SYSTEM_BADGE => {
            let response = system_host_dispatch(request, &mut System { platform, reset });
            system_host_reply(reply, &mut server.space, &response).unwrap();
        }
        Request::Vm(request) => {
            if let Some(response) = platform.manager.dispatch_vm(request, badge) {
                horn_vm::protocol::vm_host_reply(reply, &mut server.space, &response).unwrap();
            }
        }
        Request::Process(request) => {
            let response = platform.manager.dispatch(request, badge);
            if let Some(id) = platform.manager.take_exit() {
                if !platform.manager.notify_parent(id) {
                    fatal(reset, "supervisor stopped or faulted");
                }
                // Self-exit stops the caller; do not reply or reclaim pinned VM.
            } else {
                horn_procd::protocol::process_host_reply(reply, &mut server.space, &response)
                    .unwrap();
            }
        }
        _ => {}
    }
    platform.manager.finish_reply(server).unwrap();
}
const ROOT_HEAP_BYTES: usize = if cfg!(large_images) {
    64 * 1024 * 1024
} else {
    8 * 1024 * 1024
};
// Root cannot use its own VM RPC allocator. Keep bounded space for resident
// frame/tree metadata, including transient Vec growth as the TCP pool expands.
#[root_task(stack_size = 256 * 1024, heap_size = ROOT_HEAP_BYTES)]
fn main(boot: &sel4::BootInfoPtr) -> sel4::Result<Never> {
    let root = sel4::init_thread::slot::CNODE.cap();
    let empty = boot.empty().range();
    // Only fixed device-discovery scratch slots and the public receive slot
    // are excluded. Kernel objects and IPC imports share everything else.
    let discovery_start = empty.start;
    let slots =
        horn_ipc::root_slots::RootSlots::new(discovery_start + platform::TEMP_SLOTS + 1..empty.end);
    assert!(
        !cfg!(large_images) || empty.end >= 1 << 20,
        "large firmware images require KernelRootCNodeSizeBits >= 20; rebuild kernel AND root"
    );
    let mut sources = boot
        .untyped_list()
        .iter()
        .enumerate()
        .filter(|(_, d)| !d.is_device());
    let (i, d) = sources.next().unwrap();
    let mut allocator = KernelAllocator::with_slots(
        root,
        boot.untyped().index(i).cap(),
        d.size_bits(),
        slots.clone(),
    );
    for (i, d) in sources {
        allocator.add_untyped(boot.untyped().index(i).cap(), d.size_bits());
    }
    let scratch = scratch(boot);
    let mut manager = unsafe {
        Manager::new(
            allocator.clone(),
            sel4::init_thread::slot::VSPACE.cap(),
            scratch,
            sel4::init_thread::slot::ASID_POOL.cap(),
            (0..boot.inner().numNodes as usize)
                .map(|cpu| boot.sched_control().index(cpu).cap())
                .collect(),
            boot.inner().nodeID as usize,
        )
    };
    unsafe {
        manager
            .enable_launch(
                0xc000_0000..if cfg!(large_images) {
                    0xc800_0000
                } else {
                    0xc200_0000
                },
            )
            .unwrap();
    }
    // Only the initial supervisor is bootstrapped here. No device/driver ELF,
    // matching, child budgets or service launch ordering are selected by root.
    let process = manager
        .create(
            ELF,
            Config {
                name: "supervisor",
                ram_limit: if cfg!(large_images) {
                    256 * 1024 * 1024
                } else {
                    128 * 1024 * 1024
                },
                cnode_bits: SUPERVISOR_CAPS_SIZE_BITS as _,
                vm_window: 0x4000_0000..if cfg!(large_images) {
                    0x5000_0000
                } else {
                    0x4800_0000
                },
                cpu_budget_us: 1000,
                cpu_period_us: 1000,
            },
        )
        .unwrap();
    let mut child_allocator = manager.allocator(process).unwrap();
    let service = child_allocator
        .allocate_fixed_sized::<sel4::cap_type::Endpoint>()
        .unwrap();
    let events = child_allocator
        .allocate_fixed_sized::<sel4::cap_type::Notification>()
        .unwrap();
    manager
        .tcb(process)
        .unwrap()
        .tcb_bind_notification(events)
        .unwrap();
    let child_reply = child_allocator
        .allocate(sel4::ObjectBlueprint::Reply)
        .unwrap();
    let bootstrap = allocator
        .allocate_fixed_sized::<sel4::cap_type::Endpoint>()
        .unwrap();
    let reply = allocator
        .allocate(sel4::ObjectBlueprint::Reply)
        .unwrap()
        .cast::<sel4::cap_type::Reply>();
    let mint = |allocator: &mut KernelAllocator, badge| {
        let slot = allocator.reserve_slot().unwrap();
        root.absolute_cptr(sel4::CPtr::from_bits(slot as _))
            .mint(
                &root.absolute_cptr(bootstrap.cptr()),
                sel4::CapRights::all(),
                badge,
            )
            .unwrap();
        slot
    };
    let platform_slot = mint(&mut allocator, PLATFORM_BADGE);
    let system_slot = mint(&mut allocator, SYSTEM_BADGE);
    let fault_slot = mint(&mut allocator, FAULT_BADGE);
    let critical_slot = mint(&mut allocator, CRITICAL_BADGE);
    let critical_note = allocator
        .allocate_fixed_sized::<sel4::cap_type::Notification>()
        .unwrap();
    sel4::init_thread::slot::TCB
        .cap()
        .tcb_bind_notification(critical_note)
        .unwrap();
    let critical_exit = allocator.reserve_slot().unwrap();
    root.absolute_cptr(sel4::CPtr::from_bits(critical_exit as _))
        .mint(
            &root.absolute_cptr(critical_note.cptr()),
            sel4::CapRights::write_only(),
            CRITICAL_BADGE,
        )
        .unwrap();
    let vm = horn_vm::service::Service::new(
        child_allocator.clone(),
        horn_vm::sel4::FramePool::for_account(child_allocator),
        bootstrap,
        manager.vspace(process).unwrap(),
        0x4000_0000..0x4800_0000,
        128,
    )
    .unwrap();
    let vm_factory = vm.factory();
    let vm_space = vm.address_space();
    manager.attach_vm(process, vm).unwrap();
    let lifecycle = manager.provision_self(process, bootstrap).unwrap();
    let (factory, observer) = manager
        .provision_launch(
            process,
            bootstrap,
            LaunchPolicy {
                max_children: 8, // Matches the supervisor's bounded job table.
                max_ram: if cfg!(large_images) {
                    256 * 1024 * 1024
                } else {
                    64 * 1024 * 1024
                },
                vm_window: 0x4000_0000..if cfg!(large_images) {
                    0x5000_0000
                } else {
                    0x4400_0000
                },
                cpu_budget_us: 1000,
                cpu_period_us: 1000,
            },
        )
        .unwrap();
    let child = manager.cnode(process).unwrap();
    for (slot, source) in [
        (SUPERVISOR_CAPS_SELF_SLOT, child.bits()),
        (SUPERVISOR_CAPS_VM_SLOT, vm_factory.bits()),
        (SUPERVISOR_CAPS_ADDRESS_SPACE_SLOT, vm_space.bits()),
        (SUPERVISOR_CAPS_LIFECYCLE_SLOT, lifecycle.bits()),
        (SUPERVISOR_CAPS_REPLY_SLOT, child_reply.bits()),
        (SUPERVISOR_CAPS_FACTORY_SLOT, factory.bits()),
        (SUPERVISOR_CAPS_PROCESSES_SLOT, observer.bits()),
        (SUPERVISOR_CAPS_PLATFORM_SLOT, platform_slot as _),
        (SUPERVISOR_CAPS_SYSTEM_SLOT, system_slot as _),
        (SUPERVISOR_CAPS_SERVICE_SLOT, service.bits()),
        (SUPERVISOR_CAPS_EVENTS_SLOT, events.bits()),
        (SUPERVISOR_CAPS_CRITICAL_FAULT_SLOT, critical_slot as _),
        (SUPERVISOR_CAPS_CRITICAL_EXIT_SLOT, critical_exit as _),
    ] {
        manager
            .install_initial_capability(
                process,
                slot,
                root.absolute_cptr(sel4::CPtr::from_bits(source)),
                sel4::CapRights::all(),
            )
            .unwrap();
    }
    let mut bindings = [
        ("horn.vm", SUPERVISOR_CAPS_VM_SLOT),
        ("horn.address-space", SUPERVISOR_CAPS_ADDRESS_SPACE_SLOT),
        ("horn.lifecycle", SUPERVISOR_CAPS_LIFECYCLE_SLOT),
        ("horn.reply", SUPERVISOR_CAPS_REPLY_SLOT),
        ("supervisor.factory", SUPERVISOR_CAPS_FACTORY_SLOT),
        ("supervisor.processes", SUPERVISOR_CAPS_PROCESSES_SLOT),
        ("supervisor.platform", SUPERVISOR_CAPS_PLATFORM_SLOT),
        ("supervisor.system", SUPERVISOR_CAPS_SYSTEM_SLOT),
        ("supervisor.service", SUPERVISOR_CAPS_SERVICE_SLOT),
        ("supervisor.events", SUPERVISOR_CAPS_EVENTS_SLOT),
        (
            "supervisor.critical_fault",
            SUPERVISOR_CAPS_CRITICAL_FAULT_SLOT,
        ),
        (
            "supervisor.critical_exit",
            SUPERVISOR_CAPS_CRITICAL_EXIT_SLOT,
        ),
    ]
    .map(|(name, selector)| libhorn::startup::Entry {
        name,
        selector: selector as u64,
    });
    bindings.sort_unstable_by_key(|entry| entry.name);
    let size = 8 + bindings.iter().map(|e| 16 + e.name.len()).sum::<usize>();
    let mut descriptor = alloc::vec![0; size];
    libhorn::bindings::encode(&bindings, &mut descriptor).unwrap();
    manager.startup_context_bytes(process, &descriptor).unwrap();
    manager
        .tcb(process)
        .unwrap()
        .tcb_set_space(
            sel4::CPtr::from_bits(fault_slot as _),
            child,
            sel4::CNodeCapData::new(0, sel4::WORD_SIZE - SUPERVISOR_CAPS_SIZE_BITS as usize),
            manager.vspace(process).unwrap(),
        )
        .unwrap();
    let reset = allocator.reserve_slot().unwrap();
    sel4::init_thread::slot::IO_PORT_CONTROL
        .cap()
        .ioport_control_issue(
            0xcf9,
            0xcf9,
            &root.absolute_cptr(sel4::CPtr::from_bits(reset as _)),
        )
        .unwrap();
    let frequency = boot
        .extra()
        .find(|e| e.id == sel4::BootInfoExtraId::X86TscFreq)
        .unwrap();
    let mhz = u32::from_le_bytes(frequency.content()[..4].try_into().unwrap()) as u64;
    let mut platform = unsafe {
        platform::Platform::new(
            boot,
            allocator,
            scratch,
            discovery_start,
            manager,
            option_env!("DEVMGR_TEST_VTD") == Some("1"),
            mhz,
        )
    };
    let receive = root.absolute_cptr(sel4::CPtr::from_bits(
        (discovery_start + platform::TEMP_SLOTS) as _,
    ));
    let space = Sel4CapSpace::new_shared_local(root, slots);
    let mut server = Sel4Server::new(root, bootstrap, receive, space);
    let root_tcb = sel4::init_thread::slot::TCB.cap();
    root_tcb.tcb_set_priority(root_tcb, 0).unwrap();
    let started = unsafe { core::arch::x86_64::_rdtsc() };
    let timeout = if cfg!(feature = "boot-tests") && option_env!("DEVMGR_TEST_STALL") == Some("1") {
        100_000
    } else {
        option_env!("ROOT_STARTUP_TIMEOUT_US")
            .unwrap_or("30000000")
            .parse::<u64>()
            .unwrap()
    };
    platform.manager.start(process).unwrap();
    loop {
        if !platform.ready
            && unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(started) > mhz * timeout
        {
            fatal(reset, "startup timeout");
        }
        serve(&mut platform, &mut server, reply, reset);
    }
}
fn fatal(reset: usize, reason: &str) -> ! {
    sel4::debug_println!("DEVMGR_FATAL reboot=true reason={}", reason);
    sel4::with_ipc_buffer_mut(|ipc| {
        let _ = ipc
            .inner_mut()
            .seL4_X86_IOPort_Out8(reset as _, 0xcf9, 0x0e);
    });
    loop {
        core::hint::spin_loop();
    }
}
#[repr(C, align(4096))]
struct Scratch([u8; 4096]);
static mut SCRATCH: Scratch = Scratch([0; 4096]);
fn scratch(boot: &sel4::BootInfo) -> usize {
    unsafe extern "C" {
        static __executable_start: usize;
    }
    let address = core::ptr::addr_of!(SCRATCH) as usize;
    let start = core::ptr::addr_of!(__executable_start) as usize;
    boot.user_image_frames()
        .index((address - start) / 4096)
        .cap()
        .frame_unmap()
        .unwrap();
    address
}
