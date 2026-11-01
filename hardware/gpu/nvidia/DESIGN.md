# AD106 → first GSP reply

Status: native read-only probe builds, plus host-testable guards; no VFIO run
or GSP boot yet. Target is only the
host-confirmed Palit RTX 4060 Ti `10de:2803 / 1569:f306` (AD106). No seL4 source changes
or host device operations are part of this change.

## Ownership and layering

```text
future libgpu / NVK / compositor       (not part of boot milestone)
            capability IPC
native nvidia server, one assigned GPU
  AD106 registers / Falcon / VBIOS / firmware / GSP transport
            bounded capability operations
horn-driver adapter + devmgr + VM + clock + supervisor
            ordinary seL4 primitives
QEMU VFIO + virtual VT-d → host VFIO/IOMMU → physical GPU
```

One server per GPU owns its device capability and all firmware/channel state.
Separate future `gpu.*` and `disp.*` capability families do not imply two device
owners. They also do not assume that GSP allows only one logical RM client.
No public rendering API or Linux DRM shim is frozen before boot works.

The microkernel remains vendor-neutral. devmgr, not the server, owns assignment,
SID-scoped IOMMU and interrupt-remapping configuration. BAR numbers, IOVAs, local
object IDs and GPU VAs are selectors, never authority. Linux on the host owns
AMD-Vi on this machine; Horn would use the **virtual Intel VT-d** device in QEMU.
That does not require a native Horn AMD-Vi implementation, but successful
VFIO/vIOMMU integration is not yet established.

## What the reviewed NOVA actually does

All paths below are relative to `drivers/gpu/nova-core/` at
`3497a403264b094ebd61b8b755165d56e7d9dc37`.

| Subject | Primary source | Finding for this port |
| --- | --- | --- |
| Basic device lifetime | `driver.rs`, `gpu.rs` | BAR0 is 16 MiB; interrupts/resources outlive their users; shutdown ordering matters |
| Identification | `gpu/regs.rs`, `include/uapi/drm/nova_drm.h` in Linux | BOOT42 chipset `0x196` is AD106, architecture `0x19`; reject other chips |
| Initial devinit | `gpu/hal/tu102.rs`, `regs.rs` | Wait for privilege bit before reading GFW progress; nominal upper wait 4 s; DMA mask 47 bits |
| Ada boot selection | `gsp/hal.rs`, `gsp/hal/ga102.rs` | Ada uses GA102/TU102 boot flow, **no FSP**, no FWSEC bootloader stage |
| Signed boot/unload | `gsp/hal/tu102.rs` | VBIOS-derived FWSEC, FRTS/WPR2 checks, SEC2 booter load; prepare FWSEC-SB + booter-unload before starting |
| Boot handshake | `gsp/boot.rs`, `gpu.rs` | System info/registry commands are queued before boot; RISC-V active is NOT GSP init done; static info comes later |
| Firmware inputs | `firmware/gsp.rs`, `firmware/booter.rs`, `firmware/tlv.rs` | Per-chip NVFW TLVs + binary/signatures, bootloader, load/unload booter; FWSEC also depends on actual VBIOS |
| ABI | `gsp/fw.rs`, `gsp/fw/r570_144/bindings.rs` | r570_144 wire structures; maximum transport element 64 KiB; not an arbitrary current host firmware ABI |
| Command queue | `gsp/cmdq.rs`, `gsp/fw.rs` | Outer element count/sequence/checksum AND inner RPC header; command size includes variable payload |
| Async processing | `gsp/cmdq.rs`, `gsp/sequencer.rs` | Process boot notifications/sequencer callbacks while awaiting replies; they are not all ignorable log messages |
| Interrupts | `irq/interrupt_tree.rs`, `irq/hal/tu102.rs` | MSI-X or single MSI; GSP source 155 is leaf 4 bit 27 / subtree 2; MSI uses device-specific rearm |
| Memory | `mm.rs`, `firmware/radix3.rs`, `gsp.rs` | GPU VA, VRAM addresses and host DMA addresses differ; firmware uses radix3; queues/logs/args use device-visible backing |

Useful Linux Rust abstractions to study, not wholesale import:
`rust/kernel/io/`, `dma.rs`, `pci.rs`, `irq/`, `time.rs`, `types.rs`, and
pin-init usage. Specifically: bounded register windows, different DMA/VRAM
address types, ownership-aware DMA views, and dependent resource lifetimes.
Do not import Linux registration, debugfs, GFP allocation or kernel threads into
Horn just to make NOVA source compile.

### Two corrections to the proposed protocol sketch

1. `len` is not universally the fixed structure size. In this revision,
   `rpc_message_header_v::init` sets length to RPC header + command bytes, and
   command bytes include the variable payload. Outer page count is separate.
   Every field must be interpreted according to this specific ABI, with lower
   bounds, upper bounds, checked arithmetic and continuation accounting.
2. Do not assume a reply echoes the host's sequence number. NOVA assigns outer
   `seqNum` but zero-initializes inner RPC sequence on send; its receive path
   logs inner sequence and matches by function. The exact wire ordering/replay
   rules require validation against the pinned firmware/OpenRM before porting.
   Our `RpcTracker` has **local** tickets only; it does not settle those rules.

