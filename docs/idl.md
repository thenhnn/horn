# Horn IDL

Horn IDL describes the control-plane RPC and shared-memory channels between
seL4 components.  It deliberately does **not** describe process creation,
placement, scheduling, or the component graph.  A process manager can create
components and hand them capabilities; the IDL only says what those
capabilities mean and which messages they accept.

The source language is Python.  A schema is an ordinary module using
decorators and type annotations.  The compiler emits Rust and C glue; the
wire/runtime machinery lives in separate native libraries.

## Shape

Three concepts are kept separate:

| Concept | Meaning |
|---|---|
| `record`, `enum`, `flags` | Plain wire values. |
| `iface` | A versioned set of methods.  An interface is a protocol, not an object. |
| `object` | A capability-backed thing implementing one or more interfaces. |
| `ring`, `region` | Shared-memory layouts reached through capabilities. |
| `Cap[T, mode, rights]` | Authority to a kernel object, object, interface, ring, or region. |

An interface is a typed view.  An object is the authority.  A component may
hold a `Cap[Counter]` even though the peer implements `Counter` and
`Control`; the cap exposes only the `Counter` facet.  The wire still
dispatches by method label, so the receiver is not told which facet the
sender used.

```python
@record
class CounterValue:
    value: U64
    generation: U32

@record
class Connection:
    service: Cap[sel4.Endpoint, Copy]
    wake: Cap[sel4.Notification, Borrow]

@iface("net.nhnn.horn.counter", version=(1, 0))
class Counter:
    @method
    def add(self, amount: U64) -> CounterValue: ...

    @method
    def exchange(self, connection: Connection) -> Cap[sel4.Notification, Move]: ...

@object(implements=(Counter, Control))
class CounterService:
    pass
```

## Rust interface views

Each interface emits a server trait and a non-owning `FooReceiver`
implementing `horn_ipc::RpcInterface`. The receiver
keeps only a typed capability; each method takes an explicit `&mut transport`.
Clock provides short public aliases `horn_clock::{Clock, Alarm}`:

```rust
let mut io = ctx.transport()?;
let clock = ctx.interface::<horn_clock::Clock>("clock")?;
let now = clock.now(&mut io)?;
let events = ctx.cap::<horn_ipc::Notification>("events")?;
```

`interface(name)` views an existing immutable startup binding. It does not
create an object, query the server's type, reduce authority, copy a kernel cap,
or reserve a transport. The deployment supplies the interface contract.
Several views can coexist; only the call borrows `io`. Receivers directly
invoke the transport using generated layouts and wire values: labels,
encoding, Copy/Move/Borrow, errors, one-way sends and synchronous blocking
behavior are unchanged.
Neither constructing, copying nor dropping a receiver closes a remote object.

For an explicitly obtained capability, `RpcInterface::from_cap` and
`as_cap` bridge the receiver and `Cap<InterfaceTag>`; retain the capability's
original ownership/cleanup discipline. `ctx.cap::<InterfaceTag>` remains
available; there are no generated transport-bound `Client` types. C wire
bindings are unchanged.

## Payload enums

Integer enums are unchanged. Add decorated, zero-argument variants for a
payload-carrying enum:

```python
@enum
class Outcome:
    @enum.value(0)
    def Empty(): ...

    @enum.value(1, type=CounterValue)
    def Value(): ...

    @enum.value(2, type=Cap[sel4.Notification, Move])
    def Wake(): ...
```

Rust emits `Outcome::{Empty, Value(CounterValue), Wake(Cap<Notification>)}`;
C emits a tag and payload union. Omitting the tag assigns the next integer,
starting at zero; use explicit tags for a stable ABI. Tags use unsigned
`U32` by default and may use an explicit unsigned integer `base`.
A string payload type supports forward references. Recursive variants and
payload enums in shared rings are rejected.

Wire storage is a tag followed by aligned storage for the largest payload.
Unused bytes are zeroed; unknown tags, truncated storage, and nonzero unused
variant bytes are rejected. Only the active payload contributes capabilities.
Layouts reserve the largest variant's capability budget, not their sum, and
active caps occupy consecutive slots. Payloads can nest records, enums,
tuples, arrays, lists, and options. See `examples/idl/tagged.py`.

