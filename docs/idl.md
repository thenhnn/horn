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

@iface("gpos.counter", version=(1, 0))
class Counter:
    @method
    def add(self, amount: U64) -> CounterValue: ...

    @method
    def exchange(self, connection: Connection) -> Cap[sel4.Notification, Move]: ...

@object(implements=(Counter, Control))
class CounterService:
    pass
```

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

- 0–3 caps are transferred directly in the IPC message.
- More caps are packed into one to three root CNodes; each root may contain
  child CNodes.  The layout is static per method, so no tree metadata is sent.
- The planner prefers the fewest CNodes that fit: one CNode up to the fanout,
  then two or three roots, then a tree when more than `3 * fanout` caps are
  present.

This works around seL4's per-message limit (`seL4_MsgMaxExtraCaps = 3`)
without a broker round trip.  The root CNode capabilities are the only caps
placed in the IPC message.

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
handle and replies later.  The C backend exposes the same split through
`horn_sel4_recv`/`horn_sel4_reply` and will gain a deferred reply handle.

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

The runtime libraries own the `Encoder`, `Decoder`, `CapList`, and `Cap`
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
operation.  The server side generates a request enum, a response enum, and a
`dispatch` function that calls the handler traits.

This keeps dynamic systems cheap: the process manager hands out one endpoint
cap per object, and a component can be given a narrower `Cap[SomeInterface]`
facet without creating another kernel object.

## Parcels and capability layouts

A `Parcel` is the transport envelope for a logical value: encoded data plus
the capabilities reachable from that value, optionally plus shared regions.
The generator computes a `CapLayout` per method from the argument/result type
graph.  The layout is static and both sides know it, so the wire only carries
the root CNode capabilities.

The runtime owns `CapPacker`/`CapUnpacker`, which build and walk the CNode
tree using a `CapSpace` implementation.  The Rust and C runtimes both have
packers/unpackers.  `Sel4CapSpace` retypes CNodes from an Untyped into a
caller-provided root-CNode slot range and stores opaque capability handles in
a fixed table; a production allocator can replace it without changing
generated glue.

A capability unpacked from a received CNode tree is *reachable* immediately,
but it is not necessarily a CPtr in the receiver's current CSpace.  The
runtime therefore distinguishes:

- `cap_at`: obtain a handle to a cap inside a CNode;
- `localize`: copy or move that cap into a slot in the caller's root CNode,
  producing a handle that `cptr` can safely pass to seL4;
- `cptr`: returns `NotLocal` for handles that are not in the root CNode.

If the received CNode is installed as the receiver's CSpace root (or a known
sub-CNode), its slots are directly addressable and no `localize` copy is
needed.  For runtime results, `localize` is the general safe path.  `cptr`
returns `NotLocal` rather than a misleading slot number for caps that are not
in the caller's root CNode.

The current `Sel4CapSpace` assumes CNode capabilities are addressable from
the root CNode.  One-level layouts (up to three root CNodes) work directly;
deeper child-CNode trees need either installation as a sub-CNode or a
recursive localize step.

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
Ring elements must be value types with no capabilities.  Wakeups are separate
notification capabilities, usually grouped in a connection record:

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

- `runtime/rust/horn-ipc`: `Wire`, `Encoder`, `Decoder`, `Cap`, `CapList`,
  `Ring`, `Region`, and the `Transport` trait.
- `runtime/c/horn-ipc`: the same primitives with a `horn_transport_t` vtable.

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
@iface("gpos.registry", version=(1, 0))
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
constants and `pack`/`unpack` helpers when the layout is fixed.  Both forms
put one CNode cap on the wire.

Capset slot 0 is reserved for a capability to the capset CNode itself.  The
generated `pack` helper populates it by copying the CNode cap to a temporary
root slot and moving that copy into slot 0; seL4 rejects copying a CNode into
itself directly.  The child can then use slot 0 as its CSpace root for cap
receives, `localize`, and CNode operations.  Fields start at slot 1.

## HornTag

A Horn component ELF carries a `.horn` section with a `HornTag`:

```c
struct HornTag {
    char magic[4];      // "HORN"
    uint32_t version;
    char component[32];
    uint64_t layout_hash; // expected capset/CSpace layout
};
```

The loader reads the section with the `object` crate before mapping the
image.  This is not a substitute for the ELF OSABI field: OSABI is coarse,
often left as `SYSV`/`NONE` for bare-metal binaries, and cannot carry a capset
layout hash.  The tag is the place for the Horn ABI version, component name,
and expected CSpace layout.

The demo currently verifies magic and version at build time; the layout hash
field is reserved for the next step, where the generator will hash the
`@capset` layout and the loader will compare it with the packed CNode.

## Bootstrap demo

`examples/idl/demo` is a child-process demo:

- `schema.py` declares the `Counter` interface, `CounterService` object, and
  `ChildCaps` capset.
- `root` packs the capset into a fresh CNode, creates the child VSpace/TCB,
  starts the child, and calls the generated `CounterClient`.
- `child` serves one request with the generated `CounterService` handler and
  signals a wake notification.
- `root/build.rs` uses the `object` crate to parse the child ELF, verify the
  `.horn` tag, and emit a `ChildImage` table of segments.  The root task does
  not parse ELF at runtime.

Run it with:

```sh
pytest -q tests/boot/test_idl_demo.py -s
```

## Open questions

- Should fully non-blocking clients use a request ID in the payload, or one
  reply endpoint/notification per in-flight call?
- Should result status be a `u32` word for every response, or only for
  fallible methods?
- Do we want a `strict`/`flexible` distinction per interface, or always fail
  unknown synchronous methods?
- How should a server reclaim received `Copy` caps it does not retain?
