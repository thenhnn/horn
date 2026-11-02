# libhorn — initial named context (work in progress)

The initial context substrate is wired through procd, supervisor and the Rust
runtime, with timer as the first ABI2 consumer. This path is build-checked, **not
boot-validated**. Managed handles, CSpace growth, thread-local contexts and
`chamctx` are not implemented.

## Agreed design

- Dependency-free `no_std` Rust core; C header generated separately using
  `rust-cbindgen` from pinned nixpkgs.
- Opt-in ELF ABI=2; preserve ABI=1 for existing executables during migration.
- x86-64 entry: RDI points to a startup record, RSI gives its exact byte length.
  Procd installs these registers only for an explicit ABI2 ELF tag.
- Initial context immutable/resident for process lifetime. Local, printable ASCII
  names, no spaces/NUL, sorted bytewise and unique. `horn.*` reserved for runtime.
- Process-local selectors, not pointers, PIDs or globally meaningful authority.
- Initial caps are retained for process lifetime. Lookup currently yields a
  bootstrap selector for runtime import, **not an owning managed handle**.
- Context names/grants come from deployment, not `@capset` in RPC IDL. Timer
  uses explicit `main(ctx: horn_runtime::Context)`; legacy tasks keep ABI1.

## Draft wire format

`src/startup.rs` is the single layout source. `default.nix` generates
`include/horn/startup.h` using the build host's pinned nixpkgs `rust-cbindgen`,
then compiles `header-check.c` with C11 layout/prototype assertions. Only
stddef/stdint C headers are required. The Rust crate has no VM, seL4, heap or
Cargo build dependencies; header generation is independent of its target graph.

Build both artifacts:

```sh
nix-build nix -A rust.libhornHeaders -A rust.workspace.members.libhorn --no-out-link
```

All wire integers are little endian. Header is96 bytes, entries24 bytes each.
Entry string offsets are relative to a strings region; other offsets are
relative to record start. No interior VA pointers. Parser uses field-wise reads,
including unaligned records, and checks arithmetic, regions, ordering, names,
selectors and the initial free-slot range. Encoder takes caller storage and
sorted entries; it allocates nothing. No fixed entry-count/record-size maximum
is imposed beyond representability and supplied storage.

Initial runtime parameters explicitly contain IPC-buffer VA, root selector,
flat initial CNode radix and an empty slot interval. The interval is a bootstrap
seed, not a prohibition on later growth. The current draft requires initial caps
and the root selector below that interval; this is a loader/runtime prefix
convention, not a general restriction on seL4 CSpaces. Aliases between names are
allowed and must not become duplicate deletion ownership during future import.

`horn_startup_validate` and `horn_startup_lookup` are low-level C entrypoints.
They validate bytes, not whether the caller supplied a readable address or whether
an actual kernel cap has the advertised role. Pointer validity/immutability are
caller obligations; trusted provisioning and runtime type handling remain needed.
The caller must keep record memory alive for the duration of a borrowed view.

## Provisioning descriptor

`bindings` encodes a separate names/selectors-only descriptor: eight bytes
`HORNBND2`, then repeated little-endian u64 selector, u64 name-byte-length and
printable ASCII name bytes. Names are sorted/unique; selectors are nonzero.
There are no runtime addresses, padding or fixed name/count limits. The
`ProcessControl.startup_context` request passes one borrowed sealed-memory cap
and an exact descriptor byte length (the memory object itself is page-rounded).

Procd constructs the final startup record using the child's actual IPC address,
CNode configuration and highest installed initial slot. It maps accounted
backing RO/NX after the resident image/IPC page and below the VM window. Temporary
writable aliases disappear before publication. Once allocation starts, failed
provisioning cannot be retried in that process; retained charged resources are
released through normal process teardown. A sealed-source unmap failure
invalidates publication and quarantines the reader window. ABI2 start requires a
successfully published record. Initial installs freeze once provisioning starts.

This is not kernel-role authentication: initial cap types/roles are a trusted
deployment contract. A name/selector record cannot create capability authority.
The current runtime still partitions a flat initial CNode into fixed bootstrap,
application and heap pools; lazy tree growth remains separate work.

## Validation so far

- Host `cargo check --offline -p libhorn` passes.
- Test sources compile with `cargo check --offline -p libhorn --tests`; **not run**.
- Generated C header passes C11 syntax and layout/prototype assertions.
- Cargo.lock and crate2nix graphs regenerated using pinned toolchain.
- Nix seL4 library build and separate header generation/C11 checks pass.
- Full pc-dev devmgr root build (mixed ABI1/ABI2) passes.
- pc-release build is blocked by unchanged `horn-vm` calls to
  `sel4::debug_println!`, unavailable with that kernel configuration. No unrelated
  release-logging fixes are included here.
- Procd test sources and Python compiler tests are compile-checked, not run.
- The earlier Cargo build-dependency experiment exposed crate2nix host/target
  feature-resolution trouble in rustix/linux-raw-sys. Moving header generation
  outside Cargo removes that dependency chain; it does not fix the general
  resolver limitation.
- No boot, hardware run or ASLR work performed.

Next: separately authorized boot validation, then broaden runtime/handle work.
Do not infer cancellation, owning handles, concurrent contexts or ASLR from this
initial bootstrap path.
