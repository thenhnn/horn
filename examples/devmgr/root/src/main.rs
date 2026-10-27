#![no_std]
#![no_main]
#![feature(const_index)]
#![feature(const_trait_impl)]
#![allow(incomplete_features)]
extern crate alloc;
mod platform;

use horn_devmgr::protocol::*;
use horn_ipc::*;
use horn_procd::{Config, Manager};
use horn_vm::sel4::KernelAllocator;
use sel4_root_task::{Never, root_task};

static ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/devmgr.elf"));

const PLATFORM_BADGE: u64 = 1 << 48;
const FAULT_BADGE: u64 = PLATFORM_BADGE + 1;
enum Request {
    Platform(DevicePlatformObjectRequest),
    Vm(horn_vm::protocol::VmHostRequest),
    Process(horn_procd::protocol::ProcessHostRequest),
}
fn layout(label: u64) -> Result<&'static CapLayout, RpcError> {
    device_platform_object_request_layout(label)
        .or_else(|_| horn_vm::protocol::vm_host_request_layout(label))
        .or_else(|_| horn_procd::protocol::process_host_request_layout(label))
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
    } else {
        Ok(Request::Process(
            horn_procd::protocol::ProcessHostRequest::decode(label, input)?,
        ))
    }
}
fn serve(
    platform: &mut platform::Platform,
    server: &mut Sel4Server<Sel4CapSpace>,
    authority: sel4::cap::Reply,
    reset: usize,
) {
    sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&server.recv_slot));
    let (info, badge) = server.endpoint.nb_recv(authority);
    if badge == 0 {
        sel4::r#yield();
        return;
    }
    if badge == FAULT_BADGE {
        fatal(reset, "devmgr stopped or faulted");
    }
    let envelope = server.decode_received_dynamic(info, badge, authority, layout, decode);
    let Ok((_, request, reply)) = envelope else {
        server.recv_slot.delete().unwrap();
        if badge == PLATFORM_BADGE {
            fatal(reset, "bootstrap protocol");
        }
        return;
    };
    match request {
        Request::Platform(request) if badge == PLATFORM_BADGE => {
            let response = device_platform_object_dispatch(request, platform);
            device_platform_object_reply(reply, &mut server.space, &response).unwrap();
        }
        Request::Vm(request) => {
            if let Some(response) = platform.manager.dispatch_vm(request, badge) {
                horn_vm::protocol::vm_host_reply(reply, &mut server.space, &response).unwrap();
            }
        }
        Request::Process(request) => {
            let response = platform.manager.dispatch(request, badge);
            horn_procd::protocol::process_host_reply(reply, &mut server.space, &response).unwrap();
        }
        _ => {}
    }
    platform.manager.finish_reply(server).unwrap();
}