## Horn gaps to fix before firmware DMA

Generic resource support now exists; device-specific work and hardware
validation remain explicit, not hidden behind a pretend platform backend:

### 1. MSI for this exact board

Host inventory confirms **MSI, Count=1/1, 64-bit, Maskable-**, no MSI-X.
Devmgr now prefers MSI-X entry zero, otherwise programs a generic SID-remapped
single MSI, with MSI disabled during updates and MME=0. The IRTE and armed
kernel IRQ precede device interrupt enable. Both 32/64-bit layouts and optional
mask bits are supported; no INTx fallback exists. Address/data come only from
devmgr. Native builds and local layout guards pass, not physical GPU delivery.
The probe does not yet request an IRQ. Port GIN/device rearm and coordinate it
with the seL4 notification/ack path before enabling GSP interrupts. The MSI-X alternative would also need entry selection: source 155 routes
to entry 2, not our current hardcoded entry 0.

### 2. Selective MMIO

`DeviceSession::map_bars` maps every BAR. It is inappropriate for this board:
BAR1 is an 8 GiB ReBAR aperture, BAR3 32 MiB, and BAR5 is legacy IO.
Start with only UC BAR0, not all BARs, no IO ports and no writable global ECAM.
The probe uses two 4-KiB BAR0 windows through the new bounded
`DeviceControl.bar_window` (at most 64 KiB / eight leases). Legacy `bar` still
grants only whole small BARs. Larger VRAM-access policy is not implemented.
Keep CPU VA, BAR bus address and VRAM offset distinct. Never map full BAR1 as
resident RAM.

### 3. Larger DMA objects, bounded operations

Physical pin/map chunks remain at most sixteen pages. Devmgr now admits
larger contiguous IOVA reservations under trusted per-assignment byte/object
quotas, with fixed ordered append chunks and explicit finish/close. NOVA's queue object alone
has a 4 KiB PTE page and two roughly 256 KiB queue areas (63 data pages each),
plus separate log/argument buffers and a large firmware image. These
new generic primitives still need a GSP-specific allocator/lifetime adapter.
The native module grants a provisional 64-MiB / 64-object ceiling; virtio keeps
its existing 768-KiB / twelve-object budget. The common allocator uses the new
reservation for allocations over 64 KiB and rolls back partial population through
acknowledged close. Finish is not atomic publication: drivers must not submit
a partially populated window to hardware. Contiguous IOVA is reserved by the
broker, not inferred from consecutive allocations. Firmware radix3/SG may
reference separate IOVA pages; none of those entries is a CPU physical address.
Fixed chunk sizing also bounds pin metadata. Root has a separate global
4096-record hard ceiling; VT-d table pages and monotonic address space remain
bounded. Firmware sizing and GPU DMA behavior have not been validated yet.

Close means unmap/invalidate/drain acknowledgment before CPU backing release.
Partial admission/rollback keeps uncertain mappings pinned. Validate the entire
range against the AD106 47-bit DMA mask. VRAM reservations (WPR, heap, tables)
need their own accounting and must exclude firmware-owned regions.

### 4. Firmware/VBIOS and lifetime adapter

Identify and pin a matching redistributable bundle and extraction tooling with
hashes/license; no build-time downloads, firmware patches or dependency on
`work/nova`. Expected Ada inputs in this checkout are `gsp.tlv`, referenced
`gsp.bin`, `gsp_bootloader.tlv`, `booter_load.tlv`, `booter_unload.tlv`, plus
VBIOS-derived FWSEC. An installed host `gsp_*.bin` alone is not that bundle.
Validate TLV lengths/tags/duplicates, ELF/descriptor extents, signatures and
chip/version selection before using any offsets. Select production signatures
as the source requires, not a signature-bypass scheme.

Represent firmware, DMA and IRQ ownership explicitly. NOVA's successful Drop
ordering relies on Linux free_irq/unbind semantics; copying that RAII behavior
would be wrong in Horn. A timed-out firmware operation may still DMA. No Drop,
ordinary error return, client disconnect or process death may silently free its
backing. Retain/quarantine until supervisor/devmgr cleanup acknowledges it.

## Boot control flow sketch (NOT implemented)

This is future **GSP** readiness. The present probe publishes only the separate
read-only diagnostic service, never a GPU/GSP-ready capability.

```text
receive named bootstrap device / VM / clock / supervisor capabilities
  validate assigned function against the Palit allowlist
  map bounded BAR0 UC; identify AD106; do not enable DMA yet
  wait GFW privilege then completion against an absolute deadline
  prepare validated firmware and VBIOS-derived FWSEC + unload resources
  allocate/account/initialize logs, queues, libos args, radix3 and WPR metadata
  establish permitted IOVA mappings and interrupt route; publish with barriers
  queue SetSystemInfo and reviewed registry settings (not arbitrary passthrough)
  activate only after the required DMA/IRQ resources are in place
  verify clean initial WPR state; run FWSEC-FRTS, verify requested WPR2 extent
  reset/prepare GSP Falcon, set libos arguments; run SEC2 booter loader
  wait RISC-V active; service sequencer requests and asynchronous boot messages
  wait GspInitDone; enable/rearm GSP interrupt path; drain early queued messages
  issue GetGspStaticInfo; validate headers, status, framing and FB ranges
  log real name / chipset / reported usable VRAM; only now signal GSP READY
```

