# Root-hosted VM

`idl/horn_vm.py` defines the full capability-addressed VM protocol.
`src/service.rs` implements the resident subset; `src/sel4.rs` owns the kernel
objects and a shared, reclaimable Untyped allocator. The demo provisions an
ordinary child with factory and adopted-space capabilities, never raw VSpace or
ASID authority. Untyped is available explicitly through quota-backed leases.

## Surface and availability

- `VmFactory`: account information, backing, Untyped leases, address spaces, pagers, subaccounts.
- `VmUntyped`: get the kernel capability plus its size bits; close revokes all descendants.
- `VmMemory`: backing information, duplicate, close, revoke.
- `VmAddressSpace`: region reservation and destruction.
- `VmRegion`: information, prepared mapping batches, pager binding, destruction.
- `VmPager`: notification-driven fault draining and resolution batches.

`VmHost` generates dispatch. Kernel-authenticated endpoint badges and object
state authorize operations; interface casts do not grant authority. Badge values
never wrap or get reused. An account cannot be selected using a client integer.

The implementation advertises RESIDENT, PROTECTION and UNTYPED. It supports Anywhere
reservations inside one loader-provisioned window, prepared maps, aliases,
read/write protection changes, acknowledged unmaps, close/revoke and region
teardown. The loader installs translation tables before adopting the VSpace.

Demand-zero, fixed placement, new spaces, duplicate handles, subaccounts and
pagers return typed Unsupported on authorized requests. Adopted-space destruction
returns Busy: the service does not own thread shutdown/ASID reclamation. Invalid
pager capabilities are denied, not accepted as dummy pager objects.

The dynamic heap window supports readable, non-executable mappings only. Its
factory and regions reject executable ceilings; executable ELF pages are loaded
by procd outside that window. Write-plus-execute mappings **and ceilings** are rejected. x86 non-executable mappings use the vendored kernel's ExecuteNever
attribute; missing NX support fails closed. RW-to-RX code publication and its
alias policy are not implemented. Debug/release boots check permission changes
and an instruction-fetch protection fault on a present 4 KiB NX page.

## Mapping batches

`VmRegion.apply(memory: Option[Cap[VmMemoryObject, Borrow]], updates:
Parcel[List[VmUpdate, 16]])` takes one optional backing capability shared by all
Map operations. Prepare, Protect and Unmap do not need backing authority.
Same-endpoint seL4 unwrapping authenticates that borrowed capability without a
CNode bundle or per-entry capabilities. A foreign capability, serialized badge
lookalike or wrong object badge cannot select backing. Unwrapped badges are
borrow-only server references, not transferable CPtrs.

Offsets are region-relative; source offsets are backing-relative. Ranges must
be nonempty, page-aligned, overflow-free and bounded. Mapping permissions must
fit both ceilings. Prepare reserves mapping-cap slots; page metadata and native
translation tables already exist. Map requires prepared, unmapped destinations;
replacement is explicit Unmap then Map. Unmap retains prepared slots. Publication
does not allocate physical backing.

Success acknowledges completion, including native invalidation. Failure reports
the completed prefix and does not attempt later entries. An ordinary failure
leaves the failing operation unchanged. Failed rollback poisons the affected
region/backing and returns Quarantined; no assumption that its range is unmapped
is safe. Cleanup can be retried through retained authority. Batches are not
transactions or pipelined calls. Empty batches with valid authority succeed.
Parcels are inline; the 16-entry limit fits the 960-byte payload and needs at most
one borrowed capability.

## Lifetimes and accounting

Mappings retain backing after logical close. Kernel-cap copies alias the same
handle; close revokes those copies, not mappings. Revoke removes mappings before
closing the owner handle. Failed revoke retains cleanup authority and prevents
new mappings from poisoned backing. Region destruction removes native aliases
before returning backing or virtual ranges to their allocators.

`limit_bytes` is the process account limit, not a preallocated pool. Managed
backing, ELF pages, process kernel objects and whole delegated Untypeds share
that budget. Physical allocations use at least a page-sized, independently
revocable Untyped; small kernel objects are conservatively charged a whole page.
Retype inside a delegated lease is not charged again. Child account charges also
consume their parent budget; limits do not reserve RAM.

RAM is acquired lazily from the non-device BootInfo Untypeds. Release revokes
all descendants before returning the unit to the common allocator; free buddy
units coalesce. Other accounts can then use the memory. Dropping a Rust handle
or deleting a client cap does not return a lease. Failed reclamation retains
its charge and authority; quarantine is not reusable RAM.

Root control-heap metadata and the shared root CNode are not charged per request.
Object counts, cap slots and the virtual window still have explicit finite
bounds. Metadata allocation can still abort. This is not complete resource
prepayment, DMA lifetime management or a hardened untrusted-RPC server.

Explicit single-threaded exit cleanup is wired through procd: stop the thread,
clear VM objects, then revoke remaining process resources. Automatic fault/death
cleanup, malformed-request recovery, untrusted CNode recovery, SMP scheduling
and failure-injected kernel cleanup remain unfinished. seL4 does not notify the
service when the last endpoint-cap alias disappears. This service must not be used as a
claim of complete process lifecycle, DMA isolation or Linux VM integration.

## Deferred fault contract

Pager creation copies a write-only notification. Drain is nonblocking, bounded
to 16 events and followed by notification waits; producer wakeups must cover
wait/drain races. Fault IDs are pager-scoped, one-shot values, not per-fault caps.
Stale/replayed IDs and ID wrap must be rejected. Cookies are data, not authority.
Resolution uses one optional borrowed backing capability and the same ordered,
completed-prefix semantics. Queue overflow and pager destruction need explicit
fail-closed thread-resolution policies before USER_PAGER can be advertised.
