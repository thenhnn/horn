# Device manager

`horn-devmgr` contains the generated API, bounded immutable PCI inventory and
userspace VT-d backend. `task/` builds a separate `devmgr.elf`, using the ordinary
procd ELF loader, Horn ABI tag and independent RAM account. Root supplies kernel
mechanics; device/IOMMU policy runs in devmgr, with `KernelIOMMU` disabled.
The task uses [`horn-runtime`](../../runtime/rust/horn-runtime/README.md): its
VM-backed global mimalloc heap is installed before discovery/application code.
Heap callbacks have a separate CSpace pool from public/private service IPC.
A native startup provenance probe checks an ordinary Box is not on the control
heap (`DEVMGR_ALLOCATOR allocator=mimalloc`).

**Status:** read-only discovery and the optional Q35 virtio-console DMA/IRQ path
are boot-tested. The default harness still publishes observer authority only and
reports `iommu = NotInitialized`; that state never means DMA is safe. The console
profile enables userspace VT-d and interrupt remapping, launches a separate
capability-restricted driver, and exercises real host TX/RX plus rejected DMA.
This remains a narrow experimental QEMU profile, not physical-hardware support.

The PC demo also always starts [timerd](../clock/README.md). The existing
shell launch includes the [virtio-net/netd/HTTP demo](../net/README.md).

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

