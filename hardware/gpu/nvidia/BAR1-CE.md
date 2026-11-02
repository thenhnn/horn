# BAR1 / CE — implemented, hardware-unvalidated

Separate jj change after operator-confirmed live PRAMIN animation. The operator's
first BAR1 attempt (`work/nvidia-vfio-20261102-165219/serial.log`) stopped at the
first map with backend `Error::State`, before BAR1_MAP/CE/display PUT. Existing
markers cannot locate the exact branch or prove whether root publication began.
Added phase/root-entry/invalidation diagnostics preserve the same fail-stop policy;
they have not been run on hardware. See HARDWARE.md. No successful BAR1, CE,
visible result or speed measurement is claimed.
Native build: `work/nvidia-bar1-ce-build.log`; unit tests skipped by request.

## Active CPU backend

`task/src/native/bar1.rs` is the CPU path for all driver-owned VRAM: framebuffer,
continuous in-place animation, LUT, SF CRC reads/initialization, display RAMHT
and CE instance initialization. There is **no PRAMIN fallback**. PRAMIN's active
use is confined to the shared BAR1 page-table bootstrap: bounded reads of the
existing path and publication of one revalidated zero entry.
The old generic GSP PRAMIN helpers remain available for historical fixtures,
not as the active display backend.

A PCI BAR1 CPU mapping does not select physical VRAM; ReBAR changes aperture
size, not this requirement. We use RM's published `bar1PdeBase`, following
nouveau `nvkm/subdev/gsp/rm/r535/bar.c::r535_bar_bar1_init`. The operator explicitly
authorized these shared page-table memory writes. This is not permission to
write firmware code or WPR. Preserve occupied PD3/PD2 entries, following only
validated ordinary VRAM PDEs below WPR, outside owned allocations and without
cycles. An empty ancestor can link a private subtree; otherwise scan PD1 slots
within the actual aperture for a sufficiently large free512-MiB segment. Only
an initially **zero** entry is installed, after revalidating the observed path.
Do not follow foreign system-memory pointers or huge PTEs. No free PD1 slot,
unsupported entry or changed path stops the session; no deeper-tree overwrite,
root replacement RPC or speculative retry. Private PD0 indexing is relative to
the chosen segment, while CPU BAR1 leases use its actual (possibly >4GiB) VA.

- Detect the second memory BAR from devmgr's resource snapshot, accounting for
  PCI 64-bit upper slots; require a prefetchable memory aperture. Before any
  publication, inspect BAR1_BLOCK read-only, require virtual mode and an
  inspectable VRAM instance below WPR. Read only its SDK-defined page-directory/
  ADR_LIMIT fields; require the published root and VER2 format. Bound VA by the
  minimum of PCI aperture size and active GPU ADR_LIMIT. Never rewrite the bind
  register or dereference foreign system memory to make this check pass.
- Reject root/VRAM ranges reaching WPR, VRAM overlapping the root or previous
  mappings, misalignment, overflow, too many mappings or aperture exhaustion.
- Private PD2/PD1/dual-PD0 parents are accounted coherent DMA memory. Each fresh
  allocation gets a fresh 2-MiB-rounded aperture span and sealed 4-KiB leaf tables.
  Only owned allocation pages are valid; rounded guard pages remain invalid.
- Parents append unused slots only, valid-last; no existing mapping or leaf is
  overwritten, unmapped or recycled. Low IOVAs keep valid-last publication a
  32-bit store. DMA publication barriers precede hardware access.
- BAR1 invalidation follows `tu102_vmm_flush`: PAGE_ALL/HUB_ONLY/ALL_PDB through
  `0xb830a0/a4/b0`, waiting for the trigger to clear under a 2-second deadline.
  Never replace a still-active invalidation request. Native serializes CPU use.
- One revocable CPU window, at most64KiB, stays within the eight-export device
  bound together with three cached BAR0 and two fixed interrupt windows. Windows
  do not expose the allocation's invalid guard. All GPU backing/tables remain
  retained; CPU-window revocation is not GPU-memory retirement.
- Bulk aligned 64-bit UC stores (32-bit edges), a publication fence and one
  final UC read per batch, not per pixel. The final word is checked for gross
  mapping/order failure; this is **not full data verification or scanout proof**.
- CRC reads are volatile UC observations with acquire ordering, not ordinary
  Rust slices or an atomic live snapshot. Existing DONE/detach protocol remains.

This backend is **UC, not WC**. Current VM/devmgr exports do not implement WC.
No incompatible cache aliases are introduced. A future WC path needs explicit
VM/devmgr cache policy, x86 PAT support and correct write-draining semantics.
The 64-KiB window switching and UC stores can still limit throughput; measure
before claiming an improvement. Animation keeps its unbounded lifetime,
per-repaint2-second deadline, event checks, no new UPDATE, and possible tearing.

## CE execution

`task/src/boot/copy.rs` performs exactly one fresh pre-scanout transaction:
64KiB RAM→owned framebuffer VRAM, then VRAM→a separate coherent RAM destination.
This temporary pattern is overwritten by the ordinary framebuffer initializer
only **after** completion and verification. No display PUT has happened yet.

- Confirm COPY0 exists; allocate its RM instance and required fault method buffer.
- Prepare all page tables, GPFIFO, both launches and virtual semaphore mapping
  before SET_PAGE_DIRECTORY or channel publication. Freeze the compute VMM.
