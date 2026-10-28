# Horn task runtime

`horn-runtime` composes the existing `horn-mimalloc`, `horn-ipc` and rust-sel4
runtime machinery. It is for ordinary **single-threaded** Horn ABI tasks, not
root tasks that implement the VM backend.

## Normal tasks

Declare the entrypoint once per ELF, using generated bootstrap slot names:

```rust
const SLOTS: horn_runtime::TaskSlots =
    horn_runtime::TaskSlots::new(DEVICE_CAPS_FIRST_FREE_SLOT, DEVICE_CAPS_SIZE_BITS);

horn_runtime::task!(main, vm: horn_runtime::VmBootstrap {
    cnode: DEVICE_CAPS_SELF_SLOT,
    factory: DEVICE_CAPS_VM_SLOT,
    address_space: DEVICE_CAPS_ADDRESS_SPACE_SLOT,
    slots: SLOTS,
});
```

The macro declares the Horn ABI tag, 256KiB stack, global allocator and entrypoint.
It reuses rust-sel4 for assembly entry, TLS, constructors and panic handling,
sets the ABI IPC buffer after `_end`, and installs resident VM-backed mimalloc
**before `main() -> !`**. Heap-install failure aborts; it never silently runs the
application on dlmalloc.

`TaskSlots` (pure layout logic in `horn-ipc`, reexported here) preserves the
entire generated bootstrap prefix and derives three disjoint allocations:

- `SLOTS.public_receive()` is the dedicated public-server receive slot;
- `SLOTS.application()` contains an application/private RPC receive slot and
  imports, ending at the CNode midpoint;
- `SLOTS.heap()` contains the heap callback receive slot and imports above it.

`transport(root, SLOTS.application())` reserves the range's first slot for receive
and uses the rest for imports; heap entry does the same with `SLOTS.heap()`.
For a 256-slot CNode the midpoint happens to be 128, but consumers no longer
coordinate that number. Unsupported sizes, overflowing/out-of-range prefixes
and empty import pools fail before capability operations (at compile time for
the const above). Pure host tests cover the partition and its boundaries.

This is numeric layout validation, not a kernel slot allocator or new authority.
The supplied size/prefix must describe the actual bootstrap CNode and the free
slots must really be empty. Instantiate each pool only once; copying the layout
does not grant permission to run two independent allocators over the same pool.

`bootstrap_cap(root, GENERATED_SLOT)` references an already installed capability.
`allocator` reexports `horn-mimalloc` for explicit allocation/collection/diagnostics;
`is_control(pointer)` is a provenance check used by boot tests.

## Bootstrap and safety

The bounded 256KiB control heap is shared glue, not the application heap. It is
used only before installation and inside mimalloc's VM callbacks, preventing
recursive VM allocation. Ordinary `Box`/`Vec` and upstream driver allocations use
mimalloc. There is no dlmalloc fallback on ordinary mimalloc OOM. Frees still use
pointer provenance, so bootstrap objects may survive installation.

`task!(main, deferred)` is an explicit exception for staged bootstrap tests and
the lifecycle-only launched probe, which has no VM authority. It leaves bootstrap
allocation active until an explicit installation. The IDL VM probe retains its
own deliberately staged `VmHeap` construction to test zeroing, exhaustion,
installation and freeing pre-install objects. It uses the same shared allocator
and entrypoint, not a copied runtime.

Devmgr and virtio-console use automatic installation. Root demos keep the
root-task runtime: calling their own VM server synchronously to grow a global
heap would deadlock. A future local-backend allocator is separate work.

No same-process SMP support is implied. The allocator callback recursion guard
is not a thread scheduler or a concurrent allocator-routing mechanism. Native
boot tests cover all consumers, allocator provenance/OOM/collection, actual
DMA/IRQ operation and idle wakeup; this crate deliberately has no host test
harness that would install a seL4 panic handler into a std test executable.
