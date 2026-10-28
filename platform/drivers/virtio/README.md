# Virtio drivers

Driver-side integration of the pinned `virtio-drivers = 0.13.0` crate (MIT).
Devmgr does **not** depend on this crate and does not parse virtio vendor
capabilities, negotiate virtio features, reset devices or operate virtqueues.

Reuse upstream `PciTransport`, `VirtIOConsole`, `VirtQueue` and `Hal` rather than
implementing them again. `ConfigSnapshot` lets upstream's BAR-size probes run
against a shadow of devmgr-provided configuration, not writable hardware ECAM.
Only devmgr can change the real BARs, PCI command register and MSI routing.

Upstream 0.13.0 needs two small driver-side additions for Horn's first PCI path:

- `IommuPciTransport` requires modern virtio and `VIRTIO_F_ACCESS_PLATFORM`, adds
  that bit to the driver's feature selection, and checks FEATURES_OK readback.
  Upstream console's supported feature mask currently omits this bit.
- It selects the devmgr-provisioned MSI-X entry for configuration and queues
  before enabling each queue. Upstream PCI transport has no MSI-X selector API.

These are adapters, not another PCI transport or console implementation.
The mapped common-config region must remain live until the upstream transport
has been dropped. Driver initialization, reset and Drop run in the driver
process. Do not revoke its BARs first and then run transport Drop on stale MMIO.

## Console task and DMA integration

`console/` now builds a separate `virtio_console.elf` with a capability HAL
implementing all five upstream hooks using VM/device authority:
`dma_alloc`, `dma_dealloc`, `mmio_phys_to_virt`, `share`, `unshare`.
The crate's `PhysAddr` is a **device-visible IOVA**, not an exported RAM paddr.
Contiguous IOVA mappings do not require contiguous physical RAM.

The HAL is a thin adapter over [`horn-driver::DeviceSession`](../horn-driver/README.md).
BAR/VM/DMA ownership, directional bounces and IRQ grants live there, without any
virtio dependency or global state. The adapter translates `BufferDirection` to
device READ/WRITE permissions; its singleton exists only because upstream Hal
uses static methods.

- Allocate resident VM backing in the driver's own RAM account; map it into the
  CPU address space and into the device domain with the requested direction.
- Only resolve MMIO addresses within BAR grants already owned by this driver.
- For ordinary heap/stack buffers, initially use direction-aware bounce buffers
  in owned VM backing. `share` cannot blindly translate arbitrary virtual pages.
- `unshare`/`dma_dealloc` must wait for mapping close/invalidation/drain before
  releasing or reusing backing, and copy bounce output back only when complete.
  A failed close must retain/quarantine backing, not return it to the allocator.

The task installs VM-backed mimalloc as its default Rust allocator before
constructing upstream transport/console objects. Bootstrap and internal allocator
RPC metadata use separate bounded control storage; the heap callback transport
and device HAL use disjoint CSpace pools to avoid reentrant slot allocation.
One checked `TaskSlots` derived from generated bootstrap bounds supplies both
pools and the public receive slot; no independently coordinated midpoint literals.

The optional Q35 harness now assigns `00:04.0` in devmgr, passes only its scoped
device capability plus the driver's own VM authority, and launches this ELF.
`tests/boot/test_virtio_console.py` validates real host TX/RX echo and remapped
MSI-X on one/two CPUs in debug/release, including thousands of DMA map/unmap
operations. Negative builds deliberately issue an unmapped IOVA and a previously
used IOVA after its completed unmap/drain; devmgr wakes on its fault IRQ and reports the actual
VT-d source/address/reason. Fault injections require the `dma-probe` feature and
a separate Nix profile; the normal driver has no injection path. Test tasks
retain remaining backing and block on notification after injecting a failure,
rather than spinning or consuming device error completions as successful data.
The boot tests also measure QEMU host CPU during idle, including after a denied
DMA, and verify a new echo wakes the sleeping system.

The normal console task exports ConsoleInterface to the native shell; test
probes remain separate. Orderly reset/drain precedes buffer release. Failed
cleanup retains backing instead of claiming recovery. Native physical hardware
and device reassignment remain unvalidated.

## Network task

`net/` builds `virtio_net.elf`, using upstream `VirtIONetRaw` and the optional
`horn-driver::virtio` HAL/PCI helpers. It exports bounded Ethernet IPC to a
separate smoltcp netd; neither IP nor HTTP runs in the hardware driver.
See the [network demo](../../core/net/README.md) and
[always-on clock](../../core/clock/README.md).
