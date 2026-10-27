# Root-hosted process manager

`horn-procd` moves resident ELF loading and process ownership out of the demo.
The trusted in-root API creates a suspended process, installs bootstrap caps,
starts it, lists status/usage, changes its RAM limit, stops it and finishes
cleanup. It currently supports one initial TCB and SC per process, on one CPU.
CPU budget/period are trusted launch parameters, not an aggregate CPU account.

Each process has a RAM account shared with its VM service. ELF pages, IPC buffer,
VSpace/page tables, CNode, TCB, SC and later managed/delegated allocations consume
that account. Limits are ceilings, not reservations. See `../vm/README.md` for
allocation rounding and the remaining root-metadata/capacity limits.

## ELF and policy

The loader parses the actual ELF at runtime using the pinned `object` crate.
Only static, little-endian x86-64 executable ELF is supported. The mandatory
`.horn` section is exactly `HORN` followed by the little-endian ABI version 1.
The executable emits `horn_ipc::horn_tag!()` once; generated protocols emit no
ELF metadata. The tag identifies an ABI, not trust, permissions or a cap layout.
Name, quota and bootstrap capabilities come from the trusted launcher.

Entry point, segments and TLS layout remain standard ELF. The current rust-sel4
runtime initializes static TLS; it also supplies the image's stack. The loader
bounds segments, rejects dynamic linking/executable stack requests and checks
W^X at page granularity, including overlapping segments. Temporary writable
copy mappings use NX and are removed before publishing executable mappings.
The dynamic heap window is non-executable. No RW-to-RX/JIT publication API exists.
Debug/release boots check that executing the mapped IPC page produces an NX
instruction-fetch protection fault, then reclaim the stopped process.

## Lifecycle

States: Created, Active, Stopping, Exited, Quarantined. `Active` means started,
not currently scheduled on a CPU. Exit reason is separate from lifecycle state.

The child receives only `net.nhnn.horn.process.lifecycle`: its RAM usage and
`exit(status)`. Successful exit has no reply. The root dispatcher stops the
caller, tears down its VM, then revokes remaining account resources. Failed
cleanup must not be reported as completed or make memory reusable.

Parent launch/control/list are currently Rust APIs inside root, **not** public
spawn/list IPC or a file service. Bootstrap layout remains a launcher/runtime
contract. Fault supervision, multiple threads, delegated-thread activation,
Linux integration, SMP and DMA teardown are not implemented. Granting Untyped
does not grant raw VSpace, ASID allocation or scheduling authority.
