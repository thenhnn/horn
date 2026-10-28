# Capability-based driver resource helpers

`horn-driver` factors device-independent resource mechanics out of the console
HAL. It depends on the devmgr/VM protocol and seL4 IPC, **not** on virtio or an
executable runtime. There is no driver base class, singleton, matching registry,
reset algorithm, bus enumeration or supervisor.

`DeviceSession` receives an application transport plus explicit VM factory,
address-space and assigned-device capabilities. Bootstrap names and CSpace
layout are provided by the executable, not guessed by this library:

```text
horn-runtime        entry, mimalloc, bootstrap helpers and checked TaskSlots
horn-driver         owned BAR mappings, resident DMA/IOVA/bounces, IRQ notification
horn-virtio         upstream transport/feature adaptation
virtio-console      upstream Hal forwarding and console protocol
```

## Resource operations

- `control()` exposes only the supplied scoped DeviceControl authority.
- `map_bars(resources)` imports/maps the assigned memory BARs. `mmio(address,
  size)` resolves only inside those retained grants, with checked bounds.
  Bus addresses select grants; they do not authorize arbitrary MMIO mapping.
- `interrupt()` obtains/retains a read-only notification; repeated calls reuse
  it. `activate()` enables the assigned device and `ack_interrupt()` rearms it.
- `dma_alloc(pages, access)` returns fresh zeroed resident pages plus **IOVA**, not
  RAM physical address. The current bounded pin profile permits 1..16 4KiB pages.
  `DeviceAccess::{Read, Write, ReadWrite}` names DEVICE permissions.
- Unsafe `share`/`unshare` use independent directional bounce buffers for caller
  stack/heap memory. Sources stay live and CPU-exclusive until matching unshare.
  Device-readable data is copied in; device-written data is copied back only
  after acknowledged translation removal and read/write draining.
- Unsafe `dma_dealloc` validates the original pointer/length, closes/drains the
  DMA mapping, then destroys the CPU region and closes backing authority.

Only the virtio adapter translates upstream `BufferDirection` and maintains a
singleton to satisfy upstream static Hal methods. Test fault injection remains
in the console executable; the common library has no injected-address path.

## Lifetime and failure policy

A session retains mappings, buffer records and its IRQ grant. DMA records remain
present until close/drain, copy-back and CPU/backing cleanup complete. No Drop
performs implicit hardware cleanup. Dropping Rust bookkeeping is not a claim
that resources were closed: VM grants/pins remain retained by their services.

The current API preserves the prototype's fail-stop error policy: RPC/cleanup
failure panics, and the task runtime aborts. Do not catch a panic and continue
using the session or treat uncertain close as successful. This is not yet a
recoverable device-error API or a shutdown transaction. Driver-specific queue
quiescence/reset, orderly full-session shutdown and safe detach/reassignment are
separate work. Never infer that clearing bus mastering alone drains a device.

Tests cover permission bits, MMIO bounds/overflow and DMA bounds; the console
boots exercise this shared path with thousands of real DMA operations, actual
MSI-X, wrong-cap rejection, outside/stale IOVA faults and idle/wakeup checks.
Physical hardware and other driver protocols remain unvalidated.