The provisioning profile remains narrow: two segment-0 root-bus function-zero
endpoints, no enumerated sibling functions, and page-exclusive memory BARs.
A multifunction header alone is accepted (e.g. VFIO's GPU-only guest function);
an enumerated audio sibling is not assigned implicitly. Bridges/requester
aliases, shared-page BARs and reassignment remain unsupported.

Interrupt selection is **MSI-X entry zero first, remapped single MSI second,
never INTx**. Both mechanisms are disabled before configuration changes; MSI
uses MME=0, supports 32/64-bit address layouts and optional mask bits, and writes
the data field as a WORD. The full-SID IRTE and bound, armed kernel IRQ precede
device interrupt enable. Close disables both mechanisms before removing the
route. This new MSI path has local layout/encoding evidence and builds, not yet
GPU interrupt-delivery evidence.

Large BARs are inventoried/sized without mapping them wholesale.
`bar_window(index, offset, length, space)` grants only a page-aligned window,
at most 64 KiB; each device retains at most eight exported MMIO leases.
Legacy `bar()` still only accepts an entire BAR up to 64 KiB. The broker's
private MSI-X map covers just entry zero's page(s). CPU MMIO decoding is enabled
while bus mastering remains off, so a read-only probe needs no DMA activation.

### Quota-bound DMA reservations

The legacy `map()` and each physical pin/map operation stay bounded to sixteen
pages (64 KiB). `reserve_dma(length, access)` allocates a contiguous monotonic
IOVA window and charges its **entire** length and one object immediately,
including unpopulated reservations. Its scoped mapping capability supports
ordered `append(memory, offset, length)` chunks of exactly
min(64 KiB, remaining), followed by `finish()`. Different reservations can be
populated interleaved without reusing IOVAs. Finish rejects incomplete windows;
initial DMA activation rejects pending reservations. Finish is bookkeeping,
**not atomic hardware publication**: already populated prefixes are mapped.
Drivers must not submit a new window to hardware before finishing it.

On ordinary partial failure the caller must close the reservation: every
published chunk is unmapped/drained before its pin is released. The common
driver allocator performs this rollback before reporting a population failure.
An uncertain map/drain/unpin is fail-stop and retains backing, never a
successful cleanup. Device teardown also retires unfinished reservations.

Trusted launch configuration supplies `dmaBytes` and `dmaObjects`; drivers
cannot raise them through IPC. Defaults preserve virtio's 768 KiB / twelve
mapping budget. Policy ceilings are 64 MiB / 64 objects per assignment; physical
pins remain chunked and charged to the VM memory owner. The root platform has a
separate global hard ceiling of 4096 retained pin records (previously 128), not
an unbounded pin table. Fixed chunk sizing bounds metadata as well as bytes.
The VT-d table-page ceiling and no-IOVA-reuse policy are unchanged. Exhaustion
remains possible; these are not unlimited GPU allocations or tested GSP DMA.

Do not treat this profile as isolation support for arbitrary physical hardware.

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

The backend uses cache-maintained legacy tables, queued invalidation, full-SID checked
interrupt remapping and deny-by-default contexts. Compatibility interrupts stay
blocked. Mapping permissions are device READ/WRITE, and IOVAs are monotonic even
after unmap. A dedicated fault-event MSI wakes devmgr through a bound notification
while it blocks in endpoint receive. The handler masks fault events, drains the
records, acknowledges the kernel IRQ, then unmasks pending events. There is no
steady-state fault polling. The remapping unit's own fault MSI bypasses remapping;
this does **not** enable compatibility interrupts from PCI devices. Device faults
do not themselves poison the controller, but queue failures/timeouts do.

DMAR parsing uses `acpi` table lookup/checksums plus a bounded DRHD parser. The
profile requires one segment-0 unit and interrupt remapping, with either
INCLUDE_PCI_ALL or explicit single-hop root-bus endpoint scopes. Only covered
requester IDs may be attached; IOAPIC/HPET scopes grant no DMA permission.
RMRRs, multiple units, unsupported scopes and active firmware ownership fail
closed. No implicit identity maps or unprotected fallback are created. The x86
backend publishes table zeroing/writes with CLFLUSH and fences, and invalidates
cached completion words before reads: QEMU does not advertise table snooping.
The current native backend requires 64-byte CLFLUSH support.

Tables live in devmgr-owned resident VM memory with retained private physical
pins. Root cannot reclaim DMA backing on process teardown while pins or MMIO
leases remain. Uncertain cleanup retains/quarantines resources. Devmgr death or
unrecoverable IOMMU failure requires fail-stop/reboot, not DMA backing reuse.
Ordinary driver-death coordination remains incomplete; retained resources are
not a claim of automatic restart/cleanup.

## Bootstrap and tests

`DeviceCaps` names self/reply/service, private platform, VM factory, address
space and an IOMMU fault notification bound to devmgr's TCB. Its notification
badge is disjoint from the endpoint authority namespace. The private platform supplies RSDP, bounded physical access, MMIO import/
revocation, authenticated VM pin/unpin and kernel IRQ mechanics. Ordinary driver
capabilities never include this endpoint. An optional trusted bootstrap
assignment ID is consumed inside devmgr; its resulting scoped device capability
is delivered to root and copied into the unstarted driver's named slot. The
driver receives neither assignment authority nor the private platform endpoint.

`examples/devmgr/` is a Q35 test root, not a production supervisor. Its device
Untypeds belong exclusively to a shared `DeviceBank`. Temporary reads and
retained HPET/VT-d/BAR frames share a bounded 256-slot tree; eviction releases only its
own reference, and coalesces/revokes idle ancestors without touching live BARs.
Cleanup uncertainty poisons the bank and retains uncertain slots. Large frames
are an optimization, not the lifetime solution. A native probe holds an MMIO
mapping across temporary sibling/shared claims and verifies zero MMIO RAM
charge and complete slot return/coalescing, including x86 kernel-pointer versus
physical alignment above the direct-map wrap.

The root authenticates bootstrap/fault messages with distinct badges, enforces a
30-second startup deadline and yields at task priority on one CPU during that
bounded bootstrap polling phase. After readiness, root blocks on IPC/faults and
devmgr blocks on IPC or its bound IOMMU notification, allowing the kernel idle
thread to HLT. A timer-backed startup deadline remains future work. Critical
faults request Q35 full reset without account teardown. This is not a portable
watchdog or runtime hang supervisor; the read-only observer probe temporarily
precedes resumed fault monitoring.

Tests include ACPI/ECAM bounds, segments/start buses, multifunction, BARs,
capability chains, ReBAR, pagination, codecs, model VT-d permissions/invalidation/
interrupt-source validation/faults, VM seal/pin policy, and native discovery/
slot-lifetime/fatal-reset regressions. `tests/boot/test_virtio_console.py` additionally
runs host TX/RX echo with thousands of map/unmap operations in debug/release on
one/two CPUs, and isolated feature-gated negative drivers. An unmapped address
and a previously used, then invalidated/drained IOVA produce real hardware fault
records for the assigned requester. The latter first transmits valid data to
warm translation caching before unmapping and deliberately trying its stale
address again. Neither test relaxes IOMMU permissions or adds identity mappings.
Linux host process-CPU measurements check idle after normal traffic and after
negative tests; another echo verifies wakeup after idle. Failed test drivers
block without rearming the device or releasing uncertain backing, rather than
spinning in Yield. Model tests are not a substitute for these tests or physical-hardware validation.
