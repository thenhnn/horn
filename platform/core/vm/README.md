# VM protocol

`idl/horn_vm.py` is the proposed root-hosted VM service contract. It is not yet
an implemented service. mimalloc is pinned separately in `vendor/mimalloc/`.

## Surface

- `VmFactory`: budget-bound allocation, address spaces, pagers and subaccounts.
- `VmMemory`: backing information, attenuated duplicate handles, close, revoke.
- `VmAddressSpace`: virtual-region reservation and destruction.
- `VmRegion`: prepared/batched native mapping updates and pager binding.
- `VmPager`: notification-driven fault draining and batched resolution.

`VmHost` generates the aggregate dispatcher. Badges/object state, not interface
views, must authorize each request. A factory capability identifies an account;
callers cannot nominate another account by passing an integer.

## Availability

`VmFactory.info().features` advertises implemented optional facilities. Merely
having a generated method does not mean that facility is supported. Unfinished
features return application error `VmError.Unsupported`, not a panic, framework
error or successful dummy object. Batch failure uses `VmBatchError` instead.

The intended first implementation supports resident backing and managed mappings
in a root-provisioned child address space. Demand-zero backing, user pagers,
new address spaces and subaccounts may initially return `Unsupported`. Requests
for unavailable protection modes must fail, not silently weaken protection.

## Mapping batches

`VmRegion.apply(Parcel[List[VmUpdate, 16]])` accepts ordered Prepare, Map,
Protect and Unmap operations. All offsets are relative to that region; backing
offsets are relative to the supplied memory handle. Ranges must be nonempty,
page-aligned, overflow-free and bounded by the relevant objects. Permissions
must fit both the region ceiling and the memory handle's ceiling. Fixed
reservations reject overlaps, including loader/provisioning reservations.

Prepare reserves the translation tables, mapping-cap slots and metadata needed
for its range. Map requires prepared capacity and an unmapped destination;
replacement is explicit Unmap then Map. Unmap retains prepared capacity for
reuse. Resident publication must not allocate backing or wait on a caller.

Success acknowledges completion of every operation, including required TLB
invalidation. Failure reports the completed prefix; subsequent operations were
not attempted. An ordinary failure leaves the failing operation unchanged.
Rollback failure returns Quarantined: the affected object is unusable and its
resources remain retained/charged. No assumption that the failing range is now
unmapped is safe in that case. Batches are not all-or-nothing transactions.

An empty batch succeeds. Unsupported operations return Unsupported at their
position, with the actual completed prefix. Two-way calls are completion
barriers; this protocol does not promise transport pipelining.

Parcels currently use inline storage. The limit of 16 keeps the maximum request
within the current 960-byte IPC payload and 256-capability flat-bundle limits.
There is no unimplemented shared-parcel transport hidden behind this API.

## Fault batches

`create_pager` copies a write-only notification capability. `drain` is
nonblocking and returns up to 16 events. The consumer waits on its notification
and drains until empty. The producer must signal when events become available,
including races with drain/wait, and bound queues/pending-fault resources.

Fault IDs have meaning only under the pager capability that delivered them.
They are one-shot, must reject stale/replayed resolutions, and must never wrap
into an outstanding or previously valid ID. Binding cookies are caller data,
not authority. Resolving a fault cannot escape its bound region, page or
permission ceiling. There is no capability mint/import/delete for every fault.
Fault-reply batches have the same completed-prefix semantics as mapping batches.
Queue overflow and pager destruction require an explicit fail-closed thread
resolution policy before the pager feature can be advertised.

## Lifetimes

A mapping retains backing independently of memory handles. `close` invalidates
that logical handle, not its mappings or independently duplicated handles.
`duplicate` creates an independent handle with an equal or narrower ceiling and
without owner-only revocation authority. `revoke` is owner-authorized and must
invalidate all derived handles and remove their mappings before backing reuse.
Kernel-cap copies are aliases of one logical handle, not independent duplicates.

Destroying a region acknowledges removal of all its mappings and prepared
resources. Address-space destruction requires that the process service first
unbind/stop its threads; otherwise it returns Busy. ASID reclamation and physical
reuse require completed native teardown. Cleanup failure quarantines resources
and preserves their charges. Process-death cleanup must not depend on seL4
notifying the service when the last endpoint-cap copy is deleted.
