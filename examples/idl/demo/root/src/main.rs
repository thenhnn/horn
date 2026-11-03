#![no_std]
#![no_main]
#![feature(const_index)]
#![feature(const_trait_impl)]
#![allow(incomplete_features)]
extern crate alloc;

use core::ptr;
use horn_ipc::*;
use horn_procd::protocol as process;
use horn_procd::{Config, Exit, LaunchPolicy, Manager};
use horn_vm::protocol as vm;
use horn_vm::{
    sel4::{FramePool, KernelAllocator},
    service::Service as VmService,
};
use sel4_root_task::{Never, root_task};
mod memory_probe;
mod smp_probe;
#[allow(dead_code, unused_imports, non_camel_case_types)]
mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
use protocol::*;

static CHILD_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/child.elf"));
static LAUNCH_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/launched.elf"));
const GRANULE_SIZE: usize = ::sel4::FrameObjectType::GRANULE.bytes();
const VM_START: usize = 0x4000_0000;
const VM_SIZE: usize = 64 * 1024 * 1024;
const RAM_LIMIT: usize = 32 * 1024 * 1024;

fn install_context(
    manager: &mut Manager,
    id: horn_procd::ProcessId,
    entries: &mut [libhorn::startup::Entry<'_>],
) {
    entries.sort_unstable_by_key(|e| e.name);
    let size = 8 + entries.iter().map(|e| 16 + e.name.len()).sum::<usize>();
    let mut bytes = alloc::vec![0; size];
    libhorn::bindings::encode(entries, &mut bytes).unwrap();
    manager.startup_context_bytes(id, &bytes).unwrap();
}
fn config(name: &str) -> Config<'_> {
    Config {
        name,
        ram_limit: RAM_LIMIT,
        cnode_bits: CHILD_CAPS_SIZE_BITS as usize,
        vm_window: VM_START..VM_START + VM_SIZE,
        cpu_budget_us: 1000,
        cpu_period_us: 1000,
    }
}
enum Request {
    Vm(vm::VmHostRequest),
    Process(process::ProcessHostRequest),
}

