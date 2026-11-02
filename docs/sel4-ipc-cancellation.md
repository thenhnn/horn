# Cancelling IPC on Horn's actual seL4

## Scope

Source audit, not an implemented or tested cancellable RPC facility. No kernel
patch, hardware run or test execution was performed. Paths below refer to
`vendor/sel4/src/`, including Horn's local modifications. Upstream import:
`c6ce4d2a0c334cc9365b2cc41ff0126d75c0ea3c` (see
`vendor/sel4/upstream.json` and `LOCAL_CHANGES.md`).

`platform/sel4/profiles/common.nix` defaults to MCS. The retained built pc99
kernel `/nix/store/2sr8dlkwnsdxhxafqsw332n25qmy2mxz-horn-sel4-pc99-debug-c6ce4d2a0c33/share/sel4/config.json`
confirms `KernelIsMCS=true`, x86_64, MaxNumNodes=64. This identifies an inspected
artifact, not a guarantee about arbitrary future profile overrides.

## Answer

**The kernel already permits an authorized controller to cancel a thread's IPC
by suspending its TCB. It does not provide a timed Call that returns a normal
userspace cancellation error.** A controlled runtime continuation/worker design
is possible in principle using existing authority, but correctness and latency
are not established by this audit.

## Source chain

- `src/object/tcb.c:1775`: `invokeTCB_Suspend` invokes `suspend`.
- `src/kernel/thread.c:72`: `suspend` invokes `cancelIPC`, dequeues scheduling
  state and leaves the target Inactive.
- `src/object/endpoint.c:327`: `cancelIPC` removes BlockedOnSend/Receive targets
  from endpoint queues. BlockedOnReply calls `reply_remove_tcb` on MCS.
- `src/object/reply.c:114`: `reply_remove_tcb` detaches reply and call-stack links.
- `include/object/reply.h:14`: `reply_unlink` clears `replyTCB` and makes the
  target Inactive. `src/kernel/thread.c:134` checks a valid BlockedOnReply target
  before performing reply transfer; a detached reply does not deliver to the
  cancelled caller. This does not make arbitrary reuse of that reply object
  safe against a stale server: reply-object ownership/reuse is a separate issue.
- `src/object/tcb.c:928`: TCB invocation stalls a remotely running target on SMP
  before decoding the operation (`remoteTCBStall`). This is useful kernel
  synchronization, not a replacement for userspace request-state synchronization.

### Why Suspend + Resume is not ETIMEDOUT

`restart()` in `src/kernel/thread.c:91` sets ThreadState_Restart. `activateThread`
restores the restart PC. On x86, `getRestartPC` reads FaultIP
(`src/arch/x86/machine/hardware.c:30`). Simply resuming does not synthesize an
error return and can restart the cancelled invocation.

`invokeTCB_WriteRegisters` (`src/object/tcb.c:2095`) accepts a controlled user
context, updates NextPC from the supplied restart PC, and can restart the target.
Thus a runtime-specific assembly landing pad is mechanically possible. This is
not permission to rewrite arbitrary application PCs or unwind arbitrary C/Rust
frames safely.

### Why a bound notification is insufficient

`src/object/notification.c:64` checks for **BlockedOnReceive** on the bound TCB.
It does not interrupt BlockedOnSend or BlockedOnReply. Timers can signal an event
loop waiting in Recv; they cannot directly turn a blocked Call into a timed call.
MCS timeout faults concern consumed CPU budget, not a wall-clock deadline while
waiting for a server that is not running.

## The important MCS complication

`reply_push` donates the caller's SC when the callee has none and donation is
allowed. `reply_remove_tcb` unlinks the caller, but unlike `reply_pop`, it does not
perform the normal donation back to the caller. **Do not assume Suspend restores
the caller's execution budget.** Recovery must account for the actual SC holder,
possibly nested donation chains, and authority to stop/rebind or replace affected
execution resources. Giving the caller another SC is not automatically a correct
resource-accounting policy.

This directly affects the desired fast path: `src/fastpath/fastpath.c:159` sends a
Call to slowpath if the destination already has an SC. Therefore "give every
server its own SC" simplifies recovery but disables this MCS Call fastpath.
The fastpath also has other preconditions; direct Call is not a performance
promise, especially for cap transfer or large messages.

## Safe design direction to prototype (not adopted implementation)

Retain direct Call for the successful path; let an independent controller handle
exceptional cancellation. Candidate: a bounded pool of transport workers or an
explicitly restricted cancellable call stub. No kernel changes appear necessary
for the elementary suspend/register-control mechanism.

Required invariants:

1. The controller has its own runnable scheduling resources and deadline source;
   it must not depend on the blocked caller or on a potentially hung service.
2. Only the controller holds cancellation authority. An exposed raw TCB cap would
   let arbitrary parties rewrite execution, not merely cancel a request.
3. Each operation has a generation; deadline cancellation cannot target the next
   operation after a previous one completed.
4. Suspend first; resolve completion-vs-cancel races before choosing recovery.
   Only redirect a known runtime boundary. Never kill/unwind a worker while it
   could hold arbitrary application or allocator locks.
5. Recover SC ownership explicitly before attempting to run the caller again.
   Initially restricting donation/nesting may be appropriate, but changes fastpath
   eligibility and must be an explicit choice.
6. Keep request/receive storage alive until the worker is stopped or completed.
   Inspect/clean owned receive slots: caps may already have arrived before timeout.
7. Preserve ownership of transferred caps and shared/DMA backing. Cancellation
   does not undo a cap already delivered or stop remote work.
8. Do not blindly reuse server-held reply objects after caller cancellation.
   A stale server must not be able to reply to a new caller through reused state.
9. Return an uncertain-operation transport outcome when acceptance/execution is
   not known. Do not transparently retry non-idempotent operations.

A worker pool can expose completions to one event loop using local storage and
notifications without a shared page per service connection. It costs TCBs,
stacks, IPC buffers, scheduling resources and controller traffic; pool saturation
needs backpressure. It is a candidate, not a zero-cost universal answer.

## Existing Horn mechanisms vs missing facility

Horn already uses `tcb_suspend` in process termination
(`platform/core/procd/src/lib.rs:534`) and scoped execution control
(`platform/core/procd/src/execution.rs`). Those paths stop/clean up execution;
they do not implement request-aware interruption and normal RPC error return.
Hosted execution register access is not a generic native-thread cancellation API.

Still missing: cancellation registration/generation protocol, controller timing,
assembly continuation or worker discipline, SC recovery policy, receive/reply
ownership cleanup, and a validated completion-vs-timeout race protocol.

Next experimental matrix, if tests are authorized separately: cancel before
receive; after cap transfer; while awaiting reply; after reply before userspace
completion; live-but-hung server; nested donated SC; late reply; slot/reply reuse;
controller and target on different CPUs. Use synthetic peers, not GPU hardware.
