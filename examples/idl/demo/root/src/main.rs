#![no_std]
#![no_main]
#![feature(const_index)]
#![feature(const_trait_impl)]
#![allow(incomplete_features)]

extern crate alloc;

use core::ptr;

use sel4_root_task::{Never, root_task};

mod child_image {
    include!(concat!(env!("OUT_DIR"), "/child_image.rs"));
}
mod child_vspace;
mod object_allocator;
mod memory;

#[allow(dead_code, unused_imports, non_camel_case_types)]
mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

use child_image::CHILD_IMAGE;
use child_vspace::create_child_vspace;
use horn_ipc::*;
use object_allocator::{find_largest_kernel_untyped, ObjectAllocator};
use protocol::*;

const GRANULE_SIZE: usize = ::sel4::FrameObjectType::GRANULE.bytes();

#[root_task(stack_size = 1024 * 256, heap_size = 1024 * 64)]
fn main(bootinfo: &::sel4::BootInfoPtr) -> ::sel4::Result<Never> {
    ::sel4::debug_println!("idl demo root: start");

    ::sel4::debug_println!("idl demo root: child tag verified at build time");

    let empty = bootinfo.empty().range();
    let split = empty.start + (empty.len() / 2).max(128);
    let mut allocator = ObjectAllocator::new(bootinfo, empty.start..split);
    let untyped = find_largest_kernel_untyped(bootinfo);

    let root_cnode = ::sel4::init_thread::slot::CNODE.cap();
    let mut client_space = Sel4CapSpace::new(
        root_cnode,
        untyped,
        split as ::sel4::Word,
        empty.end as ::sel4::Word,
    );

    let service = allocator.allocate_fixed_sized::<::sel4::cap_type::Endpoint>();
    let memory_endpoint = allocator.allocate_fixed_sized::<::sel4::cap_type::Endpoint>();
    let memory_reply: ::sel4::cap::Reply = allocator.allocate(::sel4::ObjectBlueprint::Reply).cast();
    let memory_authority = memory::mint(&mut client_space, memory_endpoint, memory::ALLOCATOR_BADGE).unwrap();
    let memory_cap = ::sel4::cap::Endpoint::from_bits(memory_authority.path().bits());
    let memory_handle = client_space.register(memory_cap).unwrap();
    let wake = allocator.allocate_fixed_sized::<::sel4::cap_type::Notification>();
    let reply: ::sel4::cap::Reply = allocator.allocate(::sel4::ObjectBlueprint::Reply).cast();
    let child_tcb = allocator.allocate_fixed_sized::<::sel4::cap_type::Tcb>();
    let child_sc = allocator.allocate_variable_sized::<::sel4::cap_type::SchedContext>(
        7,
    );
    bootinfo
        .sched_control()
        .index(0)
        .cap()
        .sched_control_configure_flags(child_sc, 1000, 1000, 0, 0, 0)
        .unwrap();

    ::sel4::debug_println!("idl demo root: building child caps");
    let service_handle = client_space.register(service).unwrap();
    let wake_handle = client_space.register(wake).unwrap();
    let reply_handle = client_space.register(reply).unwrap();
    let tcb_handle = client_space.register(child_tcb).unwrap();
    let child_caps = ChildCaps {
        service: Cap::from_ref(service_handle.with_mode(CapMode::COPY)),
        wake: Cap::from_ref(wake_handle.with_mode(CapMode::COPY)),
        reply: Cap::from_ref(reply_handle.with_mode(CapMode::MOVE)),
        tcb: Cap::from_ref(tcb_handle.with_mode(CapMode::COPY)),
        memory: Cap::from_ref(memory_handle.with_mode(CapMode::COPY)),
    };
    let child_cnode_cap = child_caps.pack(&mut client_space).unwrap();
    let child_cnode =
        client_space.cap::<::sel4::cap_type::CNode>(child_cnode_cap.as_ref()).unwrap();

    let free_page_addr = init_free_page_addr(bootinfo);
    let (child_vspace, ipc_buffer_addr, ipc_buffer_cap) = create_child_vspace(
        &mut allocator,
        &CHILD_IMAGE,
        ::sel4::init_thread::slot::VSPACE.cap(),
        free_page_addr,
        ::sel4::init_thread::slot::ASID_POOL.cap(),
    );

    child_tcb
        .tcb_set_sched_params(
            ::sel4::init_thread::slot::TCB.cap(),
            0,
            0,
            child_sc,
            ::sel4::init_thread::slot::NULL
                .cast::<::sel4::cap_type::Endpoint>()
                .cap(),
        )
        .unwrap();
    child_tcb
        .tcb_configure(
            child_cnode,
            ::sel4::CNodeCapData::new(0, ::sel4::WORD_SIZE - CHILD_CAPS_SIZE_BITS as usize),
            child_vspace,
            ipc_buffer_addr as ::sel4::Word,
            ipc_buffer_cap,
        )
        .unwrap();

    // The loader has finished using this reserved scratch page.
    let pool = unsafe {
        horn_memory::sel4::FramePool::new(
            &mut allocator.0, root_cnode, ::sel4::init_thread::slot::VSPACE.cap(),
            free_page_addr, 4,
        ).unwrap()
    };

    let mut ctx = ::sel4::UserContext::default();
    *ctx.pc_mut() = CHILD_IMAGE.entry as ::sel4::Word;
    child_tcb.tcb_write_all_registers(true, &mut ctx).unwrap();

    let target = Cap::<CounterTag>::from_ref(service_handle.with_mode(CapMode::COPY));
    let recv_slot = client_space.reserve_slot().unwrap();
    let transport = Sel4Transport::new(root_cnode, recv_slot, client_space);
    let mut client = CounterClient::new(transport, target);

    let value = client.add(41).unwrap();
    wake.wait();
    ::sel4::debug_println!("IDL_DEMO_PASS value={}", value.value);

    let transport = client.transport;
    let server = Sel4Server::new(root_cnode, memory_endpoint, recv_slot, transport.space);
    memory::serve(server, memory_reply, pool);
    wake.wait();
    ::sel4::init_thread::suspend_self()
}


#[repr(C, align(4096))]
struct FreePagePlaceholder(#[allow(dead_code)] [u8; GRANULE_SIZE]);

static mut FREE_PAGE_PLACEHOLDER: FreePagePlaceholder = FreePagePlaceholder([0; GRANULE_SIZE]);

fn init_free_page_addr(bootinfo: &::sel4::BootInfo) -> usize {
    let addr = ptr::addr_of!(FREE_PAGE_PLACEHOLDER) as usize;
    get_user_image_frame_slot(bootinfo, addr)
        .cap()
        .frame_unmap()
        .unwrap();
    addr
}

fn get_user_image_frame_slot(
    bootinfo: &::sel4::BootInfo,
    addr: usize,
) -> ::sel4::init_thread::Slot<::sel4::cap_type::Granule> {
    unsafe extern "C" {
        static __executable_start: usize;
    }
    let user_image_addr = ptr::addr_of!(__executable_start) as usize;
    bootinfo
        .user_image_frames()
        .index(addr / GRANULE_SIZE - user_image_addr / GRANULE_SIZE)
}
