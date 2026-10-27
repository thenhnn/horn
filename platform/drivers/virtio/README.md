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

## DMA integration contract (not wired yet)

A Horn `Hal` must implement all five upstream hooks using VM/device authority:
`dma_alloc`, `dma_dealloc`, `mmio_phys_to_virt`, `share`, `unshare`.
The crate's `PhysAddr` is a **device-visible IOVA**, not an exported RAM paddr.
Contiguous IOVA mappings do not require contiguous physical RAM.

- Allocate resident VM backing in the driver's own RAM account; map it into the
  CPU address space and into the device domain with the requested direction.
- Only resolve MMIO addresses within BAR grants already owned by this driver.
- For ordinary heap/stack buffers, initially use direction-aware bounce buffers
  in owned VM backing. `share` cannot blindly translate arbitrary virtual pages.
- `unshare`/`dma_dealloc` must wait for mapping close/invalidation/drain before
  releasing or reusing backing, and copy bounce output back only when complete.
  A failed close must retain/quarantine backing, not return it to the allocator.

The separate console ELF, capability-backed HAL and QEMU DMA/IRQ end-to-end test
remain implementation work. Unit tests of these adapters are not DMA isolation
validation.