- Allocate/bind the CE and SET_OBJECT on subchannel0; validate relevant RM echoes.
- Query typed `NVC36F_CTRL_CMD_GPFIFO_GET_WORK_SUBMIT_TOKEN`. The opaque token,
  not a guessed CID/runlist encoding, goes to `0xbb0090`.
- Publish CPU stores, GP_PUT=1, another DMA barrier, then the doorbell.
- Use non-pipelined physical transfers, coherent-system/VRAM targets, SYS flush,
  and release the **virtual-addressed** semaphore only on the final launch.
- Drain display events while waiting, with a2-second fence deadline inside a
 10-second total CE phase and the boot deadline. No retry or second submission.
- Require the GPU fence payload, then verify every returned RAM word by volatile
  loads and, once during bring-up, every word of the BAR1 VRAM view. This full
  readback is diagnostic only and is absent from the animation upload hot path.
- GP_GET is logged, not mistaken for execution completion or surface retirement.
  Retain tables, ring, USERD, method buffer and transfer backing. DeviceSession
  also retains pins on all error exits; no Drop cleanup, free, reset or reuse.

This is a real single-batch submit path, **not yet a reusable streaming CE upload
backend**. Repeated submissions, ring retirement/reuse, frame flips and vblank
synchronization are not implemented. Live updates currently use BAR1 CPU stores.

## Source-level corrections

Pinned570.144 SDK (`8ec351aeb96a93a4bb69ccc12a542bf8a8df2b6f`) supplies generated
fields, sizes and typed field-wise RM codecs; requests have zeroed padding.
Nouveau/Nova reference: `3497a403264b094ebd61b8b755165d56e7d9dc37`.

- GPFIFO LENGTH belongs in high-word30:10, not9:0; validate the full40-bit span.
- Use NVC56F DMA_INCR/NONINCR host packets (count28:16), not the older CE method
  packet's count27:18/opcode definitions.
- AD10x uses GP10X VER2 geometry: PD3[48:47], PD2[46:38], PD1[37:29], dual16-byte
  PD0[28:21], 8-byte PTE[20:12]. Upper directories are ordinary8-byte entries.
  Use the pinned GP100 common VER2 fields plus TU102's current kind/tag constants,
  not obsolete pre-VER2 PTE/PDE bit positions. Sysmem aperture is4; VRAM PTE is0
  and VRAM PDE is2. Small-page pointers occupy PD0's second64-bit half.
- `Tables::existing` never clears earlier mappings. Compute mappings are added
  CPU-side only before final arena publication, never after RM sees the root.

These defects do not prove the cause of earlier hardware timeouts.

## Validation markers / boundary

Expected before the first display PUT:
`NVIDIA_BAR1_MAP` → `NVIDIA_CE_ROUNDTRIP_BEGIN` →
`NVIDIA_CE_ROUNDTRIP_DONE` (real fence + RAM/BAR1 data verification), followed by
`NVIDIA_DISPLAY_SCANOUT_READY ... path=bar1-uc` and the usual LUT/CRC/commit flow.
Live `NVIDIA_DISPLAY_LIVE_PATCH_BEGIN` now reports `path=bar1-uc`; DONE reports
`readback=batch-tail-only`, not full verification. Map, invalidate, RM, fence,
data or selected-link errors stop the session, retaining all backing.

A second operator attempt proved PD3[0] was occupied:
`0x1f382c02` decodes as a VRAM PDE to`0x1f382c000`. The original bootstrap
stopped at`require-unused-root`, with`root_publication_started=false`, before CE
or display PUT. The bootstrap was then corrected to preserve and walk that
existing hierarchy. New markers:`NVIDIA_BAR1_SHARED_ENTRY` (level/table/index/
value) and`NVIDIA_BAR1_SHARED_SLOT` (fresh entry + actual aperture VA). This
corrected walk was then run by the operator at17:00:42. It preserved the
existing PD3/PD2, installed the zero PD1[15] entry, and reported image/instance
mappings at BAR1 VA`0x1e0000000`/`0x1e1000000`. However, the first data operation
(zeroing16KiB of CE instance memory) failed its BAR1 tail-word checksum, before
CE channel creation or submission. Translation/data access is **not validated**.
`NVIDIA_BAR1_WRITE_MISMATCH` now reports the failing expected/actual word and VA;
`NVIDIA_BAR1_WRITE_TABLES` reports only the CPU's PDE/PTE view. Those new
mismatch diagnostics ran at17:54:05 after an operator-reported reboot and showed
expected0 / actual`0xbad0fb0d`, with CPU small PDE`0x3dd00c` and PTE`0x1ee5d309`.
The pinned SDK does not establish that word's precise meaning. The repeated
failure is not explained solely by the previous epoch. A new read-only binding/
instance-root/ADR_LIMIT guard (`NVIDIA_BAR1_BIND`, `NVIDIA_BAR1_CONTEXT`) has been
added before publication; it has not yet been run or proven to fix the failure.

A build is not BAR1 mapping, CE completion, speed, physical connectivity or
visible-pixel evidence. Existing PRAMIN results do not validate this path.
Any assistant hardware run requires separate authorization; the assistant made
no run/reset/rebind. The operator's failed attempt is recorded above.
