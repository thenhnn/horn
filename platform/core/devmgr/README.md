# Device manager

`horn-devmgr` contains the generated API, bounded immutable PCI inventory and
userspace VT-d backend. `task/` builds a separate `devmgr.elf`, using the ordinary
procd ELF loader, Horn ABI tag and independent RAM account. Root supplies kernel
mechanics; device/IOMMU policy runs in devmgr, with `KernelIOMMU` disabled.

**Status:** read-only discovery is boot-tested. Resource provisioning and native
VT-d wiring are in progress; there is not yet a validated virtio-console DMA/IRQ
end-to-end path. The default Q35 harness still publishes observer authority only
and reports `iommu = NotInitialized`. That state never means DMA is safe.

## Authority and API

`net.nhnn.horn.device.observe` v1:

- `status()` reports phase, count and explicit IOMMU state;
- `list(after)` returns four summaries and a next cursor (zero at end);
- `info(id)` returns identity/address, command snapshot, BAR addresses,
  MSI/MSI-X presence and Resizable BAR v1 metadata.

Pagination starts at zero. Records are sorted by segment:BDF; IDs are that
32-bit routing address plus one as u64. IDs/cursors select records, **not device
authority**. The complete snapshot has at most 256 functions and is immutable;
there is no hotplug/rescan API. Observer replies contain no capabilities.
Observers cannot receive endpoint traffic or transfer capabilities. Exported
endpoint badges are nonzero so a client cannot mint an observer into assignment
or device-control authority; the native boot probe checks this. Malformed
requests/unsolicited caps are discarded without killing the service.

Provisioning interfaces are separate from observer authority:

- `device.assign.claim(id)` requires trusted assignment authority;
- `device.control` grants only the assigned function's BAR memory, immutable PCI
  configuration snapshot/resource lengths, DMA mappings and IRQ notification;
- `device.mapping.close()` acknowledges IOTLB invalidation/read+write draining
  before the trusted broker releases its VM pin.

Writable ECAM, RAM physical addresses and the private platform authority are
never given to drivers. BAR memory is mapped directly through managed VM memory
objects, uncached and NX, not proxied per register access. MMIO bytes are not
charged as resident RAM; mapping/kernel objects still have an owner account.
DMA backing stays charged to its VM owner while pinned, without a second charge.
WRITE/FUTURE_WRITE seals also constrain writable DMA pins.

## Separation from drivers

Devmgr handles **PCI resources**, IOMMU domains, interrupt routing and safe
revocation. It does not identify virtio console IDs, parse virtio vendor
capabilities, negotiate virtio features, reset virtio devices or manage queues.
Those operations belong to a separate driver process using
[`horn-virtio`](../../drivers/virtio/README.md) and upstream `virtio-drivers`.
QEMU/virtio-console are an integration target, not devmgr's device model.

Detach first revokes/acknowledges every exported CPU MMIO alias, disables bus
mastering, masks/removes interrupt routes and denies the requester with completed
context/IOTLB invalidation and draining. Only then are DMA pins released. No
requester, domain ID, interrupt index or IOVA is reassigned during this boot;
late requests therefore fault instead of reaching another allocation. A device-
specific reset is not needed to promise reuse because **reuse is unsupported**.
Supporting reassignment will require explicit reset/quiescence policy, not a
claim that clearing PCI Bus Master Enable alone drains a device.

The current provisioning profile is deliberately narrow: one segment-0,
single-function root-bus endpoint, page-exclusive assigned memory BARs and
MSI-X. Bridges/requester aliases, shared-page BARs, device reassignment and MSI
fallback still need implementation. The intended interrupt policy remains
MSI-X first, MSI second, never INTx fallback. Do not treat this profile as
isolation support for arbitrary physical hardware.

## Firmware and PCIe

Pinned `acpi = 5.2.0` with default features disabled handles static ACPI tables;
there is no AML interpreter. The task validates the copied RSDP, traverses
RSDT/XSDT through `AcpiTables`, locates MCFG and scans only declared segment/bus
ranges through ECAM. Missing MCFG is fatal: no CF8/CFC fallback.

MCFG ranges are bounded, sorted, checked for bus/physical overlap, alignment and
overflow. Addressing is relative to bus ZERO, including for nonzero start buses;
acpi 5.2's bus-minus-start helper is deliberately not used.

Discovery handles multifunction and endpoint/bridge BAR layouts, 64-bit BARs,
validated conventional/extended capability chains, MSI/MSI-X presence and ReBAR
supported/current sizes. Discovery does not write configuration or size BARs.
Zero BARs are omitted because a read cannot distinguish absent from unassigned
32-bit BARs. Provisioning, separately, disables decoding/bus mastering and probes
current BAR sizes. There is no dynamic ReBAR resizing or bridge resource
allocation, ARI, SR-IOV, ATS/PASID, AER, hotplug or driver matching.

## VT-d profile

The backend uses coherent legacy tables, queued invalidation, full-SID checked
interrupt remapping and deny-by-default contexts. Compatibility interrupts stay
blocked. Mapping permissions are device READ/WRITE, and IOVAs are monotonic even
after unmap. Fault records are polled; device faults do not themselves poison
the controller, but queue failures/timeouts do.

DMAR parsing uses `acpi` table lookup/checksums plus a bounded DRHD parser. The
first profile requires one segment-0 INCLUDE_PCI_ALL unit and interrupt remapping;
RMRRs, multiple units, unsupported scopes and active firmware ownership fail
closed. No implicit identity maps or unprotected fallback are created.

Tables live in devmgr-owned resident VM memory with retained private physical
pins. Root cannot reclaim DMA backing on process teardown while pins or MMIO
leases remain. Uncertain cleanup retains/quarantines resources. Devmgr death or
unrecoverable IOMMU failure requires fail-stop/reboot, not DMA backing reuse.
Ordinary driver-death coordination remains incomplete; retained resources are
not a claim of automatic restart/cleanup.

## Bootstrap and tests

`DeviceCaps` names self/reply/service, private platform, VM factory and address
space. The private platform supplies RSDP, bounded physical access, MMIO import/
revocation, authenticated VM pin/unpin and kernel IRQ mechanics. Ordinary driver
capabilities never include this endpoint.

`examples/devmgr/` is a Q35 test root, not a production supervisor. Its device
Untypeds belong exclusively to a shared `DeviceBank`. Temporary reads and
retained BAR frames share a bounded 128-slot tree; eviction releases only its
own reference, and coalesces/revokes idle ancestors without touching live BARs.
Cleanup uncertainty poisons the bank and retains uncertain slots. Large frames
are an optimization, not the lifetime solution. A native probe holds an MMIO
mapping across temporary sibling/shared claims and verifies zero MMIO RAM
charge and complete slot return/coalescing, including x86 kernel-pointer versus
physical alignment above the direct-map wrap.

The root authenticates bootstrap/fault messages with distinct badges, enforces a
30-second startup deadline and yields at task priority on one CPU. Critical
faults request Q35 full reset without account teardown. This is not a portable
watchdog or runtime hang supervisor; the read-only observer probe temporarily
precedes resumed fault monitoring.

Tests include ACPI/ECAM bounds, segments/start buses, multifunction, BARs,
capability chains, ReBAR, pagination, codecs, model VT-d permissions/invalidation/
interrupt-source validation/faults, VM seal/pin policy, and native discovery/
slot-lifetime/fatal-reset regressions. **Model tests are not a QEMU DMA proof.**
