# Shared VM backing and immutable publication (proposal)

Status: design for discussion, not implemented. First consumer: timerd's
immutable calibration page, shared read-only with netd. This is not an SMP clock
fast-path proposal or a GPU VRAM implementation.

## Today

Each process has a `horn_vm::service::Service`: it holds one VSpace and its
own memory and region tables. A `VmMemory` badge resolves in that service's
memory table. A region applies a borrowed same-endpoint capability, so a
foreign service's badge is not a transferable backing reference today. Frame
caps are copied per mapping, but the native map targets the service's VSpace.
`duplicate` exists in IDL but returns Unsupported. `netd` allocates and seals a
local copy of the clock calibration, not shared physical memory.

Seals today live on the per-service memory record. WRITE is rejected while any
page has `may_write` or a DMA writer exists; a read-only page with a writable
ceiling retains `may_write` even after Protect(READ). FUTURE_WRITE prevents new
writable mappings but preserves old mapping authority. SEAL freezes the seal
set; GROW/SHRINK are currently inert because memory is fixed-size. Existing
launch of ELF sources relies on WRITE as a strong immutable-content guarantee.

## Goals and non-goals

- Pass a VM object as a capability to another process, map the same resident
  bytes in its authorized VSpace with independently chosen range and offset,
  and attenuate authority. The holder cannot choose an arbitrary VSpace.
- Publish genuinely immutable data for timerd and the existing ELF consumer.
- Make ownership, cleanup and accounting explicit across process exit and
  failed kernel operations; never reuse uncertain backing or mapping slots.
- Do not export managed raw frame caps to untrusted clients. Do not assume the
  endpoint badge or an integer ID is an authority when supplied as data.
- Defer page faults, resize, COW, executable publication, GPU VRAM/BAR, and
  cross-device cache-coherence policy. Do not imply that READ-only CPU mappings
  prevent DMA or device firmware from writing.

## Three different things

**Backing** owns physical pages, size, allocation charge, global policy and all
writer/mapping/pin references. It outlives the process that created it whenever
references remain. It has no client-chosen integer identifier as authority.

**Handle** is a distinct capability-authenticated authority over backing. It
has a non-increasing access ceiling and explicit control rights. A copy of the
same endpoint cap aliases the same handle, not a new independently closable
handle. A derived handle is a new record and badge, with a separate close
lifetime. Receiving a handle must not imply authority to seal, revoke globally,
resize, pin DMA, or create a more powerful handle. In particular, a READ mapping
ceiling alone does not restrict destructive RPC methods: operation rights must
be enforced too.

**Mapping** belongs to a region in a specific address space. It holds a backing
reference independent of the issuing handle. Effective map and upgrade rights
are bounded by the handle, region and backing policies. Each native alias has a
distinct frame cap and tracked VSpace, range, and source offset. Deleting an RPC
cap or closing a handle does not silently unmap an existing mapping.

The authority boundary between independent handles, descendants and revocation
must be represented explicitly. An endpoint-cap Copy duplicates a reference to
one handle; `duplicate` creates a new handle. Transfer of a handle via RPC
requires an appropriate transferable cap, not an unwrapped badge.

## Server structure and authentication

The current per-process table cannot recognize a foreign process's memory.
Proposed architecture: a root-owned shared backing/handle registry with a single
serialization domain for backing policy, handles, mappings and DMA references;
per-process VM state retains VSpace, virtual allocation and regions. Refactor
both under one root VM authority, or an equivalent centrally routed service.
The existing `VmRegion.apply` same-endpoint unwrapping requires memory and
region authorities to resolve at the same seL4 endpoint. Merely putting a shared
map behind today's independent dispatch is insufficient: dispatch must route
badges to the right region and handle while unwrapping is authenticated by the
kernel. Confirm actual endpoint topology and dispatch before choosing the
refactor. Badge allocation must be globally unique within that endpoint and
never recycled while stale caps can exist. All lookup accepts only a kernel
unwrapped badge, with kind and state checked; no raw badge from payload.

Mapping requires both an authorized destination region and a live compatible
memory handle. Serialize Map, Protect, Unmap, publication, revocation and DMA
pin transitions against the shared backing. Validate ranges, ceilings and seals
before native changes. Preserve completed-prefix semantics and quarantine on
uncertain rollback. Never create a capability path which bypasses this accounting.

## Seals: assessment and recommendation

The existing model has a useful **strong invariant**: WRITE means bytes cannot
change through any mediated CPU alias or DMA writer. That invariant, rather
than Linux flag compatibility, is the reason to retain a backing-global,
monotonic immutable state. Its guarantee is only valid when *every* writer is
mediated; MMIO and externally mutable device memory must reject publication.
Read-only handles alone are not immutability: another holder could still write.

However the Linux-style five-flag surface is not a good foundation for Horn:
GROW/SHRINK describe an unsupported operation, SEAL is a policy lock unrelated
to content, and FUTURE_WRITE has subtle semantics and does not mean immutable.
A writable ceiling surviving Protect(READ) as `may_write` is conservative and
correct for preventing later upgrades, but it is easy to confuse with active
writes. Shared memory multiplies this confusion. Also today every holder of a
memory badge can request seals, close or revoke; a read-only derived handle must
not inherit those global mutation powers.

