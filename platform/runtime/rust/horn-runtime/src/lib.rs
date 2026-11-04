// Entrypoint glue derived from rust-sel4 task examples.
// Copyright 2024, Colias Group, LLC
// SPDX-License-Identifier: BSD-2-Clause
//
//! Single-threaded Horn task entry, VM-backed default heap and bootstrap IPC.
//! Root tasks that implement the VM backend must use their own bootstrap runtime.
#![no_std]

use core::ops::Range;
use horn_ipc::*;

#[cfg(target_arch = "x86_64")]
pub mod clock;
#[cfg(target_arch = "x86_64")]
pub use clock::Clock;

mod context;
pub use context::{Context, Inbox, Received};

pub use horn_log as logging;
pub use horn_mimalloc as allocator;
pub use horn_mimalloc::Error;

/// Reference an already-installed local bootstrap capability, not new authority.
pub(crate) fn bootstrap_cap<T: CapType>(root: sel4::cap::CNode, slot: u32) -> Cap<T> {
    Cap::new(
        root.bits() as _,
        slot,
        sel4::WORD_SIZE as _,
        0,
        CapMode::COPY,
        Rights::ALL,
    )
}

/// Build an IPC transport from an empty, exclusive CSpace range. Its first slot
/// is reserved for receive, never handed to the imported-cap allocator.
pub(crate) fn transport(
    root: sel4::cap::CNode,
    slots: Range<u32>,
) -> Result<Sel4Transport<Sel4CapSpace>, Error> {
    let first = slots.start.checked_add(1).ok_or(Error::Invalid)?;
    if first >= slots.end {
        return Err(Error::Invalid);
    }
    Ok(Sel4Transport::new(
        root,
        root.absolute_cptr(sel4::CPtr::from_bits(slots.start as _)),
        Sel4CapSpace::new_local(root, first as _, slots.end as _),
    ))
}

/// Diagnostic provenance check; control storage serves only bootstrap and heap
/// callback metadata, never ordinary application OOM after heap installation.
pub fn is_control(pointer: *const u8) -> bool {
    let bounds = __private::CONTROL.bounds();
    (pointer as usize).wrapping_sub(bounds.start() as usize) < bounds.size()
}

/// Declare one Horn ABI task per ELF, with mimalloc as its default allocator.
/// Reuses rust-sel4 stack/TLS/constructors/panic entry machinery. Stack and bounded
/// control storage are each 256KiB, as in the former executable-local glue.
///
/// This runtime supports ONE thread only; declaring it is not SMP enablement.
/// The explicit `deferred_heap` variant is ABI2 too, but leaves the heap for
/// staged allocator tests to install through their Context.
#[macro_export]
macro_rules! task {
    ($main:path $(,)?) => { $crate::task!(@entry $main, true); };
    ($main:path, deferred_heap $(,)?) => { $crate::task!(@entry $main, false); };
    (@entry $main:path, $heap:expr) => {
        mod __horn_task {
            use super::*;
            #[cfg(not(target_arch = "x86_64"))]
            compile_error!("Horn ABI2 register startup is currently x86_64 only");
            #[global_allocator]
            static ALLOCATOR: $crate::__private::GlobalAllocator = unsafe {
                $crate::__private::GlobalAllocator::new($crate::__private::CONTROL.bounds())
            };
            #[used]
            #[unsafe(link_section = ".horn")]
            static HORN_TAG: [u8; 8] = *b"HORN\x02\x00\x00\x00";
            $crate::__private::runtime::declare_stack!(256 * 1024);
            $crate::__private::panicking::register_debug_put_char!($crate::__private::sel4::debug_put_char);
            $crate::__private::runtime::declare_entrypoint_with_stack_init!();
            $crate::__private::runtime::declare_rust_entrypoint! { horn_entry(pointer: usize, length: usize) }
            fn horn_entry(pointer: usize, length: usize) -> ! {
                let application: fn($crate::Context) -> ! = $main;
                unsafe { $crate::__private::enter_context(application, pointer, length, $heap) }
            }
        }
    };

}

#[doc(hidden)]
pub mod __private {
    pub use horn_mimalloc::GlobalAllocator;
    pub use sel4;
    pub use sel4_panicking_env as panicking;
    pub use sel4_runtime_common as runtime;
    pub static CONTROL: sel4_dlmalloc::StaticHeap<{ 256 * 1024 }> =
        sel4_dlmalloc::StaticHeap::new();

    /// Loader-only ABI2 entry. Invalid records fail closed; never try ABI1.
    pub unsafe fn enter_context(
        main: fn(super::Context) -> !,
        pointer: usize,
        length: usize,
        install_heap: bool,
    ) -> ! {
        if pointer == 0 || length > isize::MAX as usize || pointer.checked_add(length).is_none() {
            panicking::abort!("invalid ABI2 startup range");
        }
        let bytes = unsafe { core::slice::from_raw_parts(pointer as *const u8, length) };
        let context = unsafe { super::Context::from_record(bytes) }.expect("ABI2 startup record");
        let ipc = context.record.header().ipc_buffer as *mut sel4::IpcBuffer;
        unsafe {
            sel4::set_ipc_buffer(ipc.as_mut().unwrap());
        }
        match sel4_panicking::catch_unwind(|| {
            let mut context = context;
            if install_heap {
                let heap = context.heap().expect("ABI2 heap context");
                unsafe {
                    super::allocator::install(heap).expect("runtime VM heap");
                }
            }
            unsafe {
                context.install_logging().expect("runtime logger");
            }
            main(context)
        }) {
            #[allow(unreachable_patterns)]
            Ok(never) => never,
            Err(_) => panicking::abort!("main(ctx) panicked"),
        }
    }
}
