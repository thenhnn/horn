# Root-hosted process manager

`horn-procd` owns resident ELF processes and their VM services. The root server
composes generated `ProcessHost` and `VmHost` dispatchers. Each process currently
has one initial TCB and SC. Processes receive automatic initial CPU placement;
a same-process thread runtime and musl are not part of this implementation.

## Capability API

- `ProcessFactory.launch(elf, request)` borrows a VM memory capability containing
  the actual ELF, with WRITE seal required. It returns a control capability for
  a **suspended** process. There is no executable-name registry or file service.
- `ProcessFactory.open(id)` recovers control over a child in that factory's scope.
  The authenticated factory grants authority; the numeric ID only selects within
  its owned children. It cannot open unrelated processes.
- `ProcessObserver.list(after)` returns up to four entries and a next cursor
  (zero means end). This separate capability does not authorize launch or control.
- `ProcessControl`: info, start, set_limit, terminate, close. Info reports ID, name,
  state, exit reason and RAM usage. Close retires an exited record and revokes all
  control-cap copies; dropping a cap alone does not retire a record.
- `ProcessLifecycle`: the child's own RAM usage, read-only `placement()`
  (assigned logical CPU and boot CPU count), and exit(status), not parent
  authority. Successful self-exit stops execution and receives no reply.

`Manager::provision_launch` binds factory and observer badges to an owner and a
trusted `LaunchPolicy`: retained-child limit, maximum per-child RAM, VM window,
fixed CPU budget and period. Possessing/delegating that capability spends the
owner's authority, not a claimed caller PID. Children do not automatically get
factory/observer capabilities. All object badges are kernel-authenticated,
globally monotonic and never reused. Observe/control casts do not grant authority.

A launched image receives the generated `ProcessCaps` bootstrap: self CNode, VM
factory, adopted address space, lifecycle, reply object. It gets no raw VSpace,
ASID, scheduling authority or TCB capability. The name is display data (nonempty
UTF-8, at most 64 bytes, no NUL), not a binary selector. Runtime/bootstrap layout
must match the application's contract; the ELF tag is not that layout.

## ELF ownership and RAM

Launch maps sealed source pages read-only and NX into a root-reserved reader
window for parsing and loading; it does not copy the whole file into root's heap.
Only WRITE excludes all managed writers: FUTURE_WRITE alone is rejected. Reader
aliases are removed before returning, including ordinary failure paths. Failed
native cleanup quarantines the source/window and disables further launches,
without optimistic address or backing reuse.

The source remains charged to its existing owner. Loaded ELF/stack/TLS pages,
IPC buffer, VSpace/page tables, CNode, TCB, SC and later managed/delegated memory
are charged to the new process. Process accounts are independent peers under
the system allocator: children neither inherit nor consume their launcher's RAM
limit. The launch grant bounds the chosen per-process limit and retained process
count; limits are ceilings, not reservations.

Only occupied physical RAM is charged: resident backing (mapped or not), whole
delegated Untypeds, kernel objects and retained quarantine. Retype inside an
already charged Untyped is not charged twice. Virtual reservations and absent
demand-zero pages are not physical RAM charges. DMA buffers must count as RAM;
MMIO/BAR ranges are not RAM. DMA allocation/pinning APIs are not implemented yet.

The loaded image is private; closing the source after launch does not kill it.
Failed launches revoke partially created resources before reporting ordinary
failure. Failed cleanup retains charges and reports Quarantined.

The trusted root calls `enable_launch` with an exclusive unmapped reader window;
its translation tables are boot infrastructure, not per-request allocations.
The demo bounds input to a 16 MiB window. A bounded global table retains at most
64 process records; each grant also bounds its retained children. Explicit close
allows repeated launches without reusing process IDs. Root metadata and cap-slot
charges are still not fully prepaid; allocation failure is not comprehensively
hardened. See `../vm/README.md` for allocator/quarantine and DMA limitations.

CPU budget/period are fixed trusted per-child SC parameters, not a hierarchical
aggregate CPU account. The retained-child bound also limits SC grants; this is
not CPU admission control or aggregate CPU-time reporting.

## Initial CPU placement

Root passes all boot-provided SchedControl capabilities and its boot CPU index
to the manager. A pure policy in `src/placement.rs` chooses the least-reserved
logical CPU, with a reservation for the root-hosted server and deterministic
CPU-index tie breaking. Created/suspended children reserve placement too.
Failed creation publishes no reservation. Confirmed stop releases it; exited/
quarantined records do not count. Single-CPU boots simply share that CPU.

The mechanism configures each fresh SC through that CPU's SchedControl before
binding it to the TCB. Launch requests/service definitions contain no CPU number;
neither PIDs nor reported placement grant scheduling authority. There is no
migration after creation. This is initial distribution, **not utilization-based
load balancing**: sleeping services still count, SMT/NUMA topology is not yet
considered, and there is no aggregate CPU admission control.

seL4 schedules runnable threads on each CPU. Root/procd/VM remain one serialized
server, not multiple workers; their Rc/RefCell allocator/account structures are
never concurrently accessed by other service processes. Other processes own
private writable ELF, TLS, IPC buffers, heaps and capability metadata. Blocking
RPC does not make a process multithreaded. Root servers must not spin at a higher
priority than clients sharing their CPU; both demo roots use priority zero.
IRQs retain their existing CPU-0 route; notifications can wake remote TCBs.
Local control deadlines use a fixed CPU's TSC; public timerd time disables the
TSC fast path on SMP and uses HPET/RPC instead.

The IDL boot fixture includes test-only workers and one explicitly root-mapped
shared page. Distinct QEMU APIC IDs, stable assigned CPUs, per-worker heap checks,
and simultaneous progress without syscalls/yields exercise cross-CPU execution.
Backing is retained until both workers are stopped, aliases are unmapped before
reclaim, and charges return to baseline. This does **not** implement general
VM sharing. No probe executable/marker is part of the ordinary desktop demo.

## ELF and execution policy

The pinned `object` crate parses static, little-endian x86-64 executable ELF.
`.horn` must contain exactly `HORN` plus little-endian ABI version 1; emit
`horn_ipc::horn_tag!()` once per executable. The tag grants no trust or permissions.
Entry, segments and TLS use standard ELF metadata; rust-sel4 initializes static
TLS and the image supplies its stack. The loader checks segment bounds, rejects
dynamic linking/executable stack requests and enforces page-granular W^X,
including overlapping segments. Writable copy aliases are NX and disappear before
RX mappings are published. The dynamic VM window never permits executable maps.
No generic RW-to-RX/JIT publication API exists.

## Teardown and validation

States: Created, Active, Stopping, Exited, Quarantined. Active means started,
not currently scheduled. Termination revokes exported TCB authority, suspends the
owned subtree, then cleans child VM/account resources before reclaiming the
owner. Revoked execution caps cannot resume a process during cleanup. Successful
parent exit also retires its descendants and revokes scoped grants. Exited
control records remain inspectable until close or owner teardown. Failed cleanup
never counts as completed exit or reusable RAM.

Boot tests exercise sealed input, reject unsealed/future-write-only and malformed
ELF, quota rollback, five suspended launches without charging the launcher,
record limits/pagination, role separation, scoped open, RAM limits,
terminate/close, source-close independence,
a child's code-7 exit and parent-exit cleanup with a live child. Existing VM,
Untyped, mimalloc and NX fault tests remain. Exit observation currently uses info
or list polling; asynchronous exit notification, fault supervision, malformed-RPC
recovery, kernel-cleanup fault injection, multiple threads, Linux integration,
DMA safety and SMP thread/runtime synchronization remain unimplemented.
