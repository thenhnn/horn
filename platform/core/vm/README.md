# Root-hosted VM

`idl/horn_vm.py` defines the full capability-addressed VM protocol.
`src/service.rs` implements the resident subset; `src/sel4.rs` owns the kernel
objects and a shared, reclaimable Untyped allocator. The demo provisions an
ordinary child with factory and adopted-space capabilities, never raw VSpace or
ASID authority. Untyped is available explicitly through quota-backed leases.

## Surface and availability

- `VmFactory`: account information, backing, Untyped leases, address spaces, pagers, subaccounts.
- `VmUntyped`: get the kernel capability plus its size bits; close revokes all descendants.
- `VmMemory` v3: backing information, immutable publication, rights, delegation, close and subtree revoke.
- `VmAddressSpace`: region reservation and destruction.
- `VmRegion`: information, prepared mapping batches, pager binding, destruction.
- `VmPager`: notification-driven fault draining and resolution batches.

`VmHost` generates dispatch. Kernel-authenticated endpoint badges and object
state authorize operations; interface casts do not grant authority. Badge values
never wrap or get reused. An account cannot be selected using a client integer.

The implementation advertises RESIDENT, PROTECTION, UNTYPED, IMMUTABLE,
DUPLICATE_MEMORY, EXECUTABLE and FIXED_PLACEMENT. It supports Anywhere and Fixed
reservations inside one loader-provisioned window, prepared maps, aliases,
read/write protection changes, acknowledged unmaps, close/revoke and region
teardown. The loader installs translation tables before adopting the VSpace.

Fixed reservations require an aligned, bounded exact address and return Overlap
without replacing existing regions/mappings. Anywhere uses the same registry of
occupied ranges. Destruction makes the reservation reusable only after confirmed
native cleanup; quarantined ranges stay occupied.

Demand-zero, new spaces, subaccounts and
pagers return typed Unsupported on authorized requests. Adopted-space destruction
returns Busy: the service does not own thread shutdown/ASID reclamation. Invalid
pager capabilities are denied, not accepted as dummy pager objects.

Dynamic mappings enforce **mapping-level W^X**: an individual mapping cannot
be writable and executable. Backing and region ceilings may contain both rights;
RW-to-RX protection changes and separate RW/RX aliases are permitted. An RX
mapping does not promise unchanged bytes: another authorized alias may write.
Backing-wide immutability is an optional stronger publication guarantee, not a
prerequisite for execution. MMIO remains non-executable. x86 NX mappings use the
vendored kernel's ExecuteNever attribute; missing NX support fails closed.
Debug/release boots check RW/RX transitions, rejected RWX mappings, execution of
code changed through a retained RW alias and an instruction-fetch fault on NX.

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

## Shared backing and immutable publication

Root Manager owns the shared registry of backing, delegated handles and regions.
It explicitly loans the tables to one serialized operation; there is no shared
mutable registry hidden in reference-counted state. Backing retains its allocating
pool/account, and regions retain their destination VSpace and cap allocator.

`delegate(rights)` requires DELEGATE and creates a child with an explicit subset
of READ, WRITE, EXECUTE, DELEGATE, MAKE_IMMUTABLE, REVOKE, DMA and CLOSE. Initial handles
have the allocated mapping ceiling and all control rights. `duplicate(ceiling)`
creates a child with mapping rights only. A copied endpoint aliases the same
handle; it does not create a delegation or identify the caller. Mapping pages
retain immutable lineage after handle close. Children and mappings are bounded
per allocating service; delegation depth is limited to 32.

`info().immutable` is backing-wide and initially false. `make_immutable()` requires
MAKE_IMMUTABLE and irreversibly sets it; repeated successful calls are idempotent.
It returns Busy while ANY address space retains a mapping with MAYWRITE or a DMA
writer exists. Protect(READ) does not clear MAYWRITE: unmap that mapping first.
Writable handles do not prevent publication, but cannot create writes afterwards.
No mapping is silently downgraded. MMIO/device-backed memory is not publishable.

After publication all new CPU writes, write upgrades and writable DMA pins are
denied, including through older handles. Read-only mappings and DMA remain valid.
There is no unfreeze, general seal vocabulary, or legacy seals compatibility API.
The memory interface is v3; consumers must rebuild.

