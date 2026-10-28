# Resident mimalloc port (PC demo)

`default.nix` compiles the pinned, vendored mimalloc 3.6.0 sources into a
freestanding archive without fetching sources. `horn/prim.c` implements its OS
primitives through `platform/runtime/rust/horn-mimalloc`; `init.c` configures initialization.
The Rust wrapper implements the standard allocator API, not a replacement heap.

`horn-mimalloc::GlobalAllocator` is the default Rust heap in the console task and
the IDL child test: ordinary `Box`, `Vec`, zeroed allocations and reallocations
use mimalloc after `install(VmHeap)`. The explicit `Mimalloc` allocator API remains
available. rust-sel4/procd provide native TLS, entrypoints, IPC and capabilities.

A fixed control heap serves bootstrap allocations before installation and
metadata allocations inside mimalloc's own VM callbacks. Those callbacks must
not recursively allocate through mimalloc. Frees dispatch by the pointer's
owning address range, not the current mode, so bootstrap objects may outlive
installation; realloc can move them to mimalloc. Ordinary mimalloc OOM returns
failure, never a fallback to the control heap. Both heaps remain charged to the
process: control storage belongs to its resident ELF image, mimalloc backing to
its resident VM objects. Constructing GlobalAllocator is unsafe and requires
exclusive control storage and the current runtime's single-thread discipline.

OS allocation creates resident backing and a region, prepares/maps it with one
borrowed capability, then closes the backing handle. The mapping retains backing.
OS free destroys that region; slots and ranges are reusable. Failed cleanup is
not treated as successful reuse. There is no forced allocator teardown: metadata
may stay resident until procd tears down the process.

The build uses upstream MI_FREE_USE_PAGEMAP, a fully committed page map and a
32-bit virtual-address bound. This avoids large aligned metadata reservations;
regions outside the first 4 GiB are rejected before publication. All successful
OS allocations are committed and zeroed. Virtual-only reservation, decommit,
reset, huge pages and guard protection return ENOTSUP. Delayed purging and arena
pre-reservation are disabled. There is no clock, environment or strong-entropy
provider; secure/guarded modes and profiling are disabled. This is not a hardened
allocator configuration.

Native initialization is explicit and once-only, after installing the VM client.
The current demo is single-threaded. Callback state rejects recursive/concurrent
entry rather than aliasing mutable Rust state; general thread teardown and SMP
behavior remain unvalidated. Only x86-64 PC builds/boots are covered.

Debug/release boot tests exercise explicit and default-allocator collections,
Box, Vec growth, zeroing, alignment, exhaustion, freeing bootstrap objects after
installation, and backing returned by normal free/collection. The VM charge must
decrease after collection, not reach zero; allocator metadata may remain. No allocation-latency claims have
been measured. See `platform/core/vm/README.md` for VM lifecycle/accounting limits.
