# Capability RPC source audit

## Scope and reproducibility

Read-only source inspection; no implementation changes, hardware runs, tests,
or performance measurements. This is a targeted audit of the listed transport
and CSpace paths, not a proof about every subsystem or all possible frameworks.
Checkouts are research material under ignored `work/rpc-audit/`, not dependencies
or vendored production code. No upstream code was copied into Horn.

| Project | Inspected revision |
| --- | --- |
| Genode | `04b91c7ceea7d36290453a13e980454950d7ec70` |
| CAmkES tool | `23b7818c6c56a35fc6b5bafd642ff47d4226945f` |
| CantripOS (`sparrow-cantrip-full`) | `5fda2e0ecbf8c14450545d83ac677d1b7812be5f` |

## 1. Genode

Sources (paths relative to the revision above):

- [seL4 IPC backend](https://codeberg.org/genodelabs/genode/src/commit/04b91c7ceea7d36290453a13e980454950d7ec70/repos/base-sel4/src/lib/base/ipc.cc)
- [Local CSpace implementation](https://codeberg.org/genodelabs/genode/src/commit/04b91c7ceea7d36290453a13e980454950d7ec70/repos/base-sel4/src/lib/base/capability_space.cc)
- [CSpace metadata and geometry](https://codeberg.org/genodelabs/genode/src/commit/04b91c7ceea7d36290453a13e980454950d7ec70/repos/base-sel4/src/include/base/internal/capability_space_sel4.h)
- [PD setup](https://codeberg.org/genodelabs/genode/src/commit/04b91c7ceea7d36290453a13e980454950d7ec70/repos/base-sel4/src/core/platform_pd.cc)

### Observed

- `ipc_call()` prepares a receive slot, marshals a message and directly invokes
  `seL4_Call` (ipc.cc:339). No deadline or peer-death return path is implemented
  in that function. An invalid local capability logs an error and sleeps forever;
  this is not a recoverable CLOSED result.
- Server dispatch transport uses `seL4_ReplyRecv`; object identity is represented
  by RPC keys/badges rather than a shared page for every RPC object.
- `new_seL4_message()` explicitly asserts at most one cap per message in this
  backend. Do not infer support for arbitrary cap arrays from the generic API.
- Each thread has a receive selector (with a special main-thread bootstrap case).
  Imported caps retain their selectors and cause allocation of a fresh receive
  selector. Lookup can discard a received alias and reuse a known local object.
  The source itself notes limitations in identifying aliases of foreign caps.
- Capability metadata is reference-counted; the last reference deletes the
  kernel cap and returns its selector. CSpace bookkeeping is distinct from the
  typed RPC interface.
- CSpace is **not unbounded**: two-level compile-time geometry, a fixed bitmap
  selector allocator, and a fixed `6 * 1024` local metadata array. Selector
  exhaustion hits an assertion in this path. PD construction creates second-level
  CNodes. This is not a ready-made demand-growing CSpace implementation.

### Takeaway

Useful reference for local handles, receive-slot ownership and separation of
transport from RPC types. Not evidence of transparent, deadline-bounded RPC on
server failure, and not a solution to arbitrary CSpace growth.
The inspected files are AGPLv3; reuse of source requires a licensing decision.

## 2. CAmkES

Sources:

- [RPC connector macros](https://github.com/seL4/camkes-tool/blob/23b7818c6c56a35fc6b5bafd642ff47d4226945f/camkes/templates/rpc-connector.c)
- [Standard client template](https://github.com/seL4/camkes-tool/blob/23b7818c6c56a35fc6b5bafd642ff47d4226945f/camkes/templates/seL4RPCCall-from.template.c)

### Observed

- The IDL/build tooling generates client/server glue; the standard connector's
  `perform_call` macro directly invokes `seL4_Call` (rpc-connector.c:389).
- Normal messages use the per-thread kernel IPC buffer. A configured userspace
  buffer is an alternative, not an unconditional page-per-interface requirement.
- Shared-buffer access can require a lock. The template protects an outstanding
  reply capability before lock operations that receive IPC; nested IPC is a real
  lifecycle concern, not merely serialization syntax.
- The inspected standard call creates MessageInfo with extraCaps=0. Do not claim
  that this connector automatically exposes arbitrary seL4 cap transfer through
  the ordinary IDL. Custom connectors can add transport behavior.
- The standard call macro contains no deadline/cancellation wrapper. This audit
  does not establish what a separate application supervisor might implement.

### Takeaway

Mature reference for separating schema, marshalling, generated stubs and
connector policy. Its standard connector does not supply Horn's desired async,
peer-death-aware transport semantics. Static composition differs from Horn's
intended dynamic process/object model.

## 3. CantripOS

Sources, under `apps/system/components/cantrip-os-common/`:

- [Basic RPC](https://github.com/AmbiML/sparrow-cantrip-full/blob/5fda2e0ecbf8c14450545d83ac677d1b7812be5f/apps/system/components/cantrip-os-common/src/camkes/src/rpc_basic.rs)
- [Shared-buffer RPC](https://github.com/AmbiML/sparrow-cantrip-full/blob/5fda2e0ecbf8c14450545d83ac677d1b7812be5f/apps/system/components/cantrip-os-common/src/camkes/src/rpc_shared.rs)
- [Owned CSpace slot](https://github.com/AmbiML/sparrow-cantrip-full/blob/5fda2e0ecbf8c14450545d83ac677d1b7812be5f/apps/system/components/cantrip-os-common/src/cspace-slot/src/lib.rs)

### Observed

- Two transports are explicitly separate. `rpc_basic` uses the per-thread
  IPCBuffer; `rpc_shared` uses configured client/server shared pages.
- Both client send implementations use `seL4_Call`. Shared buffers do **not** make
  the RPC asynchronous or interruptible.
- Basic RPC supports an optional cap on request/reply. Server receive loops
  install a receive slot, dispatch, and delete the residual received cap. Handlers
  must extract/save caps they intend to keep.
- With a cap in the reply, the implementation splits reply from receive so it
  can clean up the sent cap before another receive overwrites IPC-buffer state.
  In its MCS path, replying uses `seL4_Send` to the reply object. This is not an
  ordinary endpoint send; cancellation analysis must distinguish object types.
- `CSpaceSlot` owns one allocated slot, restores temporary receive paths via a
  guard, and deletes/frees the slot on Drop. Its constructor uses `expect` on
  exhaustion: the pattern is useful, but error policy is not suitable unchanged.
- Service interfaces use Serde/Postcard. This is not a complete portable C/Rust
  IDL generator just because it has typed Rust request enums.
- No timeout or peer-death completion path appears in the inspected send
  functions. No claim is made about all Cantrip subsystems.

### Takeaway

The most compact implementation reference among these for Horn's low-level
Rust slot guards and IPC transport. Particularly useful evidence that basic IPC
and bulk shared data can be separate. The inspected helper files are Apache-2.0;
check individual files before any reuse.

## Implications for Horn

1. Keep generated interfaces independent of transport. An async API does not
   turn an underlying blocking `Call` into nonblocking progress.
2. Put cap-slot allocation, receive-path guards and IPC-buffer discipline in
   libhorn, not handwritten code in every service.
3. Do not promise an unlimited CSpace. Use planned geometry, accounted growth,
   reservation for compound operations and explicit exhaustion errors.
4. Do not require shared pages for small RPCs or each logical object. Existing
   implementations already demonstrate register/IPCBuffer-based control RPC.
5. None of the inspected paths supplies the full desired combination of dynamic
   caps, async C/Rust APIs and bounded waits when a service hangs. That is an
   audit result for these paths, not a claim that no such system exists.
6. The earlier conversational fetch/ACK/retry design is **not adopted**. Reliability,
   duplicate-cap disposal, progress and cancellation were not established.
7. Do not remove direct IPC merely because failure handling is incomplete. The
   next investigation should target interruption of in-flight IPC on Horn's
   actual seL4 configuration and object semantics. Distinguish queued send,
   reply wait, cap transfer already completed, and MCS reply/SC state. Suspension
   alone is not a specified RPC error return; arbitrary register rewriting is not
   a justified cancellation protocol.

A useful proof-of-concept must exercise server failure before receive, after cap
receipt, and before reply; plus a live-but-hung server, deadline races and client
exit. Only after specifying ownership and recovery in those cases should Horn
choose direct-call recovery, managed workers, or a different transport. No
performance guarantees follow from this source audit.