## Capability transfer

Every capability field names its transfer mode explicitly.  `Cap[T]` is a
schema error.

| Mode | Sender | Receiver |
|---|---|---|
| `Copy` | Keeps its slot. | Gets a new capability. |
| `Move` | Loses its slot after a successful send/reply. | Gets the capability. |
| `Borrow` | Keeps its slot. | May use the capability only until the reply; the runtime deletes it afterwards. |

`Borrow` is only valid in request parameters of a synchronous method.  It
needs a reply boundary, so it is rejected on one-way methods, events, and
result values.  For results use `Copy` (the server keeps its authority) or
`Move` (the server gives it away).

At the seL4 level all three modes put a capability into the IPC buffer; the
kernel copies it into the receiver's CNode.  `Move` is therefore a protocol
convention plus source-slot deletion after a successful send.  A method-level
error does **not** roll back authority.  If a transaction needs rollback,
model it explicitly (return the cap, or use a future revocable handle).

The generator automatically compacts capabilities:

- 0–1 caps are transferred directly in the IPC message.
- More caps are packed into one flat CNode with up to 256 slots.  The layout is
  reserved per method; conditional payloads transfer only their active caps.
  No tree metadata is sent.
- More than 256 caps in one request or result is rejected at schema time.
  Use an opaque `Cap[sel4.CNode]` field when you need a larger, manually
  managed capability set.

Ordinary seL4 IPC receives one non-unwrapped capability, despite having three
extra-capability message registers. Bundles use that receive slot for the
root CNode capability, without a broker round trip.

### Rights

`Cap[T, Move, Rights.READ | Rights.WRITE]` records the minimum rights the
receiver needs.  The runtime maps these to seL4 `CanRead`, `CanWrite`,
`CanGrant`, and `CanGrantReply`.  An endpoint that transfers capabilities
needs `Grant`; a reply endpoint may need `GrantReply`.

### Revocable authority

seL4 has no generic per-capability revoke for arbitrary objects.  A future
`revocable` mode should not pretend otherwise.  Realistic options are:

1. a broker object that proxies calls and can stop forwarding;
2. a dedicated CNode per delegation, revoked with `CNode_Revoke`, with the
   caveat that the CNode capability itself remains;
3. a protocol-level lease with an explicit `revoke` method.

The current schema leaves revocable out rather than baking in misleading
semantics.

## Transport

Transport is a property of the method, not of the value types.

| Transport | Generated shape |
|---|---|
| `sync` | `Call`/`Reply`; one request and one response. |
| `oneway` | `Send`; no reply and no result. |
| `async` | Server-side deferred reply using an MCS saved reply cap.  The client can still use the synchronous `Call` path; a fully non-blocking client needs an explicit reply endpoint/notification. |
| ring | A shared SPSC queue, declared separately with `@ring`. |

The generated Rust client is generic over `horn_ipc::Transport`; the generated
C client calls a `horn_transport_t` vtable.  The Rust seL4 backend implements
`Sel4Transport` and `Sel4Server`; the server returns a `Sel4Reply` handle, so
sync is just `recv` followed immediately by `reply`, while async holds the
handle and replies later.

For `async` methods the Rust generator emits a pair of server helpers:

```rust
async_work_recv_async(server, authority) -> (AsyncWorkArgs, S::Reply)
async_work_reply_async(reply, space, &result)
```

`Sel4Server` implements `AsyncServer`, and `Sel4Reply` implements
`AsyncReply`.  The C backend exposes the same split through
`horn_sel4_recv_deferred`/`horn_sel4_reply_deferred`.  The generated client
still uses synchronous `Call`; a fully non-blocking client needs an explicit
reply endpoint/notification protocol.

Bulk data should not go through RPC.  A driver exports rings and regions;
control methods configure them.  This mirrors sDDF's split between a
synchronous control interface and shared-memory data/metadata regions.

