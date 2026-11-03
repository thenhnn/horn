# Local capability ownership

`Cap<T>` remains the low-level, Copy wire/backend descriptor. `OwnedCap<T>` is
non-Copy/non-Clone and owns exactly one local root-CNode slot. Its Drop deletes
the capability, then returns capacity to the originating slot pool. It retains
that local CSpace context independently of the transport; it neither retains
`&mut io` nor uses a global RPC context.

## Owning RPC replies

Generated Rust receivers decode received Move/Copy capabilities directly into
`OwnedCap<T>`. Applications do not adopt a fresh reply with unsafe code:

```rust
let alarm = clock.alarm(&mut io)?;
alarm.interface::<horn_clock::Alarm>().arm(&mut io, deadline, &signal)?;
drop(alarm); // local capability deletion and slot release, not timerd cancellation
```

Capability-bearing records and payload enums have separate `FooOwned` reply
values. Ownership is recursive through results (including application errors),
options, tuples, arrays, inline parcels and bounded lists. `OwnedList<T, N>`
stores only its active elements in a Vec; inactive wire cells never get dummy
owners. Capability-free values keep their existing types. Raw `Foo`/`Cap<T>`
Wire/server types and the C ABI are unchanged.

The decoder imports each freshly localized slot before proceeding. Previously
accepted owners are dropped if a later field, capability or final reply check
fails. The seL4 transport snapshots reply bytes before any localization/cleanup
invocation and retires its receive slot on both success and error. Borrow is
not an owning transfer and remains invalid in replies.

Runtime, network/HTTP, driver, bootstrap and supervision consumers retain these
owners instead of immediately converting them back to raw descriptors. Hardware
release remains ordered by explicit close/drain acknowledgements, not by Drop.

## Explicit low-level boundaries

Raw adoption is unsafe, like constructing OwnedFd from a raw fd: Copy selectors
cannot prove freshness, exclusivity, or that an old alias will not be reused.
The caller must stop using raw aliases, including for transfer/deletion.
Adoption also checks the root selector, full lookup depth, non-Borrow mode and
this cap-space's allocation ledger. It rejects initial/registered grants,
foreign/child/unwrapped caps, and a second adoption of an already detached slot.
Capacity remains allocated while ledger ownership belongs to the RAII handle.
Other transports/kernel clients sharing the free pool cannot reclaim it.

`into_raw` explicitly restores ownership to the originating transport ledger
without deleting the capability or freeing its slot. The caller resumes raw
cleanup/transfer discipline and must keep that cap-space alive. Aggregate owning
reply values also provide this explicit wire handoff. It is not an ordinary
post-decode step: current uses are outgoing raw server replies and handing a
process-lifetime sink to the static logger. Raw kernel-register/copy/mint APIs
remain explicit low-level boundaries.

Owning outgoing Move arguments are not implemented. Their existing raw transfer
and error semantics, the wire layouts and server dispatch are unchanged.
Initial named grants remain process-lifetime non-owning views, not owned replies.

## Borrowed views

`owner.borrow()` returns `BorrowedCap<'_, T>`. `owner.interface::<Protocol>()`
returns `BorrowedCap<'_, Protocol::Tag>` for explicit protocol selection.
Typed raw, owned and borrowed handles implement the generated `FooClient`
traits directly, so ordinary object calls need no interface view. Its lifetime
is tied to the owner, not to a transport. Views may coexist and use different
explicit transports, but cannot survive moving/dropping the owner.

Generated direct Copy/Borrow capability parameters accept `&impl CapView<T>`;
raw caps, owners and borrowed caps can be passed without relinquishing ownership.
`CapView<T>` is selector access, not another capability or ownership layer.
Explicit `owner.borrow().cast()` selects a different marker while retaining the
borrow, including across crates which independently generate the same IDL tag.

Context's process-lifetime initial handles are non-owning. Copying a borrowed
handle does not erase its lifetime; there is no receiver-to-raw conversion API. Explicit
low-level `CapView::cap_ref` access does not retain ownership and must not be
used after deletion/transfer. An interface cast is not a remote type check or
an authority restriction. Owners and borrowed views are not Send/Sync; this
stage does not add cross-thread invocation support.

## Cleanup and failures

Kernel deletion runs without a slot-ledger/pool lock held. Capacity is returned
only after success. A deletion error is **fail-stop (panic)**, not silent loss or
reuse of an uncertain slot. There is no automatic retry. Ordinary heap callbacks
may run when allocator-backed context metadata is eventually freed; Drop does
not invoke a remote object's close/cancel protocol.

Drop is still a kernel invocation using the calling thread's IPC buffer. Like
other cap-space operations, it must not occur while an undecoded receive needs
that buffer. Decode incoming messages before allowing local handle cleanup.

A remote close, where a protocol has one, remains an explicit synchronous RPC.
A caller can consume an owner, perform the RPC and let Drop do local cleanup on
both Ok and Err. Drop does not run after process abort; helpers which abort on
RPC errors must explicitly drop a consumed owner before reporting that error if
local cleanup is required first. A hung call still prevents reaching cleanup; this does not add
cancellation/timeouts or imply that a transport error means the server did
nothing.

Alarm currently has arm/cancel, **not close**. This change does not invent a
close RPC, free timerd's alarm entry or automatically cancel an armed alarm.
Server-side lifetime/owner cleanup and the existing alarm limit remain separate
work; deleting a client capability does not notify timerd of last-reference loss.

## Validation

Ownership/drop, raw handoff, failed ownership import and nested/partial reply
decode unit sources are compile-only. The IDL ownership fixture compiles owning
reply types and rejects outliving the owner, use after move, cross-thread
owner/view transfer. Generated Rust/C checks
and pc-dev deployment builds cover consumers; Python generator tests run.
No Rust test binaries, QEMU or NVIDIA hardware are run by this validation.
