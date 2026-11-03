# ABI2-only startup

All managed tasks now use ABI2: supervisor, devmgr, logd, timerd, netd, virtio
drivers, shell/HTTP, NVIDIA, hosted Linux, IDL examples, devmgr-probe and SMP
workers. Native seL4 root tasks retain their own entry/runtime.

Procd accepts only the exact ABI2 ELF tag and requires a provisioned startup
record before start. There is no ABI1 parser branch, guessed IPC-buffer address,
numeric task bootstrap API, `vm:` task entry, or service/driver ABI selector.
Service configuration rejects obsolete `abi`, `bootstrap` and `capset` options.
Clock-page protocol version fields are unrelated to executable startup.

## Authority and provisioning

Rust tasks receive explicit `main(ctx)`. Initial named capabilities are immutable
process-lifetime views; there is no ambient current context. Runtime names use
`horn.`, driver grants use `devmgr.`. Remaining numeric placement constants/
capsets belong to trusted loaders, not application runtime contracts.

All grants are installed before the sealed names/selectors descriptor. Procd
derives IPC address, root selector and free CSpace bounds, installs accounted
RO/NX backing and freezes initial installs once allocation starts. Supervisor
and devmgr share descriptor upload/provisioning mechanics through Bootstrap.

Devmgr derives its own authenticated sink through supervisor's boot-owner-only
`core_log_sink()` RPC. Supervisor activates dependency-free logd before serving
it; no factory capability or caller-selected label is exposed. Application
runtime authority cannot call this method. Supervisor alone logs service READY
after notification.

The IDL allocator fixture uses `task!(main, deferred_heap)`: still ABI2, with
a checked Context, but explicitly takes/installs the reserved heap backend after
testing control-heap provenance. Application and heap transports remain disjoint.
Its launched child cannot start until its context is installed.

Hosted Linux preserves RDI/RSI at entry and uses the shared libhorn C API.
Receive/alarm slots come from the record's free interval. Linux reserves both
the real IPC-buffer page and startup record before admitting allocator memory.

NVIDIA retains IRQ/STOP ordering, shutdown-before-owner-ACK and DMA quarantine.
Only startup/context plumbing changes; hardware programming, diagnostics and
experiments are not expanded.

## Validation

Normal + devmgr-probe, IDL + SMP, hosted Linux, NVIDIA read-only and NVIDIA GSP
pc-dev deployments build. The final ABI2-only revision still needs user boot
validation. NVIDIA builds are **not hardware validation**.

Unit tests are compile-only, never executed here. The user runs QEMU and
separately authorizes each NVIDIA hardware launch. Managed handles, dynamically
growing CSpace and IPC cancellation remain separate work.