#[root_task(stack_size = 1024 * 256, heap_size = 1024 * 1024 * 4)]
fn main(bootinfo: &::sel4::BootInfoPtr) -> ::sel4::Result<Never> {
    ::sel4::debug_println!("idl demo root: start");
    // Managed processes may share this CPU; yield only helps at the same priority.
    let root_tcb = ::sel4::init_thread::slot::TCB.cap();
    root_tcb.tcb_set_priority(root_tcb, 0).unwrap();
    ::sel4::debug_println!("VM_SERVICE_CPU id={}", unsafe {
        core::arch::x86_64::__cpuid(1).ebx >> 24
    });
    let empty = bootinfo.empty().range();
    let split = empty.start + empty.len() / 2;
    let root = ::sel4::init_thread::slot::CNODE.cap();
    let mut sources = bootinfo
        .untyped_list()
        .iter()
        .enumerate()
        .filter(|(_, d)| !d.is_device());
    let (index, desc) = sources.next().unwrap();
    let mut allocator = KernelAllocator::new(
        root,
        bootinfo.untyped().index(index).cap(),
        desc.size_bits(),
        empty.start..split,
    );
    for (index, desc) in sources {
        allocator.add_untyped(bootinfo.untyped().index(index).cap(), desc.size_bits());
    }
    let mut space = Sel4CapSpace::new_local(root, split as _, empty.end as _);
    let scratch = init_free_page_addr(bootinfo);
    let mut pool = FramePool::new(&mut allocator, 4).unwrap();
    memory_probe::check_backing(&mut pool, scratch);
    let counter = allocator
        .allocate_fixed_sized::<::sel4::cap_type::Endpoint>()
        .unwrap();
    let system = allocator
        .allocate_fixed_sized::<::sel4::cap_type::Endpoint>()
        .unwrap();
    let wake = allocator
        .allocate_fixed_sized::<::sel4::cap_type::Notification>()
        .unwrap();
    let system_reply = allocator
        .allocate(::sel4::ObjectBlueprint::Reply)
        .unwrap()
        .cast();
    // Scratch was unmapped above; the high reader window is outside this image.
    // Both ranges remain exclusive to the root's manager for its lifetime.
    let mut processes = unsafe {
        Manager::new(
            allocator.clone(),
            ::sel4::init_thread::slot::VSPACE.cap(),
            scratch,
            ::sel4::init_thread::slot::ASID_POOL.cap(),
            (0..bootinfo.inner().numNodes as usize)
                .map(|cpu| bootinfo.sched_control().index(cpu).cap())
                .collect(),
            bootinfo.inner().nodeID as usize,
        )
    };

    unsafe { processes.enable_launch(0x8000_0000..0x8100_0000) }.unwrap();
    let frequency = bootinfo
        .extra()
        .find(|e| e.id == ::sel4::BootInfoExtraId::X86TscFreq)
        .unwrap();
    let mhz = u32::from_le_bytes(frequency.content()[..4].try_into().unwrap()) as u64;
    space = smp_probe::run(
        &mut processes,
        &mut allocator,
        space,
        system,
        system_reply,
        scratch,
        mhz,
    );
    let system_charge = allocator.account().usage().charged;

    // Independent accounts share physical RAM, not their individual quotas.
    let peer = processes.create(CHILD_ELF, config("peer")).unwrap();
    let child = processes.create(CHILD_ELF, config("counter")).unwrap();
    let child_charge = processes.info(child).unwrap().ram.charged;
    let mut peer_allocator = processes.allocator(peer).unwrap();
    let baseline = peer_allocator.account().usage().charged;
    processes.set_limit(peer, baseline + GRANULE_SIZE).unwrap();
    let frame = peer_allocator
        .allocate_fixed_sized::<::sel4::cap_type::Granule>()
        .unwrap();
    assert_eq!(
        peer_allocator.account().usage().charged,
        baseline + GRANULE_SIZE
    );
    assert!(matches!(
        peer_allocator.allocate_fixed_sized::<::sel4::cap_type::Granule>(),
        Err(horn_vm::Error::QuotaExceeded)
    ));
    peer_allocator.release_slot(frame.bits() as usize).unwrap();
    assert_eq!(peer_allocator.account().usage().charged, baseline);
    assert_eq!(processes.info(child).unwrap().ram.charged, child_charge);
    // The loader mapped the IPC page RW+NX. Attempt instruction fetch there.
    let fault_endpoint = allocator
        .allocate_fixed_sized::<::sel4::cap_type::Endpoint>()
        .unwrap();
    let fault_reply = allocator
        .allocate(::sel4::ObjectBlueprint::Reply)
        .unwrap()
        .cast();
    let peer_tcb = processes.tcb(peer).unwrap();
    peer_tcb
        .tcb_set_space(
            fault_endpoint.cptr(),
            processes.cnode(peer).unwrap(),
            ::sel4::CNodeCapData::new(0, ::sel4::WORD_SIZE - CHILD_CAPS_SIZE_BITS as usize),
            processes.vspace(peer).unwrap(),
        )
        .unwrap();
    let ipc_address =
        horn_procd::elf::Image::parse(CHILD_ELF, GRANULE_SIZE..VM_START, RAM_LIMIT / GRANULE_SIZE)
            .unwrap()
            .ipc_address;
    let mut regs = ::sel4::UserContext::default();
    *regs.pc_mut() = ipc_address as _;
    peer_tcb.tcb_write_all_registers(true, &mut regs).unwrap();
    let (info, _) = fault_endpoint.recv(fault_reply);
    let fault = ::sel4::with_ipc_buffer(|ipc| ::sel4::Fault::new(ipc, &info));
    let ::sel4::Fault::VmFault(fault) = fault else {
        panic!("expected NX instruction-fetch fault")
    };
    assert_eq!(fault.inner().get_Addr(), ipc_address as u64);
    assert_eq!(fault.inner().get_PrefetchFault(), 1);
    assert_ne!(fault.inner().get_FSR() & 1, 0); // Protection fault on a present page.
    ::sel4::debug_println!("NX_EXECUTE_NEVER_PASS fault=true");
    processes.stop(peer, Exit::Killed).unwrap();
    processes.finish(peer).unwrap();
    assert_eq!(processes.info(peer).unwrap().ram.charged, 0);
    allocator.release_slot(fault_reply.bits() as usize).unwrap();
    allocator
        .release_slot(fault_endpoint.bits() as usize)
        .unwrap();

    // Fail after the ELF fits but before all kernel objects fit. Rollback must
    // return resources; a subsequent full launch must still work.
    let before = allocator.account().usage().charged;
    let mut denied = config("too-small");
    denied.ram_limit = child_charge - GRANULE_SIZE;
    assert!(matches!(
        processes.create(CHILD_ELF, denied),
        Err(horn_procd::Error::Resource(horn_vm::Error::QuotaExceeded))
    ));
    assert_eq!(allocator.account().usage().charged, before);
    let recycled = processes.create(CHILD_ELF, config("recycled")).unwrap();
    processes.stop(recycled, Exit::Killed).unwrap();
    processes.finish(recycled).unwrap();
    assert_eq!(allocator.account().usage().charged, before);

    let mut child_allocator = processes.allocator(child).unwrap();
    let child_reply = child_allocator
        .allocate(::sel4::ObjectBlueprint::Reply)
        .unwrap();
    let lifecycle = processes.provision_self(child, system).unwrap();
    let pool = FramePool::for_account(child_allocator.clone());
    let mut memory = VmService::new(
        child_allocator,
        pool,
        system,
        processes.vspace(child).unwrap(),
        VM_START..VM_START + VM_SIZE,
        128,
    )
    .unwrap();
    let elf =
        unsafe { memory.publish(LAUNCH_ELF, ::sel4::init_thread::slot::VSPACE.cap(), scratch) }
            .unwrap();
    let (process_factory, process_observer) = processes
        .provision_launch(
            child,
            system,
            LaunchPolicy {
                max_children: 5,
                max_ram: 8 * 1024 * 1024,
                vm_window: VM_START..VM_START + VM_SIZE,
                cpu_budget_us: 1000,
                cpu_period_us: 1000,
            },
        )
        .unwrap();
    let factory = memory.factory();
    let address_space = memory.address_space();
    // Private loader placement, not the task ABI. The child receives named
    // context entries; procd accounts its CNode and freezes these installs.
    for (slot, source) in [
        (CHILD_CAPS_SELF_SLOT, processes.cnode(child).unwrap().bits()),
        (CHILD_CAPS_SERVICE_SLOT, counter.bits()),
        (CHILD_CAPS_WAKE_SLOT, wake.bits()),
        (CHILD_CAPS_REPLY_SLOT, child_reply.bits()),
        (CHILD_CAPS_VM_SLOT, factory.bits()),
        (CHILD_CAPS_ADDRESS_SPACE_SLOT, address_space.bits()),
        (CHILD_CAPS_LIFECYCLE_SLOT, lifecycle.bits()),
        (CHILD_CAPS_PROCESS_FACTORY_SLOT, process_factory.bits()),
        (CHILD_CAPS_PROCESS_OBSERVER_SLOT, process_observer.bits()),
        (CHILD_CAPS_ELF_SLOT, elf.as_ref().slot as _),
    ] {
        processes
            .install_initial_capability(
                child,
                slot,
                root.absolute_cptr(::sel4::CPtr::from_bits(source)),
                ::sel4::CapRights::all(),
            )
            .unwrap();
    }
    memory.finish_exports().unwrap();
    processes.attach_vm(child, memory).unwrap();
    let mut bindings = [
        libhorn::startup::Entry {
            name: "horn.vm",
            selector: CHILD_CAPS_VM_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "horn.address-space",
            selector: CHILD_CAPS_ADDRESS_SPACE_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "horn.lifecycle",
            selector: CHILD_CAPS_LIFECYCLE_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "horn.reply",
            selector: CHILD_CAPS_REPLY_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "service",
            selector: CHILD_CAPS_SERVICE_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "wake",
            selector: CHILD_CAPS_WAKE_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "elf",
            selector: CHILD_CAPS_ELF_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "process_factory",
            selector: CHILD_CAPS_PROCESS_FACTORY_SLOT as _,
        },
        libhorn::startup::Entry {
            name: "process_observer",
            selector: CHILD_CAPS_PROCESS_OBSERVER_SLOT as _,
        },
    ];
    install_context(&mut processes, child, &mut bindings);
    ::sel4::debug_println!("idl demo root: child Horn ABI verified at runtime");
    processes.start(child).unwrap();

    let target =
        Cap::<CounterTag>::from_ref(space.register(counter).unwrap().with_mode(CapMode::COPY));
    let recv = space.reserve_slot().unwrap();
    let mut transport = Sel4Transport::new(root, recv, space);
    let client = CounterReceiver::from_cap(target);
    assert!(matches!(
        client.add(&mut transport, 0).unwrap().state,
        CounterStateOwned::Empty
    ));
    let value = client.add(&mut transport, 41).unwrap();
    let CounterStateOwned::Ready(signal) = value.state else {
        panic!("missing notification")
    };
    transport
        .space
        .cap::<::sel4::cap_type::Notification>(signal.cap_ref())
        .unwrap()
        .signal();
    wake.wait();
    ::sel4::debug_println!("IDL_DEMO_PASS value={}", value.value);
    let mut server = Sel4Server::new(root, system, recv, transport.space);
    loop {
        // Compose generated dispatchers, without duplicating method tables.
        let (_, request, reply) = server
            .recv_request_dynamic(
                |label| {
                    vm::vm_host_request_layout(label)
                        .or_else(|_| process::process_host_request_layout(label))
                },
                system_reply,
                |label, input| {
                    if vm::vm_host_request_layout(label).is_ok() {
                        vm::VmHostRequest::decode(label, input).map(Request::Vm)
                    } else {
                        process::ProcessHostRequest::decode(label, input).map(Request::Process)
                    }
                },
            )
            .unwrap();
        match request {
            Request::Vm(request) => {
                if let Some(response) = processes.dispatch_vm(request, reply.badge) {
                    vm::vm_host_reply(reply, &mut server.space, &response).unwrap();
                } else {
                    reply.reply_status(1).unwrap();
                }
                processes.finish_reply(&mut server).unwrap();
            }
            Request::Process(request) => {
                let response = processes.dispatch(request, reply.badge);
                if let Some(id) = processes.take_exit() {
                    processes.finish(id).unwrap();
                    if id != child {
                        continue;
                    }
                    let info = processes.info(id).unwrap();
                    assert_eq!(info.ram.charged, 0);
                    assert_eq!(info.ram.quarantined, 0);
                    assert_eq!(allocator.account().usage().charged, system_charge);
                    assert_eq!(info.exit, Some(Exit::Code(0)));
                    assert!(
                        processes
                            .list()
                            .all(|p| p.state == horn_procd::State::Exited && p.ram.charged == 0)
                    );
                    ::sel4::debug_println!(
                        "PROCD_PASS elf=true quota=true untyped=true cleanup=true"
                    );
                    loop {
                        wake.wait();
                    }
                }
                process::process_host_reply(reply, &mut server.space, &response).unwrap();
                processes.finish_reply(&mut server).unwrap();
            }
        }
    }
}

#[repr(C, align(4096))]
struct FreePagePlaceholder(#[allow(dead_code)] [u8; GRANULE_SIZE]);
static mut FREE_PAGE_PLACEHOLDER: FreePagePlaceholder = FreePagePlaceholder([0; GRANULE_SIZE]);
fn init_free_page_addr(bootinfo: &::sel4::BootInfo) -> usize {
    let addr = ptr::addr_of!(FREE_PAGE_PLACEHOLDER) as usize;
    unsafe extern "C" {
        static __executable_start: usize;
    }
    let image_start = ptr::addr_of!(__executable_start) as usize;
    bootinfo
        .user_image_frames()
        .index(addr / GRANULE_SIZE - image_start / GRANULE_SIZE)
        .cap()
        .frame_unmap()
        .unwrap();
    addr
}
