# Horn task runtime

Ordinary single-threaded tasks use ABI2 and an explicit initial context:

```rust
horn_runtime::task!(main);
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let service = ctx.cap::<horn_ipc::Endpoint>("service").unwrap();
    // ...
    loop {}
}
```

The macro emits the ABI2 ELF tag, a 256KiB stack, global allocator and entrypoint.
After rust-sel4 initializes stack/TLS/constructors, the runtime validates the
RDI/RSI startup record, installs its IPC buffer and VM-backed mimalloc, installs
an optional logger, and calls `main(ctx)`. Invalid records abort; there is no
legacy entry, guessed IPC address, public numeric bootstrap helper or ambient
`current()`.

## Initial capabilities and IPC

Context is non-Clone and its initial named bindings are immutable for the
process lifetime. Runtime names are `horn.vm`, `horn.address-space`,
`horn.lifecycle`, `horn.reply`, `horn.ready`, and optionally `horn.log`.
Driver names use the `devmgr.` prefix.

`cap(name)` returns an initial capability view, not a managed owning handle or
proof of the kernel object's type. `copy`/`mint` allocate through the explicitly
provided transport; `revoke` preserves an anchor while revoking descendants.
Raw imports/deletion use the IPC cap-space API. Explicitly owned local imports
can use `OwnedCap` RAII; generated RPC replies already return owning capabilities
and recursive owning records/enums. Initial grants are not adoptable owning handles. See
`docs/capability-ownership.md`.

`object::<ObjectTag>(name)` returns a non-owning typed initial object handle.
All its client interfaces are available; import their traits to call methods.
`interface::<I>(name)` remains for explicit protocol selection.

```rust
use horn_clock::ClockInterfaceClient;

let clock = ctx.object::<horn_clock::protocol::ClockHostTag>("clock")?;
let now = clock.now(&mut io)?;
let events = ctx.cap::<horn_ipc::Notification>("events")?;
```

This is a typed view, not object creation, a remote type query, or an authority
restriction. Views do not retain a transport and never perform cleanup in Drop;
calls borrow `&mut io` and invoke the transport with generated wire values.
See `docs/idl.md` for generated receiver names and explicit capability bridges.

Context hands out one application transport and one inbox, with private slot
ranges disjoint from heap callbacks. `inbox("service")` uses `horn.reply`;
`receive()` returns a guard which deletes leftover received capabilities on
drop. Decode once before further IPC, and drop notification receive state before
handling callbacks. No concurrent executor, automatic timeout, reconnect or
same-process SMP support is implied.

`notify_ready()` signals the supervisor; the supervisor alone logs readiness.
`wait(name)` waits on a named notification. Badges coalesce: recheck conditions.

## Shared monotonic clock

On x86-64, `ctx.clock(&mut io, "clock")?` obtains the named clock authority and
returns a `horn_runtime::Clock`. Alternatively, `Clock::new(&ctx, &mut io, cap)`
accepts an explicitly supplied clock capability. No transport or Context is retained.

`clock.now(&mut io)` returns `Result<u64, CallError<ClockError>>`: local shared-page
time when usable, otherwise the clock's RPC. Construction validates and maps the
timerd-owned page read-only/NX. It does not allocate a client-local copy. Acquisition,
validation or ordinary mapping failures fall back to RPC; uncertain native cleanup
is not silently ignored. `clock.is_mapped()` reports mapping setup, not TSC eligibility.

`clock.unmap(&mut io)` explicitly destroys this process's region and drops local
cap copies; subsequent `now` calls remain usable through RPC. Failed unmap retains
resources for retry. Drop alone only deletes local caps, not the server-side region.
The shared memory handle has no CLOSE right and is never remotely closed by clients.
`clock.capability()` exposes the supplied clock cap for alarm calls.

The ABI uses IDL wire fields, including a real BOOL for fast-TSC eligibility.
The provider never changes the published page, so there is no sequence counter.
Invariant-TSC enables UP/SMP fast reads under a platform synchronization assumption;
otherwise RPC is used. Revocation faults are NOT recovered by this fallback.
See `platform/core/clock/README.md`.

## Heap and logging

The bounded 256KiB control heap supports initialization and allocator callbacks,
preventing recursive VM allocation. Ordinary Box/Vec allocations use mimalloc;
there is no dlmalloc fallback on application OOM. Frees use pointer provenance,
so pre-install control allocations can outlive installation.

`task!(main, deferred_heap)` is an explicit **ABI2** allocator-test entry. It
still receives Context and validates the record, but leaves the heap uninstalled.
`ctx.heap()` takes the exclusively reserved VM heap backend once. The IDL
allocator fixture checks zeroing before installing this backend, then exercises
allocation, exhaustion, collection and freeing pre-install objects. Its
application transport remains separate from heap callbacks.

With `horn.log`, runtime reserves a dedicated logging transport before handing
out the application transport. Logging is synchronous, with reply after output.
See `platform/core/log/README.md`; bootstrap/panic/allocator critical paths use
the explicit emergency backend.

Native root tasks retain their seL4 entry/runtime. Calling their own VM server
to grow a synchronous global heap would deadlock. Hosted Linux uses the shared
`libhorn` C startup API instead of this Rust task runtime.

Clock wire-format unit tests execute on the host. Ordinary QEMU HTTP tests
exercise shared clock mapping on UP/SMP, TCG fallback and KVM fast time where
available. Physical-device/NVIDIA execution requires separate authorization.
