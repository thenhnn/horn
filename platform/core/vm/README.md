# Root-hosted VM

`idl/horn_vm.py` defines the full capability-addressed VM protocol.
`src/service.rs` implements the resident subset; `src/sel4.rs` owns the kernel
objects and reusable, independently revocable backing pool. The old memory RPC
has been removed. The demo provisions an ordinary child with factory and adopted
address-space endpoint capabilities, not frame, Untyped or VSpace capabilities.

## Surface and availability

- `VmFactory`: account information, backing, address spaces, pagers, subaccounts.
- `VmMemory`: backing information, duplicate, close, revoke.
- `VmAddressSpace`: region reservation and destruction.
- `VmRegion`: information, prepared mapping batches, pager binding, destruction.
- `VmPager`: notification-driven fault draining and resolution batches.

`VmHost` generates dispatch. Kernel-authenticated endpoint badges and object
state authorize operations; interface casts do not grant authority. Badge values
never wrap or get reused. An account cannot be selected using a client integer.

The implementation advertises RESIDENT and PROTECTION. It supports Anywhere
reservations inside one loader-provisioned window, prepared maps, aliases,
read/write protection changes, acknowledged unmaps, close/revoke and region
teardown. The loader installs translation tables before adopting the VSpace.

Demand-zero, fixed placement, new spaces, duplicate handles, subaccounts and
pagers return typed Unsupported on authorized requests. Adopted-space destruction
returns Busy: the service does not own thread shutdown/ASID reclamation. Invalid
pager capabilities are denied, not accepted as dummy pager objects.

Only readable mappings are supported. Write-plus-execute mappings **and ceilings**
are rejected. x86 non-executable mappings use the vendored kernel's ExecuteNever
attribute; missing NX support fails closed. RW-to-RX code publication and its
alias policy are not implemented. Permission changes have boot coverage, but a
hardware execution-fault test is still required.

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

`limit_bytes` is the fixed backing pool size. `charged_bytes` counts unavailable
pool pages, including quarantine; `quarantined_bytes` counts failed physical
reclamation and backing implicated in poisoned regions. Returning pages to this
pool is **not** returning its reserved Untyped memory to a system-wide allocator.
These counters do not account for all metadata, translation tables or cap slots.
The demo uses separate fixed object, CSpace, virtual-window and control-heap
budgets. Metadata allocation can still abort on control-heap exhaustion; this is
not a production resource-prepayment/accounting implementation.

Automatic process-death cleanup, malformed-request recovery, untrusted CNode
recovery, SMP scheduling and failure-injected kernel cleanup are unfinished. seL4 does not notify the service
when the last endpoint-cap alias disappears. This service must not be used as a
claim of complete process lifecycle, DMA isolation or Linux VM integration.

## Deferred fault contract

Pager creation copies a write-only notification. Drain is nonblocking, bounded
to 16 events and followed by notification waits; producer wakeups must cover
wait/drain races. Fault IDs are pager-scoped, one-shot values, not per-fault caps.
Stale/replayed IDs and ID wrap must be rejected. Cookies are data, not authority.
Resolution uses one optional borrowed backing capability and the same ordered,
completed-prefix semantics. Queue overflow and pager destruction need explicit
fail-closed thread-resolution policies before USER_PAGER can be advertised.