## Wire format

The wire format is a canonical little-endian byte layout inside the seL4
message registers.

- Scalars are 1, 2, 4, or 8 bytes with natural alignment.
- Records and tuples pack fields with alignment and round the total to the
  record alignment.
- `Array[T, N]` is `N` elements with `stride(T) = round_up(sizeof(T), align(T))`.
- `List[T, N]` is a `u64` length followed by `N` fixed slots; unused slots are
  zero/default encoded so the size is constant.
- `Option[T]` is a `u64` presence word followed by `T`.
- `Cap[T, ...]` occupies no data bytes; it consumes one IPC capability slot.
- A method request is the argument record; a response begins with a `u32`
  status word followed by the result value.
- A request or result must fit in 120 message words (960 bytes).  If it does
  not, the schema must explicitly use `Parcel[T]` or a `Region`/`Ring` field;
  there is no silent out-of-line fallback.

The runtime libraries own the `Encoder`, `Decoder`, `WireCaps`, and `Cap`
types.  Generated code contains only schema-specific `Wire` implementations,
label constants, cap paths, client stubs, and dispatch glue.

### Method labels

Labels are hash-derived, as selected for this project:

```
hash = BLAKE2b-64("horn-idl\0" + interface_fqn + "::" + method + "@major.minor")
label = hash & ((1 << 52) - 1)
```

The truncation is required because the seL4 `MessageInfo` label field is 52
bits wide on 64-bit kernels.  Renaming a method or changing its interface
version is an ABI change.  The generator rejects collisions within a schema.
Cross-schema collisions are astronomically unlikely but not impossible; a
future registry can pin labels if independently updated schemas need a
stronger guarantee.

## Objects and dispatch

One endpoint represents one object.  All interfaces implemented by the object
are multiplexed through that endpoint, and the method label selects the
operation.  The server side generates a request enum, a response enum, a
`dispatch` function that calls the handler traits, and object-level helpers
that hide layout selection and reply encoding.

Plain `-> T` methods have infallible Rust handlers returning `T`. Application
failures are declared explicitly:

```python
@method
def allocate(self, size: U64) -> Result[Cap[MemoryObject, Move], MemoryError]: ...
```

The handler returns Rust's `Result<Cap<MemoryObjectTag>, MemoryError>`. The
client returns `Result<Cap<MemoryObjectTag>, CallError<MemoryError>>`, separating
`CallError::Remote(error)` from `CallError::Rpc(error)`. One-way methods cannot
return a `Result`.

`Result[T, E]` specializes a tagged enum: `Ok(T)` is tag 0 and `Err(E)` is tag 1.
Either payload may be `None` (Rust `()`), a record, another result, or a
capability-bearing value. It uses the same maximum-payload storage and active-only
cap transfer as other tagged enums. Results can also appear in fields and
containers. Generated reply codecs preserve schema transfer modes, which Rust's
`Result` and `Cap<T>` types alone do not encode.

Application outcomes use framework status zero followed by the encoded result.
The status word remains separate: a nonzero framework status is
`CallError::Rpc(RpcError::Server(status))`, not a domain error. Unknown result tags
or closed error-enum values are malformed data (`RpcError::InvalidValue`).
In C, the function's `horn_status_t` reports RPC failure; on `HORN_OK`, inspect the
output's `tag` (`HORN_RESULT_OK` / `HORN_RESULT_ERR`) and `value.ok` / `value.err`.
Each fallible method exposes a `horn_<interface>_<method>_result_t` alias.
Unit alternatives have no payload member in C.

The generated server path is:

```rust
let (request, reply) = counter_service_recv(&mut server, reply_authority)?;
let response = counter_service_dispatch(request, &mut handler);
counter_service_reply(reply, &mut server.space, &response)?;
```

`*_recv` selects the request capability layout from the method label via
`AsyncServer::recv_request_dynamic`; `*_reply` selects the result layout from
the response variant.  Receiving is blocking; the receive/reply split only
allows the reply to be deferred.