#[root_task(stack_size = 256 * 1024, heap_size = 4 * 1024 * 1024)]
fn main(boot: &sel4::BootInfoPtr) -> sel4::Result<Never> {
    let root = sel4::init_thread::slot::CNODE.cap();
    let empty = boot.empty().range();
    let split = empty.start + empty.len() / 2;
    let mut sources = boot
        .untyped_list()
        .iter()
        .enumerate()
        .filter(|(_, d)| !d.is_device());
    let (i, d) = sources.next().unwrap();
    let mut allocator = KernelAllocator::new(
        root,
        boot.untyped().index(i).cap(),
        d.size_bits(),
        empty.start..split,
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
            boot.sched_control()
                .index(boot.inner().numNodes as usize - 1)
                .cap(),
        )
    };
    let process = manager
        .create(
            ELF,
            Config {
                name: "devmgr",
                ram_limit: 8 * 1024 * 1024,
                cnode_bits: DEVICE_CAPS_SIZE_BITS as usize,
                vm_window: 0x4000_0000..0x4400_0000,
                cpu_budget_us: 1000,
                cpu_period_us: 1000,
            },
        )
        .unwrap();
    let mut child_allocator = manager.allocator(process).unwrap();
    let service = child_allocator
        .allocate_fixed_sized::<sel4::cap_type::Endpoint>()
        .unwrap();
    let unsolicited = allocator
        .allocate_fixed_sized::<sel4::cap_type::Notification>()
        .unwrap();
    let bootstrap = allocator
        .allocate_fixed_sized::<sel4::cap_type::Endpoint>()
        .unwrap();
    let reply = child_allocator
        .allocate(sel4::ObjectBlueprint::Reply)
        .unwrap()
        .cast::<sel4::cap_type::Reply>();
    let bootstrap_reply = allocator
        .allocate(sel4::ObjectBlueprint::Reply)
        .unwrap()
        .cast::<sel4::cap_type::Reply>();
    let observer_slot = allocator.reserve_slot().unwrap();
    // Clients may call, but cannot receive other clients' requests or transfer caps.
    root.absolute_cptr(sel4::CPtr::from_bits(observer_slot as _))
        .mint(
            &root.absolute_cptr(service.cptr()),
            sel4::CapRights::new(true, false, false, true),
            horn_devmgr::OBSERVER_BADGE,
        )
        .unwrap();
    let child = manager.cnode(process).unwrap();
    let platform_slot = allocator.reserve_slot().unwrap();
    let fault_slot = allocator.reserve_slot().unwrap();
    for (slot, badge) in [(platform_slot, PLATFORM_BADGE), (fault_slot, FAULT_BADGE)] {
        root.absolute_cptr(sel4::CPtr::from_bits(slot as _))
            .mint(
                &root.absolute_cptr(bootstrap.cptr()),
                sel4::CapRights::all(),
                badge,
            )
            .unwrap();
    }
    let vm = horn_vm::service::Service::new(
        child_allocator.clone(),
        horn_vm::sel4::FramePool::for_account(child_allocator.clone()),
        bootstrap,
        manager.vspace(process).unwrap(),
        0x4000_0000..0x4400_0000,
        128,
    )
    .unwrap();
    let vm_factory = vm.factory();
    let vm_space = vm.address_space();
    manager.attach_vm(process, vm).unwrap();
    for (slot, source) in [
        (DEVICE_CAPS_SELF_SLOT, child.bits()),
        (DEVICE_CAPS_REPLY_SLOT, reply.bits()),
        (DEVICE_CAPS_SERVICE_SLOT, service.bits()),
        (DEVICE_CAPS_PLATFORM_SLOT, platform_slot as _),
        (DEVICE_CAPS_VM_SLOT, vm_factory.bits()),
        (DEVICE_CAPS_ADDRESS_SPACE_SLOT, vm_space.bits()),
    ] {
        child
            .absolute_cptr_from_bits_with_depth(slot as _, DEVICE_CAPS_SIZE_BITS as usize)
            .copy(
                &root.absolute_cptr(sel4::CPtr::from_bits(source)),
                sel4::CapRights::all(),
            )
            .unwrap();
    }
    // Devmgr faults use the same monitored endpoint, never a driver endpoint.
    manager
        .tcb(process)
        .unwrap()
        .tcb_set_space(
            sel4::CPtr::from_bits(fault_slot as _),
            child,
            sel4::CNodeCapData::new(0, sel4::WORD_SIZE - DEVICE_CAPS_SIZE_BITS as usize),
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
    let started = unsafe { core::arch::x86_64::_rdtsc() };
    let enabled = option_env!("DEVMGR_TEST_VTD") == Some("1");
    let mut platform = unsafe {
        platform::Platform::new(
            boot, allocator, scratch, split, manager, process, enabled, mhz,
        )
    };
    let receive = root.absolute_cptr(sel4::CPtr::from_bits((split + platform::TEMP_SLOTS) as _));
    let space = Sel4CapSpace::new_local(
        root,
        (split + platform::TEMP_SLOTS + 1) as _,
        empty.end as _,
    );
    let mut server = Sel4Server::new(root, bootstrap, receive, space);
    // The deadline loop must also let devmgr run on a single-CPU machine.
    // Keep the root's MCP authority, but use the child's priority and yield.
    let root_tcb = sel4::init_thread::slot::TCB.cap();
    root_tcb.tcb_set_priority(root_tcb, 0).unwrap();
    let stalled = option_env!("DEVMGR_TEST_STALL") == Some("1");
    let timeout_us = if stalled { 100_000 } else { 30_000_000 };
    if !stalled {
        platform.manager.start(process).unwrap();
    }
    while !platform.ready {
        if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(started) > mhz * timeout_us {
            fatal(reset, "startup timeout");
        }
        serve(&mut platform, &mut server, bootstrap_reply, reset);
    }
    if enabled {
        loop {
            serve(&mut platform, &mut server, bootstrap_reply, reset);
        }
    }
    let mut space = server.space;
    let observer = sel4::cap::Endpoint::from_bits(observer_slot as _);
    let mut observer_ref = space.register(observer).unwrap();
    observer_ref.rights = Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0);
    let forged = space.reserve_slot().unwrap();
    assert!(
        root.absolute_cptr(*forged.path())
            .mint(
                &root.absolute_cptr(observer.cptr()),
                sel4::CapRights::new(true, false, false, true),
                horn_devmgr::ASSIGNMENT_BADGE
            )
            .is_err()
    );
    space
        .delete(CapRef::new(
            root.bits() as _,
            forged.path().bits() as _,
            sel4::WORD_SIZE as _,
            0,
            CapMode::MOVE,
            Rights::ALL,
        ))
        .unwrap();
    sel4::debug_println!("DEVMGR_AUTH_PASS observer_rebadge=false");
    let target = Cap::from_ref(observer_ref.with_mode(CapMode::COPY));
    let receive = space.reserve_slot().unwrap();
    let mut client = DeviceObserverClient::new(Sel4Transport::new(root, receive, space), target);
    // Malformed sends cannot kill the critical service or retain incoming caps.
    // Send (not Call) avoids intentionally waiting for an abandoned bad request.
    service.send(sel4::MessageInfo::new(0, 0, 0, 0));
    sel4::with_ipc_buffer_mut(|ipc| ipc.caps_or_badges_mut()[0] = unsolicited.bits());
    service.send(sel4::MessageInfo::new(0, 0, 1, 0));
    let status = client.status().unwrap();
    assert_eq!(status.iommu, IommuState::NotInitialized);
    assert!(status.pci_devices >= 5);
    let mut cursor = 0;
    let mut count = 0;
    let mut virtio = 0;
    loop {
        let page = client.list(cursor).unwrap();
        for entry in &page.entries.data[..page.entries.len as usize] {
            assert!(entry.id > cursor);
            let info = client.info(entry.id).unwrap();
            assert_eq!(info.device, *entry);
            if entry.vendor_id == 0x1af4 {
                virtio += 1;
                assert!(info.msix || info.msi);
            }
            count += 1;
        }
        if page.next == 0 {
            break;
        }
        assert!(page.next > cursor);
        cursor = page.next;
    }
    assert_eq!(count, status.pci_devices);
    assert!(virtio >= 2);
    assert!(matches!(
        client.info(0),
        Err(CallError::Remote(DeviceError::NotFound))
    ));
    assert!(matches!(
        client.list(u64::MAX),
        Err(CallError::Remote(DeviceError::Invalid))
    ));
    sel4::debug_println!(
        "DEVMGR_RPC_PASS ecam=true pagination=true info=true msi=true readonly=true"
    );
    if option_env!("DEVMGR_TEST_FAULT") == Some("1") {
        let mut registers = sel4::UserContext::default();
        *registers.pc_mut() = 0;
        platform
            .manager
            .tcb(process)
            .unwrap()
            .tcb_write_all_registers(true, &mut registers)
            .unwrap();
    }
    loop {
        let _ = bootstrap.recv(bootstrap_reply);
        fatal(reset, "devmgr stopped or faulted");
    }
}

fn fatal(reset: usize, reason: &str) -> ! {
    sel4::debug_println!("DEVMGR_FATAL reboot=true reason={}", reason);
    // Q35 full reset; no process teardown or backing reclamation on this path.
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