RO attenuation is not immutability: another holder may still write mutable backing.
Immutability guarantees contents, not availability: subtree revoke can still remove
mappings. Procd reads immutable ELF/startup sources through a temporary NX view.
The synchronous root operation retains backing and excludes interleaved revoke
until its reader callback and native unmaps finish; it is not a general
fault-recoverable user-space read API.

QEMU probes use separate processes to check shared bytes, denied mapping and
protection upgrades, close versus mapping lifetime, selective subtree revoke,
unaffected siblings, global immutable checks and recipient-exit cleanup.

## Lifetimes and accounting

Mappings retain backing after logical close. Kernel-cap copies alias the same
handle; CLOSE authorizes closing that server-side handle and revoking every cap
copy, not mappings or dependent children. A shared READ-only handle can omit CLOSE,
so recipients cannot invalidate it for others. They retain their own region authority. Revoke
requires REVOKE and removes the selected handle's subtree and its mappings across
VSpaces. Allocating-process teardown revokes every backing rooted in its account. Failed revoke retains cleanup authority and prevents
new mappings from poisoned backing. Region destruction removes native aliases
before returning backing or virtual ranges to their allocators.

`limit_bytes` is the process account limit, not a preallocated pool. Managed
backing, ELF pages, process kernel objects and whole delegated Untypeds share
that budget. Physical allocations use at least a page-sized, independently
revocable Untyped; small kernel objects are conservatively charged a whole page.
Retype inside a delegated lease is not charged again. Child account charges also
consume their parent budget; limits do not reserve RAM. This account hierarchy
is not the process tree: procd creates peer process accounts under the system
allocator, so launching children does not spend the launcher's quota. Virtual
reservations do not charge backing. Resident DMA buffers count as physical RAM;
MMIO/BAR ranges do not. DMA pins retain existing backing without a second RAM charge.

RAM is acquired lazily from the non-device BootInfo Untypeds. Release revokes
all descendants before returning the unit to the common allocator; free buddy
units coalesce. Other accounts can then use the memory. Dropping a Rust handle
or deleting a client cap does not return a lease. Failed reclamation retains
its charge and authority; quarantine is not reusable RAM.

Root control-heap metadata and the shared root CNode are not charged per request.
Object counts, cap slots and the virtual window still have explicit finite
bounds. Metadata allocation can still abort. This is not complete resource
prepayment, DMA lifetime management or a hardened untrusted-RPC server.

Exit cleanup is owned by procd: revoke execution-cap copies, stop the owned
subtree, clear VM objects, then revoke remaining process resources. Automatic fault/death
cleanup, malformed-request recovery, untrusted CNode recovery, SMP scheduling
and failure-injected kernel cleanup remain unfinished. seL4 does not notify the
service when the last endpoint-cap alias disappears. This service must not be used as a
claim of complete process lifecycle, DMA isolation or Linux VM integration.

## DMA ownership

Registry-owned pins identify backing independently of process records. Only the
trusted device broker can request a pin, and the memory handle must include DMA
and the requested device access rights. Physical pages are not exported to drivers.
DMA writers block immutable publication; immutable backing permits device reads
but denies device writes. Any DMA pin currently makes subtree revoke Busy.

DeviceMapping remains separate: devmgr owns the device domain, IOVA reservation,
IOMMU mapping, per-device quota and drain policy. Close invalidates and drains
before unpin. Whole-domain detach uses the same post-drain release path. CPU
unmap and memory close follow hardware cleanup. No Drop performs DMA teardown.
Unknown cleanup outcomes retain pins/charge or fail-stop; VM cannot automatically
stop a device and does not send synchronous revoke callbacks to devmgr.

## Deferred fault contract

Pager creation copies a write-only notification. Drain is nonblocking, bounded
to 16 events and followed by notification waits; producer wakeups must cover
wait/drain races. Fault IDs are pager-scoped, one-shot values, not per-fault caps.
Stale/replayed IDs and ID wrap must be rejected. Cookies are data, not authority.
Resolution uses one optional borrowed backing capability and the same ordered,
completed-prefix semantics. Queue overflow and pager destruction need explicit
fail-closed thread-resolution policies before USER_PAGER can be advertised.