This keeps dynamic systems cheap: the process manager hands out one endpoint
cap per object, and a component can be given a narrower `Cap[SomeInterface]`
facet without creating another kernel object.

## Parcels and capability layouts

A `Parcel` is the transport envelope for a logical value: encoded data plus
the capabilities reachable from that value, optionally plus shared regions.
The generator computes a `CapLayout` per method from the argument/result type
graph.  The layout is static and both sides know it, so the wire carries at
most one flat CNode capability.

The runtime owns `CapPacker`/`CapUnpacker`, which build and walk the CNode
layout using a `CapSpace` implementation.  The Rust and C runtimes both have
packers/unpackers.  `Sel4CapSpace` retypes CNodes from an Untyped into a
caller-provided root-CNode slot range.

`CapRef` directly names a CNode slot:

```rust
CapRef { cnode, slot, depth, cnode_bits, mode, rights }
```

There is no fixed handle table.  `cnode` is the CNode capability, `slot` is
the slot index, `depth` is the number of bits needed to resolve that slot,
and `cnode_bits` is the size of the CNode object when the ref itself names a
CNode.

A capability unpacked from a received CNode is *reachable* immediately,
but it is not necessarily a CPtr in the receiver's current CSpace.  The
runtime therefore distinguishes:

- `cap_at`: return the direct `CapRef` for a slot;
- `localize`: copy or move that cap into a slot in the caller's root CNode,
  producing a ref that `cptr` can safely pass to seL4;
- `cptr`: returns `NotLocal` for refs that are not in the root CNode.

If the received CNode is installed as the receiver's CSpace root (or a known
sub-CNode), its slots are directly addressable and no `localize` copy is
needed.  For runtime results, `localize` is the general safe path.

Generated request/result layouts use one flat CNode.  `Sel4CapSpace` also
supports manually managed one-level child CNodes, but nested trees are
rejected rather than guessed at; use an opaque CNode or install a subtree as
a CSpace root if you need more depth.

`Parcel[T]` is currently an inline parcel: it is wire-compatible with `T` and
exists to make the transport boundary explicit.  Shared-memory payloads are
expressed with `@region`/`@ring` fields today, for example
`data: Cap[Buffer, Move]`.  Whole-value `Parcel[T, region]` storage is the next
layer; it will serialize `T` into a region and transfer the region cap.

## Rings

`@ring` declares an SPSC queue layout:

```python
@ring(element=U8, capacity=4096)
class ConsoleTx:
    pass
```

The generated Rust alias is `Ring<u8, 4096>`; the C typedef has the same
layout.  A ring is reached through `Cap[ConsoleTx, Move]`, which is a frame
capability.  The receiver maps the frame and casts it to the generated layout.
Ring and region elements must be value types with no capabilities.  Record
elements are rejected for now because generated Rust records are not
`#[repr(C)]`; use scalar, enum, or flags elements until the shared-memory
layout is guaranteed.  Wakeups are separate notification capabilities,
usually grouped in a connection record:

```python
@record
class ConsoleConnection:
    ring: Cap[ConsoleTx, Move]
    wake: Cap[sel4.Notification, Copy]
```

The queue implementation (head/tail, acquire/release ordering) belongs to the
runtime library, not to generated schema glue.

## Generated code

For each schema the compiler emits:

- Rust: `Wire` impls, labels, cap-path constants, interface traits, client
  structs, request/response enums, and object dispatch.
- C: matching structs/typedefs, size macros, encode/decode functions, cap
  path tables, a client function per method, and interface/object vtables.

The runtime lives in:

- `platform/runtime/rust/horn-ipc`: `Wire`, `Encoder`, `Decoder`, `Cap`, `WireCaps`,
  `Ring`, `Region`, and the `Transport` trait.
- `platform/runtime/c/horn-ipc`: the same primitives with a `horn_transport_t` vtable.

A seL4 backend can add `Sel4Transport`/`horn_sel4_transport` without changing
generated code.

## Dynamic-system example

A typical boot path is:

