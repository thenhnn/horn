# Resident mimalloc port (PC demo)

`default.nix` compiles the pinned, vendored mimalloc 3.6.0 sources into a
freestanding archive without fetching sources. `horn/prim.c` implements its OS
primitives through `runtime/rust/horn-mimalloc`; `init.c` configures initialization.
The Rust wrapper implements the standard allocator API, not a replacement heap.

The demo keeps its Rust global/control heap independent and allocates application
collections explicitly with `Mimalloc`. VM IPC and capability-slot bookkeeping
must never recurse through mimalloc. rust-sel4 initializes native C/Rust TLS and
provides IPC, capability operations, scheduling and virtual-range allocation.

OS allocation creates resident backing and a region, prepares/maps it with one
borrowed capability, then closes the backing handle. The mapping retains backing.
OS free destroys that region; slots and ranges are reusable. Failed cleanup is
not treated as successful reuse. There is no forced allocator teardown: metadata
may stay resident until process cleanup is implemented.

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

Boot tests exercise real collections, small/large allocations, alignment,
exhaustion and backing returned by normal free/collection. The VM charge must
decrease after collection, not reach zero; allocator metadata may remain. No allocation-latency claims have
been measured. See `platform/core/vm/README.md` for VM lifecycle/accounting limits.