**Proposed native API direction:** replace the general seal vocabulary with an
explicit one-way `publish_immutable` operation on a separately authorized control
handle. It succeeds only if no CPU mapping has retained write-upgrade authority,
no active or pending device writer/pin or other write path exists, and backing
is eligible and healthy. Otherwise return Busy/Unsupported/Quarantined without
changing state. After acknowledgement, reject *all* future writable maps,
upgrades, writable DMA pins and other writes through every handle, including
handles created earlier; immutable is irreversible and globally visible. Never
implicitly tear down or downgrade someone else's mapping. Existing read-only
mappings may remain if they have no retained write authority. Serialize the
transition with all mapping and pin paths. Future internal writers and external
exports must honor the same invariant or make the operation unavailable.

Fixed size should be a property of the backing type, not a pretend seal.
Attenuation of a handle is distinct from global publication. Reconsider an
explicit FUTURE_WRITE-like transition only with a concrete consumer; do not
claim it makes content immutable. If a Linux memfd adapter is later needed, it
must implement Linux semantics, including FUTURE_WRITE and seal locking, on top
of suitable VM primitives; it must not pretend the native API already does so.
This recommendation changes the existing IDL and ELF consumer and should not
be silently made as part of sharing. It requires agreement and migration. An
alternative is to keep the current flags and strengthen their global enforcement,
but that retains Linux-specific complexity in the core API.

## Lifetime, cleanup and accounting

Proposed defaults to review:

- Physical pages remain charged once to the originating account; virtual
  reservations, mapping slots and page-table resources are charged to the
  mapping process where the accounting mechanism supports it. Metadata limits
  must be bounded; today's root metadata is not fully charged. Expose retained
  charges even after process exit. Account lifetime therefore cannot equal
  process-record lifetime. No implicit reparenting or double charging.
- A handle has explicit close. Close revokes all kernel-cap copies of that
  handle and releases its handle reference, but existing maps retain backing.
  Dropping a local `OwnedCap` deletes only its local slot, not remote state;
  seL4 supplies no last-cap notification. Server records need explicit close or
  lifecycle tracking, and passing capabilities to third parties complicates
  attributing them to a process. Design an explicit delegation/cleanup policy;
  don't assume an RPC sender's identity authenticates the current cap holder.
- Process cleanup removes that process's regions only after stopping execution
  and acknowledged unmaps. Other processes' mappings continue. An exiting
  creator's charge and backing remain while other references exist. Forced
  unmap of another live process needs explicit authority and defined fault
  behavior; it is not ordinary close.
- Separate per-handle close from global or subtree revoke. The latter must
  explicitly define descendants and mappings, deny new operations while
  revocation runs, complete native unmaps before returning, and cope with DMA
  drain/invalidation first. Failures quarantine affected authority and backing;
  never reuse slots or return quota on uncertain cleanup. Initial sharing does
  not require exposing destructive global revoke to ordinary recipients.
- Backing is collected only after every handle, mapping, trusted export and DMA
  pin is gone and kernel cleanup is acknowledged. A successful IPC reply or
  local-cap Drop alone does not establish this condition.

A retention/DoS policy remains necessary: uncooperative holders can keep a
creator's charge alive, and abrupt death plus transferable aliases makes exact
handle reclamation nontrivial. Limits and supervisor policy must bound this;
claims of automatic reclamation are premature.

## timerd first use

1. Allocate one resident page in timerd's account, map writable, write the
   versioned calibration structure. Unmap/retire every write-capable mapping.
2. Publish immutable and retain a control handle. Return or transfer an
   attenuated read-only handle via a clock RPC; specify cap transfer and reply
   cleanup so each recipient obtains a usable independent handle. The page format
   and size must be versioned/validated; do not rely on Rust layout by accident.
3. netd reserves its own region, maps the shared page read-only/NX and retains
   the mapping and required handle. On any unsupported or unsuccessful setup,
   use `now()` RPC. Remove the local-copy allocation path once the replacement
   is implemented. Preserve existing invariant-TSC and single-CPU policy.
4. Immutable page does not propagate later clock-mode changes. CPU migration,
   live migration and SMP fast time need a separately designed mutable shared
   protocol or a different invalidation strategy. No implicit lifetime guarantee
   arises merely from holding the pointer.

## Validation and rollout

Start with unit tests for policy, handle rights, immutable transition and
reference accounting. Integration tests must use **two actual VSpaces**: verify
same bytes and offsets, denied write/upgrade from attenuated handles, global
immutable checks including another process's may-write alias and DMA writer,
close versus retained mapping, exit/revoke and cleanup failures. Verify creator
exit, recipient exit, alias copies, partial failures and quota retention. Add
negative authentication tests for foreign endpoints, wrong kinds and forged
badges. Check immutable publication against actual frame writes, not just
metadata. Then integrate timerd/netd and test RPC fallback. Builds and host
unit tests are not proof of seL4 runtime behavior; QEMU runs require user control.

Open decisions before implementation: native immutable operation versus
retaining seals; authority/right representation and delegation cleanup;
revocation scope; endpoint refactor and metadata accounting; whether indefinite
creator-charge retention is acceptable or requires an explicit transfer policy.
