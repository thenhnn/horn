//! Default Rust heap with a separate, bounded bootstrap/VM control allocator.
use crate::native;
use core::alloc::{GlobalAlloc, Layout};
use one_shot_mutex::sync::RawOneShotMutex;
use sel4_dlmalloc::{StaticDlmalloc, StaticHeapBounds};

/// Use as `#[global_allocator]` and call [`crate::install`] before application
/// allocations. Before install, and during mimalloc's own VM RPC callbacks,
/// allocations use the fixed control heap. All other allocations use mimalloc;
/// ordinary mimalloc exhaustion NEVER falls back to the control heap.
///
/// Deallocation uses pointer provenance, not the current initialization/callback
/// state: bootstrap objects can outlive installation. The default GlobalAlloc
/// realloc implementation also permits migration between these heaps.
///
/// Like the current VmHeap/mimalloc runtime, this is single-threaded. BUSY is a
/// recursion guard, not a process-wide SMP allocator routing mechanism.
pub struct GlobalAllocator {
    control: StaticDlmalloc<RawOneShotMutex>,
    bounds: StaticHeapBounds,
}
// SAFETY: bounds is an immutable address descriptor, not borrowed mutable
// storage. The control allocator synchronizes access; new() additionally requires
// the current VM/mimalloc runtime's single-threaded execution discipline.
unsafe impl Sync for GlobalAllocator {}
impl GlobalAllocator {
    /// # Safety
    /// `bounds` must describe exclusive static storage, disjoint from VM memory.
    /// The process must use a single thread, including all allocator callbacks.
    /// Use `sel4_dlmalloc::StaticHeap::bounds()`, as for StaticDlmalloc itself.
    pub const unsafe fn new(bounds: StaticHeapBounds) -> Self {
        Self {
            control: StaticDlmalloc::new(bounds),
            bounds,
        }
    }
    fn is_control(&self, pointer: *mut u8) -> bool {
        (pointer as usize).wrapping_sub(self.bounds.start() as usize) < self.bounds.size()
    }
}
// SAFETY: both underlying allocators satisfy their allocation contracts. The
// exclusive fixed control range identifies the allocator that owns each pointer
// even after installation or realloc; native VM callback metadata cannot recurse
// into mimalloc. The runtime's documented single-thread requirement still applies.
unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if native::control_required() {
            unsafe { self.control.alloc(layout) }
        } else {
            native::allocate(layout)
                .map(|p| p.as_ptr().cast())
                .unwrap_or(core::ptr::null_mut())
        }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if self.is_control(pointer) {
            unsafe { self.control.dealloc(pointer, layout) };
        } else {
            // SAFETY: GlobalAlloc callers pass a live pointer returned by us.
            unsafe { native::deallocate(core::ptr::NonNull::new_unchecked(pointer), layout) };
        }
    }
}
