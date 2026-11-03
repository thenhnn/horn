# Local capability ownership

`Cap<T>` remains the low-level, Copy wire/backend descriptor. `OwnedCap<T>` is
non-Copy/non-Clone and owns exactly one local root-CNode slot. Its Drop deletes
the capability, then returns capacity to the originating slot pool. It retains
that local CSpace context independently of the transport; it neither retains
`&mut io` nor uses a global RPC context.

## First consumer

Netd adopts its freshly returned alarm capability:

```rust
let raw = clock.alarm(&mut io)?;
// SAFETY: fresh localized RPC result, exclusively owned, with no further raw use.
let alarm = unsafe { io.space.own(raw)? };
alarm.interface::<horn_clock::Alarm>().arm(&mut io, deadline, &signal)?;
drop(alarm); // local capability deletion and slot release, not timerd cancellation
```

Raw adoption is unsafe, like constructing OwnedFd from a raw fd: Copy selectors
cannot prove freshness, exclusivity, or that an old alias will not be reused.
The caller must stop using raw aliases, including for transfer/deletion.
Adoption also checks the root selector, full lookup depth, non-Borrow mode and
this cap-space's allocation ledger. It rejects initial/registered grants,
foreign/child/unwrapped caps, and a second adoption of an already detached slot.
Capacity remains allocated while ledger ownership belongs to the RAII handle.
Other transports/kernel clients sharing the free pool cannot reclaim it.

Wire decoding and transfer modes are unchanged. Returned Move/Copy values still
use raw Cap in this first step: automatic owned decoding, owning RPC Move
parameters and migration of other consumers are separate work.

## Borrowed views

`owner.borrow()` returns `BorrowedCap<'_, T>`. `owner.interface::<Protocol>()`
returns a generated receiver specialized to a borrowed capability. Its lifetime
is tied to the owner, not to a transport. Views may coexist and use different
explicit transports, but cannot survive moving/dropping the owner.

Existing raw receivers and Context's process-lifetime initial views remain
available. Borrowed receivers do not implement the raw `RpcInterface::as_cap`
bridge: copying a borrowed receiver does not erase its lifetime. Explicit
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
both Ok and Err. A hung call still prevents reaching cleanup; this does not add
cancellation/timeouts or imply that a transport error means the server did
nothing.

Alarm currently has arm/cancel, **not close**. This change does not invent a
close RPC, free timerd's alarm entry or automatically cancel an armed alarm.
Server-side lifetime/owner cleanup and the existing alarm limit remain separate
work; deleting a client capability does not notify timerd of last-reference loss.

## Validation

Ownership/drop and slot-ledger unit sources are compile-only. The IDL ownership
fixture compiles positive examples and rejects outliving the owner, use after
move, cross-thread owner/view transfer and lifetime erasure through as_cap.
No test binaries or QEMU are run by this change's validation.
