# Device manager: read-only discovery

`horn-devmgr` contains the generated API and a bounded immutable PCI inventory.
`task/` builds the separate `devmgr.elf` process. It uses the ordinary procd ELF
loader, Horn ABI tag and an independent process RAM account. It is not a root
module, a driver, or a global service registry.

## API

`net.nhnn.horn.device.observe` v1 is the only client-facing interface:

- `status()` reports `DiscoveryReady`, device count and IOMMU state;
- `list(after)` returns up to four summaries and a next cursor (zero at end);
- `info(id)` returns identity/address, command bits, BARs, MSI/MSI-X presence
  and PCIe Resizable BAR v1 metadata.

Start pagination at zero. Records are ordered by segment:BDF; IDs are that
32-bit routing address plus one, represented as u64. A cursor is an exclusive
lower bound. IDs select records, **not authority over devices**. The inventory
is published only after a complete scan, has at most 256 functions, and does
not change during the process lifetime. There is no hotplug/rescan API.

All replies fit inline and contain no capabilities. Malformed requests and
unsolicited caps are discarded without a reply; valid clients remain usable.
Clients receive send/call endpoint authority, not the server's receive authority
or bootstrap channel.
No API can write PCI configuration, assign devices, map BARs, allocate vectors,
resize BARs, enable bus mastering, or grant DMA access.

## Firmware and PCIe

Static table parsing uses pinned `acpi` 5.2.0 with default features disabled:
no AML interpreter or allocator-api dependency. The task validates the copied
boot RSDP, uses `AcpiTables` to validate/traverse RSDT/XSDT and locate MCFG,
and scans only MCFG-declared segment/bus ranges through ECAM. Missing MCFG
is a bootstrap failure: there is no CF8/CFC fallback.

MCFG ranges are bounded, sorted and checked for bus and physical overlap,
alignment and arithmetic overflow. Address calculation uses the bus-zero-based
MCFG base, including for nonzero start buses; it deliberately does not use
acpi 5.2's bus-minus-start `PciConfigRegions` helper.

Discovery handles multifunction devices and endpoint/bridge BAR layouts,
preserves 64-bit addresses, and bounds/checks both capability chains. It reads
MSI/MSI-X **presence**, not interrupt allocation or enabled/usable state.
Future PCI interrupt provisioning will prefer MSI-X, then MSI; no INTx fallback
is planned for this first driver path.

BAR reads never use write-all-ones sizing. BAR length is therefore not claimed. Zero BAR values are omitted because a
plain read cannot distinguish an unimplemented BAR from an unassigned 32-bit
BAR; ReBAR metadata can still identify such an unassigned resource.
ReBAR metadata reports supported sizes (bit N means 1 MiB << N) and current
size, without changing the firmware's configuration. Dynamic resize and bridge
window allocation are not implemented. Neither are ARI, SR-IOV, ATS/PASID, AER,
hotplug, driver matching, or full PCI resource management.

`iommu = NotInitialized` does **not** mean absent hardware or safe DMA. There
is no IOMMU backend, DMA pinning/domain API, interrupt remapping or device
isolation yet. IOMMU ownership/backend belongs in devmgr when implemented.

## Bootstrap and integration test

`DeviceCaps` names self CNode, reply object, service endpoint and a separate
private platform endpoint. The private `device.platform` interface supplies the
copied RSDP, bounded read-only physical reads and a ready acknowledgement.
It is not exposed to observer clients and is not a general driver MMIO API.

`examples/devmgr/` is a Q35 integration harness. Its root broker can read only
BootInfo device-typed physical memory, never ordinary RAM. It lends no raw
VSpace, ASID, scheduling, Untyped or physical-frame authority to the task.
The root does physical access mechanics; ACPI interpretation and PCI discovery
run in the devmgr process. The broker is bootstrap-only, not the future device
assignment/IOMMU implementation.

The broker uses one cached frame and a dedicated 128-CSlot pool. Reads map
read-only, uncached and NX, then unmap. On cache miss/ready it revokes the
entire temporary derivation tree before returning slots. Cleanup uncertainty
poisons the broker and retains its lease/slots. Large device frames reduce
ECAM retype work; bounded lifetime, not frame size, prevents slot exhaustion.
The boot probe verifies all 128 slots can be allocated again after discovery.

The harness authenticates ready and fault traffic with separate badges. It
has a 30-second startup deadline, yields at the task's priority on one CPU,
and requests Q35 full reset on devmgr faults without process/account teardown.
Fault monitoring resumes after the synchronous observer probe; this test root
is not a production supervisor or a runtime hang watchdog. Reset is Q35-specific;
there is no portable hardware watchdog/reset implementation yet. No automatic
recovery or reboot-time DMA safety is claimed.

Tests cover synthetic ACPI/ECAM, nonzero start buses, multiple segments,
multifunction, pagination, BARs, MSI/MSI-X, ReBAR, malformed chains, bounds and
Rust/C codecs. QEMU tests exercise one/two CPUs, debug/release, unsolicited
requests/caps, bounded slot reuse, and injected devmgr fault -> reset. A
separate stalled-start probe checks the startup deadline.
