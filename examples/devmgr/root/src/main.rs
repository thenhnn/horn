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

// Private bootstrap RPCs contain no capabilities. Nonblocking receive keeps a
// startup deadline and fault monitoring independent of devmgr making progress.
struct Reply {
    cap: sel4::cap::Reply,
    label: u64,
}
impl AsyncReply for Reply {
    fn reply_error(self, _: RpcError) -> Result<(), RpcError> {
        Err(RpcError::InvalidValue)
    }
    fn reply_wire<S: CapSpace, W: Wire>(
        self,
        _: &mut S,
        value: &W,
        _: &'static CapLayout,
    ) -> Result<(), RpcError> {
        if W::CAPS != 0 || W::SIZE + 4 > MAX_MESSAGE_BYTES {
            return Err(RpcError::TooManyCaps);
        }
        let mut bytes = [0u8; MAX_MESSAGE_BYTES];
        let mut encoder = Encoder::new(&mut bytes[4..]);
        value.encode(&mut encoder);
        let words = (encoder.position() + 4).div_ceil(8);
        sel4::with_ipc_buffer_mut(|ipc| {
            ipc.msg_bytes_mut()[..words * 8].copy_from_slice(&bytes[..words * 8])
        });
        self.cap
            .send(sel4::MessageInfo::new(self.label, 0, 0, words));
        Ok(())
    }
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
            0,
        )
        .unwrap();
    let child = manager.cnode(process).unwrap();
    let platform_slot = allocator.reserve_slot().unwrap();
    let fault_slot = allocator.reserve_slot().unwrap();
    for (slot, badge) in [(platform_slot, 1), (fault_slot, 2)] {
        root.absolute_cptr(sel4::CPtr::from_bits(slot as _))
            .mint(
                &root.absolute_cptr(bootstrap.cptr()),
                sel4::CapRights::all(),
                badge,
            )
            .unwrap();
    }
    for (slot, source) in [
        (DEVICE_CAPS_SELF_SLOT, child.bits()),
        (DEVICE_CAPS_REPLY_SLOT, reply.bits()),
        (DEVICE_CAPS_SERVICE_SLOT, service.bits()),
        (DEVICE_CAPS_PLATFORM_SLOT, platform_slot as _),
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
    let mut platform = unsafe { platform::Platform::new(boot, allocator, scratch, split) };
    let mut space =
        Sel4CapSpace::new_local(root, (split + platform::TEMP_SLOTS) as _, empty.end as _);
    // The deadline loop must also let devmgr run on a single-CPU machine.
    // Keep the root's MCP authority, but use the child's priority and yield.
    let root_tcb = sel4::init_thread::slot::TCB.cap();
    root_tcb.tcb_set_priority(root_tcb, 0).unwrap();
    let stalled = option_env!("DEVMGR_TEST_STALL") == Some("1");
    let timeout_us = if stalled { 100_000 } else { 30_000_000 };
    if !stalled {
        manager.start(process).unwrap();
    }
    while !platform.ready {
        if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(started) > mhz * timeout_us {
            fatal(reset, "startup timeout");
        }
        let (info, badge) = bootstrap.nb_recv(bootstrap_reply);
        // NBRecv reports no sender through badge 0; MessageInfo is not valid.
        if badge == 0 {
            sel4::r#yield();
            continue;
        }
        if badge == 2 {
            fatal(reset, "devmgr fault");
        }
        if badge != 1 || info.extra_caps() != 0 || info.length() * 8 > MAX_MESSAGE_BYTES {
            sel4::debug_println!(
                "bootstrap invalid label={} words={} caps={}",
                info.label(),
                info.length(),
                info.extra_caps()
            );
            fatal(reset, "bootstrap protocol");
        }
        let mut bytes = [0u8; MAX_MESSAGE_BYTES];
        sel4::with_ipc_buffer(|ipc| {
            bytes[..info.length() * 8].copy_from_slice(&ipc.msg_bytes()[..info.length() * 8])
        });
        let request = DevicePlatformObjectRequest::decode(
            info.label(),
            &mut Decoder::new(&bytes[..info.length() * 8], &mut WireCaps::default()),
        )
        .unwrap_or_else(|_| fatal(reset, "devmgr fault"));
        let response = device_platform_object_dispatch(request, &mut platform);
        device_platform_object_reply(
            Reply {
                cap: bootstrap_reply,
                label: info.label(),
            },
            &mut space,
            &response,
        )
        .unwrap();
    }
    let observer = sel4::cap::Endpoint::from_bits(observer_slot as _);
    let mut observer_ref = space.register(observer).unwrap();
    observer_ref.rights = Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0);
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
        manager
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
