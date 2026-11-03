//! Dedicated test fixture: never linked into the ordinary devmgr/shell image.
//! Shared backing is mapped by the trusted root, not VmMemory::duplicate().
use crate::{Config, Exit, Manager, Request, VM_SIZE, VM_START, VmService, process, vm};
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use horn_ipc::*;
use horn_smp_probe::{ADDRESS, Shared};
use horn_vm::sel4::{FramePool, KernelAllocator};

pub fn run(
    manager: &mut Manager,
    allocator: &mut KernelAllocator,
    space: Sel4CapSpace,
    endpoint: sel4::cap::Endpoint,
    reply: sel4::cap::Reply,
    scratch: usize,
    ticks_per_us: u64,
) -> Sel4CapSpace {
    if manager.cpu_count() < 2 {
        return space;
    }
    let root = allocator.root();
    let image = include_bytes!(concat!(env!("OUT_DIR"), "/smp_worker.elf"));
    let baseline = allocator.account().usage().charged;
    let mut children = Vec::new();
    for name in ["smp-a", "smp-b"] {
        let child = manager
            .create(
                image,
                Config {
                    name,
                    ram_limit: 32 << 20,
                    cnode_bits: process::PROCESS_CAPS_SIZE_BITS as _,
                    vm_window: VM_START..VM_START + VM_SIZE,
                    cpu_budget_us: 1000,
                    cpu_period_us: 1000,
                },
            )
            .unwrap();
        let mut own = manager.allocator(child).unwrap();
        let cnode = manager.cnode(child).unwrap();
        let child_reply = own.allocate(sel4::ObjectBlueprint::Reply).unwrap();
        let lifecycle = manager.provision_self(child, endpoint).unwrap();
        // Loader prepared translation tables for the whole window. Reserve its
        // final page for this fixture; the ordinary VM service cannot allocate it.
        let memory = VmService::new(
            own.clone(),
            FramePool::for_account(own),
            endpoint,
            manager.vspace(child).unwrap(),
            VM_START..ADDRESS,
            128,
        )
        .unwrap();
        for (slot, source) in [
            (process::PROCESS_CAPS_SELF_SLOT, cnode.bits()),
            (process::PROCESS_CAPS_VM_SLOT, memory.factory().bits()),
            (
                process::PROCESS_CAPS_ADDRESS_SPACE_SLOT,
                memory.address_space().bits(),
            ),
            (process::PROCESS_CAPS_LIFECYCLE_SLOT, lifecycle.bits()),
            (process::PROCESS_CAPS_REPLY_SLOT, child_reply.bits()),
        ] {
            manager
                .install_initial_capability(
                    child,
                    slot,
                    root.absolute_cptr(sel4::CPtr::from_bits(source)),
                    sel4::CapRights::all(),
                )
                .unwrap();
        }
        manager.attach_vm(child, memory).unwrap();
        let mut bindings = [
            libhorn::startup::Entry {
                name: "horn.vm",
                selector: process::PROCESS_CAPS_VM_SLOT as _,
            },
            libhorn::startup::Entry {
                name: "horn.address-space",
                selector: process::PROCESS_CAPS_ADDRESS_SPACE_SLOT as _,
            },
            libhorn::startup::Entry {
                name: "horn.lifecycle",
                selector: process::PROCESS_CAPS_LIFECYCLE_SLOT as _,
            },
            libhorn::startup::Entry {
                name: "horn.reply",
                selector: process::PROCESS_CAPS_REPLY_SLOT as _,
            },
        ];
        crate::install_context(manager, child, &mut bindings);
        children.push(child);
    }
    let cpus = [
        manager.cpu(children[0]).unwrap(),
        manager.cpu(children[1]).unwrap(),
    ];
    assert_ne!(cpus[0], cpus[1]);
    // One root-charged page. Mapping aliases are not additional RAM charges.
    let frame = allocator
        .allocate_fixed_sized::<sel4::cap_type::Granule>()
        .unwrap();
    let mut aliases = Vec::new();
    for child in &children {
        let slot = allocator.reserve_slot().unwrap();
        root.absolute_cptr(sel4::CPtr::from_bits(slot as _))
            .copy(&root.absolute_cptr(frame.cptr()), sel4::CapRights::all())
            .unwrap();
        let alias = sel4::cap::Granule::from_bits(slot as _);
        alias
            .frame_map(
                manager.vspace(*child).unwrap(),
                ADDRESS,
                sel4::CapRights::read_write(),
                sel4::VmAttributes::EXECUTE_NEVER,
            )
            .unwrap();
        aliases.push(alias);
    }
    frame
        .frame_map(
            sel4::init_thread::slot::VSPACE.cap(),
            scratch,
            sel4::CapRights::read_write(),
            sel4::VmAttributes::EXECUTE_NEVER,
        )
        .unwrap();
    let shared = unsafe {
        let pointer = scratch as *mut Shared;
        pointer.write(Shared::new(cpus));
        &*pointer
    };
    let receive = allocator.reserve_slot().unwrap();
    let receive_cap = root.absolute_cptr(sel4::CPtr::from_bits(receive as _));
    let mut server = Sel4Server::new(root, endpoint, receive_cap, space);
    for child in &children {
        manager.start(*child).unwrap();
    }
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    let mut exited = Vec::new();
    while exited.len() != children.len() {
        assert!(
            unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < ticks_per_us * 10_000_000,
            "SMP probe timed out"
        );
        if shared.ready.load(Ordering::Acquire) == 3 {
            if shared.command.load(Ordering::Acquire) == 0 {
                assert_ne!(
                    shared.apic[0].load(Ordering::Relaxed),
                    shared.apic[1].load(Ordering::Relaxed)
                );
                shared.command.store(1, Ordering::Release);
            }
            if shared
                .progress
                .iter()
                .all(|n| n.load(Ordering::Relaxed) >= 10_000)
            {
                shared.command.store(2, Ordering::Release);
            }
        }
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive_cap));
        let (info, badge) = endpoint.nb_recv(reply);
        if badge == 0 {
            sel4::r#yield();
            continue;
        }
        let (_, request, token) = server
            .decode_received_dynamic(
                info,
                badge,
                reply,
                |label| {
                    vm::vm_host_request_layout(label)
                        .or_else(|_| process::process_host_request_layout(label))
                },
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
                let response = manager.dispatch_vm(request, badge).unwrap();
                vm::vm_host_reply(token, &mut server.space, &response).unwrap();
            }
            Request::Process(request) => {
                let response = manager.dispatch(request, badge);
                if let Some(id) = manager.take_exit() {
                    assert!(children.contains(&id) && !exited.contains(&id));
                    exited.push(id);
                    // Exit confirmed CPU stop. Retain backing until aliases are removed.
                } else {
                    process::process_host_reply(token, &mut server.space, &response).unwrap();
                }
            }
        }
        manager.finish_reply(&mut server).unwrap();
    }
    assert_eq!(shared.command.load(Ordering::Acquire), 2);
    sel4::debug_println!(
        "SMP_PLACEMENT_PASS cpus={} workers={},{} apics={},{} progress={},{} ipc=true memory=true",
        manager.cpu_count(),
        cpus[0],
        cpus[1],
        shared.apic[0].load(Ordering::Relaxed),
        shared.apic[1].load(Ordering::Relaxed),
        shared.progress[0].load(Ordering::Relaxed),
        shared.progress[1].load(Ordering::Relaxed)
    );
    for alias in aliases {
        alias.frame_unmap().unwrap();
        allocator.release_slot(alias.bits() as usize).unwrap();
    }
    frame.frame_unmap().unwrap();
    allocator.release_slot(frame.bits() as usize).unwrap();
    for child in children {
        manager.finish(child).unwrap();
        assert_eq!(manager.info(child).unwrap().exit, Some(Exit::Code(0)));
    }
    allocator.release_slot(receive).unwrap();
    assert_eq!(allocator.account().usage().charged, baseline);
    sel4::debug_println!("SMP_CLEANUP_PASS backing=true accounts=true");
    server.space
}
