# Shared VM backing and immutable publication

Status: shared registry, attenuated handles and cross-VSpace mapping/revoke are
implemented. The native memory interface is v3 with `immutable` publication;
there is no legacy seals API. timerd/netd shared calibration remains a future
consumer. This is not a GPU VRAM or SMP clock fast-path implementation.

## Authority and ownership

**Backing** owns pages, fixed size, allocating FramePool/account, immutable policy
and references from handles, mappings and DMA pins. Resident RAM is charged once.
The destination process's pool must never resolve or release foreign frames:
FramePool checks both allocator and account identity.

**Handle** is a capability-authenticated authority with mapping and control rights.
`delegate(rights)` requires DELEGATE and accepts only a subset of the parent's
READ, WRITE, EXECUTE, DELEGATE, MAKE_IMMUTABLE, REVOKE and DMA rights. A copied cap
aliases the same handle. A derived handle has a distinct badge and explicit close
lifetime. `duplicate(ceiling)` derives mapping-only authority, not control rights.
All handles include READ; dynamic executable mappings remain unsupported.

Delegation depends on the parent handle, not caller or recipient identity. A
process holding someone else's copied cap can create descendants if DELEGATE is
present; those descendants can survive that process. Their allocation/metadata
limit remains rooted in the backing's allocating service, not a guessed sender.
There is no last-cap notification in seL4 and no distributed cap refcount.

**Mapping** belongs to an authorized region in a particular VSpace. The memory
cap must be kernel-unwrapped on the shared VM endpoint. Payload integers never
establish authority. Mapping/protect rights intersect the region, originating
handle and backing policies. Each native mapping has its own root frame-cap slot.

Pages retain their originating handle ceiling and immutable ancestry even after
handle close. Immutable lineage is shared using Rc; the mutable registry is not.
Delegation depth is limited to 32. Backing, regions and derived handles have
per-service limits; root metadata and slots are not fully charged per request.

## Explicit registry

Root Manager owns VM service records and a single Registry of backing, derived
handles and regions. It explicitly loans the complete tables to a service for
one serialized operation, then restores ownership. Per-service state contains
factory/space authority, allocation pool, virtual allocator, Untyped delegations
and outgoing reply cleanup. Each region records its destination VSpace, address
and cap allocator so revocation can unmap foreign aliases correctly.

Root routes received badges by object kind and the backing/region's allocating
service. Badges are globally monotonic and never recycled. Capability transfer
between different server endpoints is not inferred from numeric badge equality.
All managed CPU and DMA policy transitions share the root serialization domain.

## Immutability

`info().immutable` starts false and is global to backing. `make_immutable()`
requires MAKE_IMMUTABLE and is irreversible and idempotent. It returns Busy while
any mapping retains MAYWRITE or a writable DMA pin exists. Protect(READ) retains
MAYWRITE, so callers must unmap such aliases before publication. The operation
never silently tears down or downgrades someone else's mappings.

After publication, all handles (including older writable handles) are denied
writable mappings, write upgrades and device-write pins. Read-only CPU and DMA
access remain permitted. Writable handle ceilings alone do not block publication:
the global backing policy prevents subsequent writes through them.

MMIO/device-backed memory returns Unsupported; poisoned backing returns
Quarantined. Fixed size is a property of resident backing, not a fake resize seal.
There is no FUTURE_WRITE or general Linux memfd compatibility layer. A future
Linux adapter must implement its own file/FD semantics on suitable primitives.
RO attenuation alone is not immutability: other holders can write mutable backing.

ELF and startup-descriptor consumers now require immutable backing. Procd's
synchronous root reader retains the source and excludes interleaved revoke until
the temporary NX mappings and callback finish. This provides scoped availability,
not general fault recovery. Immutable data mapped into another process can still
be revoked, and later access can fault.

## Close, revoke and process cleanup

Close revokes every kernel-cap copy of that handle. It does not revoke children
or mappings. Dropping OwnedCap only deletes a local slot; it does not close a
remote object. Retained descendant ancestry is independent of closed ancestors.

REVOKE permits revoking the invoked handle's subtree, not unrelated delegations.
The operation removes dependent mappings across all VSpaces, revokes descendants,
then releases references. Unrelated sibling handles and mappings remain valid.
Any DMA pin currently makes revoke Busy; VM does not know how to stop devices.
Cleanup failures poison/quarantine rather than permit new mappings or quota reuse.

Process is the VSpace cleanup boundary, not thread. Recipient exit removes its
regions but does not revoke foreign handle aliases merely held in its CSpace.
Allocating-process teardown revokes every backing rooted in that account,
including dependent mappings in other processes. Account destruction follows
confirmed VM cleanup. Outstanding DMA/MMIO authority or uncertain cleanup retains
the account instead. Independent transfers surviving allocating-account teardown
are not implemented.

## DMA

Registry pin identities refer directly to backing, not a process record. The
trusted platform brokers pin tokens to devmgr; ordinary drivers receive IOVAs,
not physical pages. DMA authority and requested access are checked on the handle.
Pinning adds references but does not charge resident RAM again. DMA writers block
immutable publication; immutable backing permits device reads only.

DeviceMapping is deliberately separate: devmgr owns the device assignment,
IOMMU domain, IOVA reservation, hardware mappings and per-device quota. Its common
post-drain cleanup releases platform pin tokens and mapping quota after either
range unmap/invalidate/drain or whole-domain detach/drain. Unpublished rollback
may unpin only when hardware cleanup is known. Unknown outcomes retain resources
or fail-stop. Driver cleanup then performs CPU unmap and memory close.

Automatic DMA revoke would require an explicit asynchronous stop/drain protocol;
it is not introduced by this refactor or hidden in Drop.

## Validation and remaining work

QEMU probes exercise two real processes: shared contents, denied write mapping and
upgrade, delegation/control attenuation, handle close with surviving mapping,
subtree revoke after a leaf was closed, unaffected sibling mapping, global
immutable checks and recipient-exit cleanup. Host policy tests cover DMA access,
immutable writer rejection and pin counters. Existing boot/ELF, HTTP and devmgr
regressions cover integration; builds alone are not runtime validation.

Still needed: failure-injected cleanup and creator-exit cross-VSpace stress,
application revocation fault recovery, full metadata prepayment, and device/DMA
revoke coordination. These tests do not claim GPU or physical-device validation.

## timerd/netd next consumer

Timerd can allocate and initialize a versioned calibration page and delegate READ
without global immutability. Netd maps it read-only in its own region instead of
copying it. Keep RPC fallback on setup failure and the existing invariant-TSC /
single-CPU policy. Updates require atomics and a consistent snapshot protocol.
Revoke/process death can invalidate the mapping; publication is not an availability
promise. Neither timerd/netd integration nor GPU sharing is part of this change.
