//
// Copyright 2024, Colias Group, LLC
//
// SPDX-License-Identifier: BSD-2-Clause
//

use core::ptr;

use sel4::CapTypeForFrameObjectOfFixedSize;
use sel4_panicking::catch_unwind;
use sel4_panicking_env::abort;

use crate::main;

const STACK_SIZE: usize = 1024 * 256;

sel4_runtime_common::declare_stack!(STACK_SIZE);

// Allocator policy belongs to each executable, not this entrypoint.

sel4_panicking_env::register_debug_put_char!(sel4::debug_put_char);

sel4_runtime_common::declare_entrypoint_with_stack_init!();

sel4_runtime_common::declare_rust_entrypoint! {
    entrypoint()
}

fn entrypoint() -> ! {
    unsafe {
        sel4::set_ipc_buffer(get_ipc_buffer().as_mut().unwrap());
    }

    match catch_unwind(main) {
        #[allow(unreachable_patterns)]
        Ok(never) => never,
        Err(_) => abort!("main() panicked"),
    }
}

fn get_ipc_buffer() -> *mut sel4::IpcBuffer {
    unsafe extern "C" {
        static _end: usize;
    }
    (ptr::addr_of!(_end) as usize)
        .next_multiple_of(sel4::cap_type::Granule::FRAME_OBJECT_TYPE.bytes())
        as *mut sel4::IpcBuffer
}

const HEAP_SIZE: usize = 256 * 1024;
static CONTROL_HEAP: sel4_dlmalloc::StaticHeap<HEAP_SIZE> = sel4_dlmalloc::StaticHeap::new();

// This child runs one thread. Only bootstrap/VM callback metadata uses this
// bounded static storage; ordinary allocations switch to mimalloc at install().
#[global_allocator]
static GLOBAL_ALLOCATOR: horn_mimalloc::GlobalAllocator =
    unsafe { horn_mimalloc::GlobalAllocator::new(CONTROL_HEAP.bounds()) };

#[allow(dead_code)] // The separate launched probe only exercises bootstrap.
pub fn is_control(pointer: *const u8) -> bool {
    (pointer as usize).wrapping_sub(CONTROL_HEAP.bounds().start() as usize) < HEAP_SIZE
}
