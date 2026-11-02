// Entrypoint glue derived from rust-sel4 task examples.
// Copyright 2024, Colias Group, LLC
// SPDX-License-Identifier: BSD-2-Clause
//
//! Single-threaded Horn task entry, VM-backed default heap and bootstrap IPC.
//! Root tasks that implement the VM backend must use their own bootstrap runtime.
#![no_std]

use core::ops::Range;
use horn_ipc::*;

mod context;
pub use context::{Context, Inbox, Received};

pub use horn_ipc::TaskSlots;
pub use horn_mimalloc as allocator;
pub use horn_mimalloc::Error;

/// Signal READY=1 using a generated, local bootstrap notification slot.
/// The supervisor supplies a per-instance write-only capability and owns the
/// deadline/dependency policy. This does not allocate or discover authority.
pub fn notify_ready(slot: u32) {
    sel4::cap::Notification::from_bits(slot as _).signal();
}

/// Named bootstrap capabilities and an exclusive CSpace range for heap callbacks.
/// The checked layout keeps bootstrap, public receive, application RPC and heap
/// callbacks disjoint. It must match the CNode installed by the trusted loader.
pub struct VmBootstrap {
    pub cnode: u32,
    pub factory: u32,
    pub address_space: u32,
    pub slots: TaskSlots,
}

/// Reference an already-installed local bootstrap capability, not new authority.
pub fn bootstrap_cap<T: CapType>(root: sel4::cap::CNode, slot: u32) -> Cap<T> {
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
pub fn transport(
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

/// Install the default application heap after the VM bootstrap is available.
/// Normally called automatically by `task!`; explicit deferred installation is
/// useful for bootstrap/allocator tests, not an ordinary application fallback.
///
/// # Safety
/// Call once from the sole task thread after rust-sel4 initialized TLS and IPC.
/// The CSpace range must be empty, within this CNode, and exclusively reserved
/// for heap callbacks for the process lifetime. VM authority must name this
/// process's resident, writable, non-executable heap window.
pub unsafe fn install_heap(config: VmBootstrap) -> Result<(), Error> {
    if config.cnode == config.factory
        || config.cnode == config.address_space
        || config.factory == config.address_space
        || [config.cnode, config.factory, config.address_space]
            .iter()
            .any(|slot| *slot >= config.slots.public_receive())
    {
        return Err(Error::Invalid);
    }
    let root = sel4::cap::CNode::from_bits(config.cnode as _);
    let heap = allocator::VmHeap::new(
        transport(root, config.slots.heap())?,
        bootstrap_cap::<horn_vm::protocol::VmFactoryTag>(root, config.factory).as_ref(),
        bootstrap_cap::<horn_vm::protocol::VmAddressSpaceObjectTag>(root, config.address_space)
            .as_ref(),
    )?;
    unsafe { allocator::install(heap) }
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
/// `vm:` installs the VM-backed heap before invoking application code. The
/// explicit `deferred` variant leaves only bootstrap allocation available until
/// the caller installs the heap; use it for staged bootstrap/allocator probes.
#[macro_export]
macro_rules! task {
    ($main:path $(,)?) => {
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
                unsafe { $crate::__private::enter_context(application, pointer, length) }
            }
        }
    };
    ($main:path, vm: $vm:expr $(,)?) => {
        $crate::task!(@entry $main, ::core::option::Option::Some($vm));
    };
    ($main:path, deferred $(,)?) => {
        $crate::task!(@entry $main, ::core::option::Option::None);
    };
    (@entry $main:path, $vm:expr) => {
        mod __horn_task {
            use super::*;
            #[global_allocator]
            static ALLOCATOR: $crate::__private::GlobalAllocator = unsafe {
                $crate::__private::GlobalAllocator::new($crate::__private::CONTROL.bounds())
            };
            $crate::__private::ipc::horn_tag!();
            $crate::__private::runtime::declare_stack!(256 * 1024);
            $crate::__private::panicking::register_debug_put_char!($crate::__private::sel4::debug_put_char);
            $crate::__private::runtime::declare_entrypoint_with_stack_init!();
            $crate::__private::runtime::declare_rust_entrypoint! { horn_entry() }
            fn horn_entry() -> ! {
                // Do not expand caller expressions inside our unsafe context.
                let application: fn() -> ! = $main;
                let vm = $vm;
                unsafe { $crate::__private::enter(application, vm) }
            }
        }
    };
}

#[doc(hidden)]
pub mod __private {
    use core::ptr;
    pub use horn_ipc as ipc;
    pub use horn_mimalloc::GlobalAllocator;
    pub use sel4;
    use sel4::CapTypeForFrameObjectOfFixedSize;
    pub use sel4_panicking_env as panicking;
    pub use sel4_runtime_common as runtime;
    pub static CONTROL: sel4_dlmalloc::StaticHeap<{ 256 * 1024 }> =
        sel4_dlmalloc::StaticHeap::new();

    /// Loader-only ABI2 entry. Invalid records fail closed; never try ABI1.
    pub unsafe fn enter_context(main: fn(super::Context) -> !, pointer: usize, length: usize) -> ! {
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
            let heap = context.heap_bootstrap().expect("ABI2 heap context");
            unsafe {
                super::install_heap(heap).expect("runtime VM heap");
            }
            main(context)
        }) {
            #[allow(unreachable_patterns)]
            Ok(never) => never,
            Err(_) => panicking::abort!("main(ctx) panicked"),
        }
    }

    pub unsafe fn enter(main: fn() -> !, vm: Option<super::VmBootstrap>) -> ! {
        unsafe extern "C" {
            static _end: usize;
        }
        let ipc = (ptr::addr_of!(_end) as usize)
            .next_multiple_of(sel4::cap_type::Granule::FRAME_OBJECT_TYPE.bytes())
            as *mut sel4::IpcBuffer;
        unsafe {
            sel4::set_ipc_buffer(ipc.as_mut().unwrap());
        }
        match sel4_panicking::catch_unwind(|| {
            if let Some(vm) = vm {
                unsafe {
                    super::install_heap(vm).expect("runtime VM heap");
                }
            }
            main()
        }) {
            #[allow(unreachable_patterns)]
            Ok(never) => never,
            Err(_) => panicking::abort!("main() panicked"),
        }
    }
}