1. root task starts the process manager and memory manager;
2. the process manager starts the device manager;
3. the device manager scans PCIe and starts drivers;
4. drivers export rings/regions and register control objects with the device
   manager;
5. clients look up an object and receive a capability.

The IDL describes steps 4 and 5, not steps 1–3.  For example:

```python
@iface("net.nhnn.horn.registry", version=(1, 0))
class Registry:
    @method
    def lookup(self, name: U32) -> Cap[CounterService, Move]: ...

    @method
    def register(self, name: U32, service: Cap[CounterService, Copy]) -> None: ...
```

The process manager and registry decide *who* gets a cap; the IDL decides
*what* the cap can do.

## Comparison

**FIDL** contributes resource types, explicit handle rights, stable ordinals,
strict/flexible unknown-method behavior, and protocol composition.  Horn IDL
keeps resource types and rights, uses hash labels instead of numeric
ordinals, and currently treats unknown methods as an error on synchronous
calls.  Flexible/open interactions can be added later without changing the
value model.

**sDDF** contributes the control/data split: a small synchronous control path
and shared-memory queues for bulk transfer.  Horn IDL adopts that split and
makes rings/regions first-class declarations rather than hidden conventions.

## Opaque CNodes

The primary bulk-transfer mechanism is an opaque CNode:

```python
@record
class Bootstrap:
    cspace: Cap[sel4.CNode, Move]
    image: Cap[sel4.Frame, Copy]
```

The memory manager decides which slots contain which caps; the IDL only
transfers the CNode cap.  The receiver can install the CNode as its CSpace
root/sub-CNode and use slots directly, or copy/move caps out by index.

`@capset` is optional typed sugar over the same mechanism: it generates slot
constants and `pack`/`unpack` helpers when the layout is fixed.  A capset is
one flat CNode with at most 255 fields.  Both forms put one CNode cap on the
wire.

Rust capsets emit per-field `*_SLOT` constants, `*_SELF_SLOT`, `*_FIRST_FREE_SLOT`
and `*_SIZE_BITS`. Bootstrap code should use these rather than repeat indices.

Capset slot 0 is reserved for a capability to the capset CNode itself.  The
generated `pack` helper populates it by copying the CNode cap to a temporary
root slot and moving that copy into slot 0; seL4 rejects copying a CNode into
itself directly.  The child can then use slot 0 as its CSpace root for cap
receives, `localize`, and CNode operations.  Fields start at slot 1.

## HornTag

A Horn executable emits `horn_ipc::horn_tag!()` once. Its mandatory `.horn`
section contains exactly four magic bytes (`HORN`) and a little-endian u32 ABI
version (currently 1). Generated protocol modules do not emit tags.

The tag identifies the runtime ABI, not a component name, capability layout,
quota or security authority. Those come from the launch contract. Entry point,
segment permissions and TLS use standard ELF metadata. `horn-procd` validates
the tag and ELF at runtime before launching the process.

## Bootstrap demo

`examples/idl/demo` is a child-process demo:

- `schema.py` declares the `Counter` interface, `CounterService` object, and
  `ChildCaps` capset.
- Root-hosted `procd` creates the child's accounted CNode/VSpace/TCB. `root`
  installs bootstrap capabilities in generated named slots, starts the child,
  and calls the generated `CounterReceiver`.
- `child` serves counter requests, signals a wake notification, exercises VM
  and mimalloc, then exits through its lifecycle capability.
- `root/build.rs` and `child/build.rs` run the Python IDL generator and
  `include!` the generated protocol from `OUT_DIR`; no generated protocol is
  checked in.
- `root/build.rs` only embeds the ELF. The root-hosted `horn-procd` validates
  and loads it at runtime, accounts for its resources, and handles explicit exit.

Run it with:

```sh
pytest -q tests/boot/test_idl_demo.py -s
```

## Open questions

- Should fully non-blocking clients use a request ID in the payload, or one
  reply endpoint/notification per in-flight call?
- Do we want a `strict`/`flexible` distinction per interface, or always fail
  unknown synchronous methods?
- How should a server reclaim received `Copy` caps it does not retain?
