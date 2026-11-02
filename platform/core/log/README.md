# Synchronous logging

First vertical slice: a shared `logd` service and an automatically installed
`log` facade for ABI2 tasks granted `horn.log`. Timer is the first migrated
consumer. Legacy tasks and bootstrap diagnostics still use their old output;
this change does **not** claim every existing message is migrated.

## Contract

- One whole record per blocking `seL4_Call`, no caps in the payload, no ring or
  queue, no retries. Logger serializes complete records.
- Success reply **after** the output backend completes, not after enqueueing.
  The current backend is seL4 DebugPutChar, requiring a printing-enabled kernel.
  This means bytes submitted synchronously to that backend, not durable storage
  or proof a human saw them. An output error is returned, not acknowledged.
- Target limit 96 UTF-8 bytes, message limit 768 UTF-8 bytes, determined by the
  inline IDL record within seL4's message size. Formatting uses stack buffers,
  no heap in the formatter/client/rendering. User Display implementations are
  responsible for their own allocation/side effects.
- Overflow returns TargetTooLong/MessageTooLong, never silent truncation or a
  successful partial record. `emit` exposes the Result; `log` macros cannot
  return errors, so facade failures produce a direct EMERGENCY diagnostic.
- Always-colored levels/targets through owo-colors, no color toggle. Invalid
  UTF-8 fails before output; terminal controls are escaped, multiline messages
  visibly indented. Process name comes from an immutable sink binding, selected
  by the kernel-supplied badge. Target remains caller-supplied module metadata.
- A stalled/dead logger may block a caller indefinitely: no universal timeout.
  Never use normal logging while holding a resource the logger needs.
- `emergency(format_args!(...))` is direct, no-RPC output for bootstrap, panic,
  allocator/pager critical paths and logger failures. Existing panic machinery
  remains direct. Emergency/kernel/legacy output can still interleave with
  normal logs until all ordinary writers are migrated.
- Default facade level is DEBUG, with INFO for `sel4` / `sel4_*` targets.
  Their warnings/errors remain visible; syscall tracing is not implicitly
  enabled. The per-target filter is checked in both enabled() and log().
  Explicit emit() bypasses facade filtering; invocation traces with an active
  IPC-buffer borrow cannot issue RPC and require the emergency path.
- Single-threaded runtime only. A dedicated transport prevents logging from
  overwriting application/heap receive slots. Reentrant formatting/logging is
  rejected rather than deadlocking on the local logger guard. This is not a
  multi-threaded logger API.

## Integration

```nix
services.definitions.timerd = {
  abi = 2;
  logging = "/services/log";
  # existing image, objects, dependencies...
};
```

`LogFactory.create_sink(name)` returns a write-only, uniquely badged sink cap.
The supervisor invokes it with the deployment process name and installs only
that sink in the child. A sink cannot invoke the factory or change its label.
Factory export uses `supervisorOnly = true`: public namespace list/lookup/open
and scoped binding resolution cannot expose it, even through a root directory
grant. Trusted deployment can explicitly delegate factory authority; ordinary
logging grants never do so. Bindings/anchors live until logd teardown, without
badge reuse; capacity exhaustion is an explicit error.

The compiler injects a logger dependency and private `horn.log` name. Runtime
initializes the facade after heap/IPC setup and before `main(ctx)`; application
code uses `horn_runtime::logging::info!(target: "timer", "ready")`. Logd has
`logging = null` and no dependency on itself. Bootstrap/root logging migration
must avoid a supervisor/logger dependency cycle.

Unit tests cover formatting bounds, invalid UTF-8, control escaping, colors,
output failure and wire bounds. Sources are compile-checked, not executed.
