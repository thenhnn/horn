#![no_std]
#![no_main]
#![feature(const_index)]
#![feature(const_trait_impl)]
#![allow(incomplete_features)]
extern crate alloc;

use core::ptr;
use horn_ipc::*;
use horn_procd::protocol as process;
use horn_procd::{Config, Exit, Manager};
use horn_vm::protocol as vm;
use horn_vm::{
    sel4::{FramePool, KernelAllocator},
    service::Service as VmService,
};
use sel4_root_task::{Never, root_task};
mod memory_probe;
#[allow(dead_code, unused_imports, non_camel_case_types)]
mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
use protocol::*;

static CHILD_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/child.elf"));
const GRANULE_SIZE: usize = ::sel4::FrameObjectType::GRANULE.bytes();
const VM_START: usize = 0x4000_0000;
const VM_SIZE: usize = 64 * 1024 * 1024;
const RAM_LIMIT: usize = 32 * 1024 * 1024;

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
    Process(process::ProcessSelfRequest),
}

#[root_task(stack_size = 1024 * 256, heap_size = 1024 * 1024 * 4)]
fn main(bootinfo: &::sel4::BootInfoPtr) -> ::sel4::Result<Never> {
    ::sel4::debug_println!("idl demo root: start");
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
    let system_charge = allocator.account().usage().charged;
    let mut processes = Manager::new(
        allocator.clone(),
        ::sel4::init_thread::slot::VSPACE.cap(),
        scratch,
        ::sel4::init_thread::slot::ASID_POOL.cap(),
        bootinfo.sched_control().index(0).cap(),
    );

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
    let child_cnode = processes.cnode(child).unwrap();
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
    let factory = memory.factory();
    let address_space = memory.address_space();
    // The generated named slots remain the bootstrap contract. The CNode itself
    // is created by procd and charged to the process, not an unaccounted cap packer.
    for (slot, source) in [
        (CHILD_CAPS_SELF_SLOT, child_cnode.bits()),
        (CHILD_CAPS_SERVICE_SLOT, counter.bits()),
        (CHILD_CAPS_WAKE_SLOT, wake.bits()),
        (CHILD_CAPS_REPLY_SLOT, child_reply.bits()),
        (CHILD_CAPS_TCB_SLOT, processes.tcb(child).unwrap().bits()),
        (CHILD_CAPS_VM_SLOT, factory.bits()),
        (CHILD_CAPS_ADDRESS_SPACE_SLOT, address_space.bits()),
        (CHILD_CAPS_LIFECYCLE_SLOT, lifecycle.bits()),
    ] {
        child_cnode
            .absolute_cptr_from_bits_with_depth(slot as _, CHILD_CAPS_SIZE_BITS as usize)
            .copy(
                &root.absolute_cptr(::sel4::CPtr::from_bits(source)),
                ::sel4::CapRights::all(),
            )
            .unwrap();
    }
    processes.start(child).unwrap();
    ::sel4::debug_println!("idl demo root: child Horn ABI verified at runtime");

    let target =
        Cap::<CounterTag>::from_ref(space.register(counter).unwrap().with_mode(CapMode::COPY));
    let recv = space.reserve_slot().unwrap();
    let transport = Sel4Transport::new(root, recv, space);
    let mut client = CounterClient::new(transport, target);
    assert!(matches!(client.add(0).unwrap().state, CounterState::Empty));
    let value = client.add(41).unwrap();
    let CounterState::Ready(signal) = value.state else {
        panic!("missing notification")
    };
    client
        .transport
        .space
        .cap::<::sel4::cap_type::Notification>(signal.as_ref())
        .unwrap()
        .signal();
    wake.wait();
    ::sel4::debug_println!("IDL_DEMO_PASS value={}", value.value);
    let mut server = Sel4Server::new(root, system, recv, client.transport.space);
    loop {
        // Compose generated dispatchers, without duplicating method tables.
        let (_, request, reply) = server
            .recv_request_dynamic(
                |label| {
                    vm::vm_host_request_layout(label)
                        .or_else(|_| process::process_self_request_layout(label))
                },
                system_reply,
                |label, input| {
                    if vm::vm_host_request_layout(label).is_ok() {
                        vm::VmHostRequest::decode(label, input).map(Request::Vm)
                    } else {
                        process::ProcessSelfRequest::decode(label, input).map(Request::Process)
                    }
                },
            )
            .unwrap();
        match request {
            Request::Vm(request) => {
                let response = memory.dispatch(request, reply.badge);
                vm::vm_host_reply(reply, &mut server.space, &response).unwrap();
                memory.finish_reply(&mut server).unwrap();
            }
            Request::Process(request) => {
                let response = processes.dispatch(request, reply.badge);
                if let Some(id) = processes.take_exit() {
                    memory.shutdown().unwrap();
                    processes.finish(id).unwrap();
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
                process::process_self_reply(reply, &mut server.space, &response).unwrap();
                server.recv_slot.delete().unwrap();
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