The ordering inside FWSEC/Falcon/SEC2 operations must follow the source, not the
compressed bullets above. In particular an existing WPR2 is not proof of success
or permission to reuse an old driver session; source rejects it and requires
reset. A VBIOS read must use the reviewed GPU access path; no executing option
ROM code and no unconditional host sysfs ROM-enable writes.

All waits use the existing clock/alarm capability, absolute monotonic deadlines
and bounded work per event-loop turn. Events never renew a request's timeout.
Keep the normal 30-second supervisor READY contract; fail with a named stage,
last register/mailbox sample and retained fault diagnostics rather than claiming
READY early. This is independent of a diagnostic VM runtime limit.

## RPC and shared memory rules for the port

- Single serialized command producer/consumer to start; do not expose raw RM
  calls or firmware payload construction to application clients.
- Separate async notifications, replies, continuation records and sequencer
  callbacks. No guessed flat dispatch or unbounded callback loops.
- Snapshot device-writable bytes before parsing. Check ring indices, wrap,
  published page counts, both headers, version/signature/checksum/status, every
  subrange/count and resource ID against the owning session. Never use an
  untrusted size directly for allocation. Enforce a bounded aggregate message
  size even when continuations arrive separately.
- Use device-memory-safe access and explicit DMA/MMIO barriers, not ordinary
  aliased Rust slices or only a compiler RELEASE store. Do not ring a doorbell
  before the payload and producer index are visible to the device.
- Do not infer usable VRAM from BAR1 size or trust firmware region lists without
  checked range/overlap/reservation validation. CPU BAR mappings are not GPUVM.
- Build system-information commands from the assigned PCI snapshot and approved
  capabilities. Never forward client requests that enlarge DMA visibility.
  Review NOVA's registry defaults explicitly: `RMSecBusResetEnable` and
  `RMDevidCheckIgnore` are not automatically suitable Horn policy. Firmware
  sequencer requests may access only checked assigned-device registers/resources,
  never arbitrary CPU memory, global ECAM or unrelated PCI functions.
- A timeout poisons the transport. A newly constructed Rust tracker is not a new
  hardware epoch; stale replies and in-flight DMA require actual teardown.

## Deferred public API constraints

Keep backing objects separate from GPUVM mappings: a numeric VA cannot be the
persistent buffer identity, especially after sharing/unmap/reuse. Raw client
command submission and doorbell grants require a separate privilege/isolation
audit; 4 KiB allocation alone does not prove doorbell isolation. GPU W^X/NX
support must be checked against actual PTE/engine permissions, not inferred from
Horn's CPU W^X or a `MEM_EXEC` flag. Shared scanout needs independently held
backing/pins and explicit producer/compositor ownership. NVK, not RADV, is the
future NVIDIA Mesa target. These are not promises of the current boot sketch.

## Shutdown and VFIO boundaries

Stop accepting work; disable GIN delivery and synchronize the handler; attempt
GSP shutdown + FWSEC-SB/booter-unload using retained resources. On any uncertain
result, keep DMA backing alive and hand control to supervisor/devmgr. Device
reset, interrupt drain, IOMMU invalidation and backing release are separate
acknowledged steps. No automatic reset/restart/reassignment in this milestone.

Host group 12 contains exactly GPU and audio; both currently have host drivers.
Before handing the group to VFIO, stop host GPU/display users and handle the HDA
function as part of the same isolation/reset unit. Only GPU needs to be passed
to/run by the guest. No ACS override or assumption that an advertised FLR proves
a reset/rebind cycle works. A bus reset can have a wider effect than one function.
No detach/reset has been attempted. QEMU teardown is not a substitute for an
explained device-cleanup failure inside Horn.

## Implementation order / completion evidence

1. Generic single MSI, bounded BAR windows and quota-bound chunked DMA are
   implemented together in devmgr; no GPU-specific microkernel API.
2. Native assigned-device task and opt-in build module now build. Run the
   read-only probe on VFIO for actual BOOT42/GFW samples; no mock substitute.
3. GSP DMA/lifetime and firmware port, starting with VBIOS and validated formats;
   prepare both start and cleanup paths before allowing GSP to DMA.
4. Execute the source-ordered Ada boot and obtain an actual decoded static-info
   response; retain serial traces and IOMMU fault logs, then exercise cleanup.

The library's current seven tests establish only its local guards. They do not
establish firmware safety, VFIO reset reliability, hardware isolation, GSP boot,
VRAM allocation, queue submission or display. Rendering/NVK and the public
`gpu.*`/`disp.*` protocol follow this milestone, not precede it.
