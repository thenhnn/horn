# Hardware evidence

Facts observed on the physical board. Nothing here is inferred from builds,
unit tests or parsers.

## Current code versus evidence

The operator subsequently confirmed that the in-place PRAMIN color animation
was visible, but reported slow writes. Its run directory/timing has not been
correlated here; no bandwidth number is claimed.

The next change switches all driver-owned CPU VRAM access (image, live patch,
LUT, CRC and display/CE instance initialization) to BAR1 UC and adds a bounded
64-KiB CE RAM→VRAM→RAM preflight before the first display PUT. It has **not**
yet passed on the board. Existing visible-image, CRC and animation evidence
belongs to the earlier PRAMIN path, not BAR1/CE. See [BAR1-CE.md](BAR1-CE.md)
for the implementation, markers and remaining validation boundary.

### 2026-11-02 16:52:19 — first BAR1 attempt stopped before CE

Operator-provided output matches `work/nvidia-vfio-20261102-165219/serial.log`.
GSP initialization and RM allocation succeeded. BAR1 config: PCI index1,
aperture8589934592, shared root`0x1f382d000`, WPR limit`0x1f3d00000`.
Surface allocation at`0x1ee600000`, size`0x1000000`, was followed by
`Error::State` in the backend (last recorded clock3892371us). No BAR1_MAP,
CE roundtrip or first display PUT was reached.

The original markers do **not** distinguish PRAMIN root-read/selector failure,
an occupied root entry, or invalidate-status failure; nor do they establish
whether a root write had started. Occupied PD3[0] is a hypothesis, not an
observation. Backing was retained and supervisor reported CPU stop/cleanup
pending, not graceful GPU unload. No speed or pixel result from this run.
Phase, root-entry value and publication-start diagnostics were added afterward;
no overwrite permission, retry, fallback or assistant hardware run was added.

### Next operator attempt — occupied root confirmed

The supplied follow-up log records`NVIDIA_BAR1_ROOT_ENTRY` at`0x1f382d000`:
PD3[0]=`0x1f382c02`, an ordinary VRAM PDE to`0x1f382c000`. Failure was explicitly
`phase=require-unused-root`, `root_publication_started=false`, last RPC clock
3820619us. Thus this attempt made no shared-entry publication and never reached
CE or display PUT. This resolves the previous ambiguity for this attempt only.

The code was corrected to preserve existing PDEs and seek a revalidated zero
entry at a lower level within BAR1, never replacing an occupied ancestor.
Only ordinary VRAM table pointers are followed; foreign system memory, WPR,
cycles and client-allocation overlaps are rejected. No free PD1 segment or a
changed path is terminal. The new walk is build-checked; its first operator result is recorded below.

### 2026-11-02 17:00:42 — adopted hierarchy, first BAR1 data check failed

`work/nvidia-vfio-20261102-170042/serial.log` matches the next operator excerpt.
Existing PD3 points to`0x1f382c000`; its PD2[0] points to`0x1f382b000`.
PD1[15] was zero. Published fresh slot at table offset`0x78`, value`0x3dc70c`,
BAR1 VA`0x1e0000000`; image16MiB mapped there. CE instance allocation was
returned at`0x1ee5d0000`, size64KiB; its first16KiB were mapped at BAR1
VA`0x1e1000000`. Both mappings reported publication/invalidation success.

The next operation, BAR1 zero initialization of those16KiB, returned Checksum
from its batch-tail readback, before compute VMM/channel creation or CE submit.
The old marker lacks expected/actual data, so the reason is unresolved. A
BAR1_MAP marker proves software publication/acknowledgement, **not usable data
translation**. No image, CE completion or performance was demonstrated.
Added failing-word VA/offset/expected/actual and CPU-side PDE/PTE diagnostics;
the latter are not evidence of GPU table fetch. No further GPU read, rewrite,
retry, cache-mode change or alternative data path is introduced on mismatch.

### 2026-11-02 17:54:05 — same data failure after operator reboot

Operator reports a host reboot before this run; the excerpt matches
`work/nvidia-vfio-20261102-175405/serial.log`. GSP/FlushPage succeeded this time.
BAR1 zero write again failed: expected0, actual`0xbad0fb0d`, VRAM
`0x1ee5d0000`, offset`0x3ffc`, BAR1 VA`0x1e1003ffc`. CPU table observations:
PD0 index8, small PDE`0x3dd00c`, leaf IOVA`0x3dd0000`, PTE index3,
PTE`0x1ee5d309`. No CE channel/submission or display PUT was reached.

This reproduces the BAR1 defect after a reported reboot; attributing it solely
to a dirty previous GPU epoch is unsupported. The separate earlier FlushPage
failure remains unresolved. No authoritative meaning for`0xbad0fb0d` was found
in the pinned SDK; do not label it a specific MMU fault code.

Source review found a missing prerequisite: CPU BAR aperture length does not
prove the active BAR1 binding, page-table format/root, or GPU VA limit. Added a
read-only SDK-derived BAR1_BLOCK/instance-context check before any BAR1 table or
data publication, and cap VA allocation by both PCI size and the active instance
ADR_LIMIT. Reject physical/uninspectable/mismatched binding; do not reprogram it.
This is a correctness guard and diagnostic, **not yet a demonstrated fix** for
the observed checksum. The new guard is build-checked, not hardware-validated.

### Subsequent operator excerpt — FlushPage recurs before BAR1

Another operator excerpt stops at`0x100c10` readback0, clock2241263us,
with all staging allocations successful. No BAR1 binding check or CE code ran.
The prior firmware allocation is63541248 bytes, below Native's per-allocation
64MiB maximum; the excerpt does not establish DMA-budget exhaustion. Raising
that limit is not a fix for an ignored MMIO address-register write.

Audited the complete pinned Nova HAL dispatch: Ada selects GA102, which delegates
flush-page registration to GA100. **Both`0x100c40` (HI) and`0x100c10` (LO)
are intentional, HI before LO**. A preliminary suggestion to use TU102's low-only
40-bit path was wrong and is not retained. Compared known-good animation commit
`b93431ea` to BAR1 diagnostics commit`59f961d9`: falcon.rs has no diff;
boot.rs changes do not change the staging/handoff sequence before FlushPage.
This does not exclude timing, layout, or effects from previous GPU epochs.

Retain both register writes, 47-bit grant policy, strict full-address readback,
and fail-stop on mismatch. Added captured requested IOVA and raw low/high pairs
before/after the single attempt. Failure logging emits
`NVIDIA_FLUSH_PAGE_DIAGNOSTIC` from captured values only (no GPU access after
quarantine, no retry). Added source tests for write order, high bits, address
policy and ignored writes; tests remain unexecuted by operator request.
Build/format checks pass. **No FlushPage fix is established or hardware-tested.**

## 2026-10-30 — GSP cold boot and validated GetGspStaticInfo

Board: Palit RTX 4060 Ti, AD106, `10de:2803 / 1569:f306`, 8187 MiB usable FB.
Topology: host functions released to `vfio-pci`; GPU passed to the guest on the
root bus (`addr=0x8`, `rombar=0`), audio sibling not passed. Q35, split
irqchip, virtual Intel VT-d with interrupt remapping, 1 GiB guest RAM.
Firmware: pinned 570.144 (`gsp.bin` 63 541 248 bytes).

Two consecutive runs reached the same result. Guest serial markers:

```
NVIDIA_PCI bdf=00:08.0 board=10de:2803/1569:f306 bar0=16777216 msi_cap=0x68
NVIDIA_PROM bytes=1048576 sig=55aa
NVIDIA_PROM_LAYOUT rom=0x0 images=4 pci=Some((0, 64512)) fwsec=Some(155136) end=Some(635392)
NVIDIA_FWSEC_DESC len=346324 hdr=0x32c0301 version=3 size=812 stored=66688 imem=63232 dmem=3456 count=2 versions=0x3 engines=0x400 ucode_id=9 imem_dest=0x0 dmem_dest=0x0 pkc=0xb24 iface=0x1c
NVIDIA_GSP_LAYOUT fb=8585740288 frts=0x1ffa00000 wpr=0x1f3d00000
NVIDIA_GSP_STAGE image_bytes=63541248 cleanup_prepared=true
NVIDIA_GSP_IMAGE_STAGED bytes=63541248 elapsed_us=337796
NVIDIA_GSP_HANDOFF_BEGIN bus_master=true
NVIDIA_GSP_RISCV_ACTIVE init_done=false
NVIDIA_GSP_SEQUENCE bytes=6416 words=16354 commands=1594 rpc_sequence=0
NVIDIA_GSP_POST bytes=1212 rpc_sequence=0
NVIDIA_GSP_INIT_DONE
NVIDIA_GSP_STATIC_INFO bytes=1656 status=0x0 observed_fb=8585740288
NVIDIA_GSP_READY name=NVIDIA GeForce RTX 4060 Ti fb=8585740288 regions=5 transport_sequence=5 rpc_sequence=0
SUPERVISOR_SERVICE_READY job=1
NVIDIA_RM_READY client=0xc1d00000 device=0xde1d0000 subdevice=0x5d1d0000
```

What this establishes: FLR/VFIO reset, scoped BAR0 access, real PROM read and
VBIOS parse, production FWSEC/FRTS/WPR2, SEC2 booter, GSP RISC-V start, a
firmware-driven CPU sequencer executed against our MMIO, the shared-queue
transport in both directions, GspInitDone, and a validated static-info reply
(name, five framebuffer regions, FB size matching the register-derived value).

Above that, an RM client exists: `NV01_ROOT` → `NV01_DEVICE_0` →
`NV20_SUBDEVICE_0` allocated through `GSP_RM_ALLOC` with RMAPI handle ranges,
each reply checked against our own handle, class and parameter size.

On top of the client, the driver owns the GPU address space and a channel:

- `FERMI_VASPACE_A` with `IS_EXTERNALLY_OWNED`, page directory built by our own
  GMMU code (GA10x geometry) and handed to RM with `SET_PAGE_DIRECTORY`.
- `NV01_MEMORY_LOCAL_USER` for the channel instance, USERD and RAMFC, placed by
  RM at VRAM offset `0x1ee5c0000`; the reply is parsed past its 32-byte
  `NVOS21_PARAMETERS` header.
- `NV_CHANNELGPFIFO_ALLOCATION_PARAMETERS` with class
  `AMPERE_CHANNEL_GPFIFO_A` (`0xc56f`), channel id 1 (id 0 is RM's), the
  GPFIFO mapped into our page directory and passed as a VA, USERD slot 1 of the
  shared page, and a method buffer sized by
  `NV2080_CTRL_CMD_CE_GET_FAULT_METHOD_BUFFER_SIZE` (`0x5000`).
- The engine table from `NV2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE` lists
  GR0, SEC2, NVENC0, NVDEC0, CE0..CE4, OFA and SOFTWARE; CE0 is
  `RM_ENGINE_TYPE_COPY0` on runlist 0.

The channel is bound and schedulable: the copy-engine object is allocated with
`NVC0B5_ALLOCATION_PARAMETERS` (`version=1`, `ENGINE_COPY0`), then
`NVA06F_CTRL_CMD_BIND` and `NVA06F_CTRL_CMD_GPFIFO_SCHEDULE` are acknowledged on
the channel object itself. `GPFIFO_SCHEDULE` takes two `NvBool`s and `NvBool` is
`NvU8`, so its payload is two bytes; a wider payload is rejected.

`NVIDIA_CHANNEL_OK cid=1` is the boundary of this record: the channel object
exists in RM and accepts scheduling, but no engine has executed anything.

Display, which is a separate boundary:

- The VBIOS DCB names every connector and its DDC route; a bit-banged I2C master
  and a GM200-layout DP AUX channel are both implemented, and EDID over
  I2C-over-AUX works on real hardware. The panel behind the DisplayPort output is
  identified as an LG ULTRAGEAR, 70x39 cm, with 13 modes; the preferred one is
  2560x1440 at 119 Hz, 497.75 MHz, with 48/32/80 horizontal and 3/5/89 vertical
  porch/sync/back-porch splits. DPCD reports revision 1.2, HBR2, four lanes.
- The display RM objects that do work: a dedicated display client and device,
  the instance memory announced with `DISPLAY_WRITE_INST_MEM`, the
  `NV04_DISPLAY_COMMON` control object, `GET_SUPPORTED` and `GET_STATIC_INFO`
  (four heads, eight windows, 84 channels), and the `AD102_DISP` root.
- What does **not** work yet: the display channels. `AD102_DISP_CORE_CHANNEL_DMA`
  is rejected with `NV_ERR_INVALID_OFFSET` for every offset we tried, and the
  window and cursor classes answer `NV_ERR_GENERIC` (`0xffff`). Earlier notes
  incorrectly called this an invalid/unfilled status; open-gpu's
  `nvstatuscodes.h` explicitly defines it. `MAP_MEMORY_DMA` and `ALLOC_CTX_DMA`
  experiments returned `NV_ERR_INVALID_FUNCTION`, but sent an unverified payload
  (an RM-alloc envelope around NVOS46 parameters). This does not establish that
  either RPC or context DMA is unavailable. No mode has been programmed.
- The RM API is now generated from pinned 570.144 headers and uses typed
  allocation/control requests. Outer RPC result and inner RM status are checked
  separately. This is a software refactor, not new hardware success evidence.

Two things this record does **not** establish:

- **The VRAM aperture.** Reading `GP_PUT`/`GP_GET` and the RAMFC words through a
  BAR1 window returned a synthetic incrementing pattern (`0xbad0ac00`, then
  `0xbad0ac01`, ... one per read, regardless of offset), so that window is not a
  plain view of video memory. The USERD was therefore moved to system memory,
  where the driver publishes `GP_PUT` with a plain store and needs no aperture at
  all. What BAR1 actually maps here is still open.
- **Engine execution.** The copy submission builds a pushbuffer, publishes a
  GPFIFO entry, writes `GP_PUT` and rings the doorbell at `0xbb0090`, but no run
  has shown the engine retiring work: the only attempt so far timed out before
  the USERD moved to system memory.
Ready-to-ready took ~3.2 s; the 60.6 MiB staging takes ~0.34 s.

What it does **not** establish: MSI/GIN interrupt delivery (events are enabled
after READY, no interrupt has been observed), the STOP/unload path, boot
determinism beyond these two runs, and anything above the GSP — RM clients, GPU
memory, channels, faults, display.

Raw serial logs (gitignored, local to this checkout):

- `work/evidence/gsp-boot-2026-10-30-a.log`
- `work/evidence/gsp-boot-2026-10-30-b.log`
- original runs under `work/nvidia-vfio-20261030-1830*/serial.log`

Reproduce: `hardware/gpu/nvidia/run-vfio.sh` with the GPU already bound to
`vfio-pci` (`hardware/gpu/nvidia/host-gpu.sh status`).


## Operator follow-up — nouveau r570 core recipe accepted

The operator supplied a physical-board serial trace for `display-core.nix`
(570.144). Both root allocations used 120-byte SDK parameters. The dedicated
display subdevice was created, and the core ring was announced as 4 KiB:

```
NVIDIA_DISPLAY_RM step=subdevice handle=0x5d2d0000
NVIDIA_RM_VRAM_REPLY handle=0x420000 size=0x10000 offset=0x1ee5d0000 limit=0xffff
NVIDIA_DISPLAY_CORE_INPUT client=0xc2d00000 parent=0xc7700000 handle=0xc77d0000 pb=0x1ee5d0000 bytes=4096 size_class=0 get_put=0 subdevices=1
NVIDIA_DISPLAY_CORE_ALLOC_OK handle=0xc77d0000 get_put=0x0 size_class=0 subdevices=0x1
NVIDIA_DISPLAY_EXPERIMENT_DONE compute=skipped
```

This establishes successful core allocation, not command submission or a
modeset. RM rounded the backing allocation to 64 KiB while accepting a 4-KiB
ring. The run changed root parameters, the display subdevice and ring size
together; it does not identify which change resolved INVALID_OFFSET.
No window/cursor or compute requests were attempted in this run.


## Operator follow-up — core NOP and UPDATE prefixes consumed

The ordinary module, using a 4-KiB coherent sysmem ring at IOVA 0x3dc3000,
allocated the core with the same 120-byte roots and display subdevice:

```
NVIDIA_DISPLAY_CORE_REGS put_raw=0x0 get_raw=0x0
NVIDIA_DISPLAY_CORE_SUBMIT kind=nop put=4 put_raw=0x10
NVIDIA_DISPLAY_CORE_FETCHED kind=nop put=4 get=4 notifier=disabled
NVIDIA_DISPLAY_CORE_SUBMIT kind=update put=11 put_raw=0x2c
NVIDIA_DISPLAY_CORE_FETCHED kind=update put=11 get=11 notifier=disabled
```

This confirms consumption through the coherent DMA path and legacy MMIO
byte-pointer encoding. It does not confirm UPDATE completion, notifier writes,
link training, scanout or a visible image.


## Operator follow-up — core UPDATE completion through RAMHT notifier

The ordinary module completed the new third prefix on the physical AD106:

```
NVIDIA_DISPLAY_RAMHT_OK instance=0x1ee5c0000 notifier=0x3dc3000 bytes=4096 handle=0x1000
NVIDIA_DISPLAY_CORE_SUBMIT kind=notifier put=22 put_raw=0x58
NVIDIA_DISPLAY_CORE_FETCHED kind=notifier put=22 get=22 notifier=enabled
NVIDIA_DISPLAY_CORE_COMPLETE kind=update status=0x80000000 put=22 get=22
```

The preceding NOP and notifier-disabled UPDATE again reached GET=PUT at 4
and 11. Status 0x80000000 is the published C77D notifier FINISHED value.
This confirms the local RAMHT binding, coherent GPU notifier write and UPDATE
completion path, not merely command consumption. It does not establish
window setup, modeset, DP link training or visible scanout.


## Operator follow-up — surface allocated, software guard stopped before upload

The next-stage run again reached GSP READY and core allocation. Exact preferred
base-DTD geometry is now 2560x1440, 497750 kHz, totals 2720x1525, 119997 mHz,
porches H=48/32/80 and V=3/5/77. This supersedes the older vertical-total
reconstruction (and its 89-line back porch).

```
NVIDIA_DISPLAY_CORE_ALLOC_OK handle=0xc77d0000 get_put=0x0 size_class=0 subdevices=0x1
NVIDIA_RM_VRAM_REPLY handle=0x420000 size=0x1000000 offset=0x1ee600000 limit=0xffffff
NVIDIA_DISPLAY_RM_FAILED error=Unsupported
```

RM accepted the surface request and rounded 14745600 bytes to 16 MiB.
Size/alignment/limit fit the software policy; format/attribute validation
rejected before SCANOUT_BEGIN, PRAMIN upload, window allocation or new PUTs.
The log omitted format/attr, so it does not identify which metadata check
rejected. The epoch fail-stopped with DMA retained; no ownership/window or
scanout success is established by this run.

Source-derived follow-up: allocation `format` is a PTE kind; TU102 RM can
choose uncompressed GENERIC_MEMORY (6) for pitch-layout IMAGE memory. The
software guard no longer equates kind zero with linearity. Scanout requests
now explicitly require contiguous VIDMEM/PITCH/COMPR_NONE, verify those fields
and allow only the published uncompressed pitch/generic kinds, with full
size/relative-limit/alignment/address checks before upload. Instance allocation
policy is unchanged. Reply/rejection logs now expose kind and attributes.
This is not proof that kind 6 was returned by the failed hardware run.

The follow-up native Nix kernel/root-task build succeeded; log:
`work/nvidia-surface-policy-build.log`. Tests and VM were not run. Ownership
completion, surface upload/readback and window assembly still require a new
operator-authorized run; DP/head modeset remains unimplemented.


## Authorized VFIO run — ownership and surface preparation confirmed

Log: `work/nvidia-vfio-20261102-034416/serial.log`. Invoked through the
operator-authorized `doas hardware/gpu/nvidia/run-vfio.sh`; QEMU was ultimately
terminated by host signal 15 (btop), not graceful STOP/unload.

- Surface reply: size 0x1000000, offset 0x1ee600000, limit 0xffffff,
  attr 0x11800000, attr2 0x100005, kind 6; requested attr 0x10000000.
- SCANOUT_READY: entire 16 MiB uploaded/read back; no active scanout.
- Ownership: FINISHED 0x80000000, GET=PUT=89, fresh notifier byte offset 16.
- Window 0 allocated; NOP GET=PUT=4; assembly GET=PUT=48.
- PIPELINE_PREPARED: head 0/window 0, image handle 0x2000,
  dp=untrained, modeset=not-submitted. No visible-image claim.

Kind 6 is now actually observed for pitch-layout memory. CPU PRAMIN per-DWORD
write/readback is intentionally slow (~5.3 seconds for 1408 active rows);
it is diagnostic upload, not scanout throughput.


## First preferred modeset — trained link and completion, operator sees black

First attempt: `work/nvidia-vfio-20261102-040205/serial.log`.
RM connector correlation selected display ID 0x100 (not DCB bit zero),
connector 0, SOR 0, link A. Source caps HBR3; DCB HBR3/four lanes with override
enabled; chosen sink link HBR2/four lanes. Watermark 23, HBlankSym 144,
VBlankSym 2720. Surface and identity LUT upload/readback, ownership and window
assembly succeeded. A direct native AUX D0 write failed the local aux-reply
check before DP training or final UPDATE; raw AUX status was not logged.
This does not establish that the sink rejected the write. No retry in epoch.

Follow-up: `work/nvidia-vfio-20261102-040717/serial.log`.
Post-acquisition AUX now uses typed RM DP_AUXCH_CTRL (pinned-header generated
layout, field-wise codec, ACK and actual returned length checked).
Native Nix build succeeded as part of the authorized run; no unit tests run.

```
NVIDIA_DISPLAY_RM_AUX_REPLY addr=0x600 reply=0x0 bytes=1 retry_ms=0
NVIDIA_DISPLAY_DP_TRAIN_REPLY id=0x100 cmd=0x2083 data=0x1404 err=0x0 retry_ms=0
NVIDIA_DISPLAY_DP_LINK_STATUS config=[14, 84] status=[77, 77, 01, 03, 66, 66]
NVIDIA_DISPLAY_DP_TRAINED id=0x100 sor=0 rate=0x14 lanes=4 tu=64 watermark=23
NVIDIA_DISPLAY_MODESET_SUBMIT head=0 window=0 core_put=139 window_put=60 core_slot=32 window_slot=64
NVIDIA_DISPLAY_MODESET_COMPLETE head=0 window=0 core_status=0x80000000 window_status=0x40000000 visible=operator-unconfirmed
```

All four lanes report CR_DONE/CHANNEL_EQ_DONE/SYMBOL_LOCKED and interlane
alignment. Core FINISHED and window BEGUN establish the interlocked commit,
not visible pixels or buffer retirement. Operator explicitly reports a black
screen; monitor recognized DP. **No successful visible-image claim.** Initial
DPCD/EDID discovery still uses native AUX; post-SOR traffic is exclusively RM.
Both runs ended via the new script-internal 30s timeout, exit 124, without
graceful driver STOP/unload. Do not infer safe resource reclamation from that.

### Read-only method shadows and running raster

`work/nvidia-vfio-20261102-041727/serial.log`: same immutable modeset, plus
read-only assembly/armed method snapshots using the pinned SDK core bases and
nouveau `gv100_disp_wndw_mthd.prev` (+0x800). All sampled pairs match: raster
2720x1525, viewport/image 2560x1440, clock 497750000 Hz, SOR0=0x801,
image DMA=0x2000, offset=0, pitch=160 units of 64 bytes, format=0xcf,
alpha=255 and factors=0x22; CSC C00=0x10000. ILUT/OLUT handles=0x3000,
relative offsets=0, controls=0x40508/0x40509, OCSC0/1 both disabled.
DPCA changed 0x000804ad -> 0x000b04fa over 25 ms: three frames, consistent
with the preferred ~120 Hz **internal** raster. This does not prove output
MSA/M/N, stream validity at the sink, or pixel fetch correctness.

Operator reports DP detected, OSD **0 Hz**, then monitor went dark after a few
seconds. Earlier report of a received signal is not evidence of a recognized
valid preferred mode; investigate DP stream as well as pixels. QEMU limit is
now 60s (+3s kill grace), build excluded. This invocation ended with shell
`Killed`/aborted rather than a normal exit-124 report; a subsequent process
query found no remaining QEMU/timeout/run-vfio process. The cause/timing of the
monitor darkening is not established. Native Nix build passed, no unit tests
run. No retry/reset or active-buffer changes made during the snapshot.

### First operator-confirmed visible image (not yet stable)

`work/nvidia-vfio-20261102-042511/serial.log`: operator explicitly reported
**a visible image**, followed by a crash. Same preferred 2560x1440 ~120 Hz
RGB8 timing, image/LUT programming and interlocked commit as above. No claim
that this run isolated the cause of the earlier black screens.

The expanded pre-commit DPCD read returned
`[14, 84, 00, 0a, 0a, 0a, 0a]`: HBR2/four enhanced-framed lanes, training
pattern disabled, scrambling enabled; all lanes still CR/EQ/symbol locked.
Read-only source state: watermark=0x08000017, hblank=0x90, vblank=0xaa0,
SOR clock=0x03890040, link-control=0x000f4001, pattern=0x50505050
(low five bits per byte are normal-video pattern 0x10 in gm107_sor_dp_pattern).
These are not a direct measurement of MSA or M/N.

The newly added optional post-commit DPCD 0x100 read (500 ms later) returned
RM `NV_ERR_GENERIC` (0xffff), propagated to the existing fail-stop panic;
`dma_retained=true`, supervisor CPU stopped, cleanup pending. Why RM rejects
this active-display AUX request is **not established**. There was no retry or
in-epoch recovery. Script ended with exit 137 after TERM/kill grace, not an
orderly driver STOP/unload.

Fix for the next fresh boot: remove the optional post-commit AUX RPC, retaining
strict pre-commit DPCD/training checks, fresh notifiers, read-only shadows,
backing retention and normal event service. Do not merely swallow RM errors.
Follow-up `work/nvidia-vfio-20261102-042832/serial.log` hardware-validates
the removal: same training/commit/source state, no recorded panic, script exit
137. Operator reports a **black screen**, not a visible image. Thus removal
avoids the observed diagnostic-query panic but does not establish reproducible
scanout. No causal claim about the AUX request making pixels visible. Stable
display remains unproven.

### Restored post-commit AUX with timing/event logs — black again

`work/nvidia-vfio-20261102-043158/serial.log`: operator reports a black screen
with the restored 500 ms delayed AUX diagnostic. Thus the request is not a
sufficient condition for reproducing the one visible-image run. Modeset,
image/LUT and UPDATE recipe were unchanged; added logging affects timing.

Commit began at 10158565 us, completed about 23 ms later. Before AUX,
clock=10775135 us (616570 us after submit), core PUT=GET=0x22c,
window PUT=GET=0xf0, DPCA=0x00490318, core FINISHED=0x80000000,
window BEGUN=0x40000000. Internal raster continued advancing during the delay.
DPCD 0x100 read request cmd=0x9, size=6 (seven bytes) received the matching
RM_CONTROL reply, transport sequence 42, outer status/private status both 0.
The decoded RM control status was NV_ERR_GENERIC=0xffff; no AUX bytes were
accepted as valid. The two-read diagnostic stopped at its first request;
no status read/retry followed. No intervening GSP event was logged in this
RPC wait. Fail-stop retained DMA backing; supervisor CPU stopped, cleanup
pending. Native build passed, no unit tests run. QEMU invocation ended
aborted/Killed, not graceful STOP/unload.

### Passive formatter/raster/underflow snapshots

`work/nvidia-vfio-20261102-043956/serial.log`: one 60 s invocation, exit 124;
operator confirms a black screen again. Native build passed; no tests.
Added only passive register reads and method-shadow reads, no modeset change.
Register-map reference: NVIDIA/open-gpu-doc commit
`9fdf5c4062007929d9f4e6cbad9c9771fe61b880`,
`manuals/ampere/ga102/dev_display_withoffset.ref.txt`. Interpretation on AD106
is diagnostic; it is not an independent measurement of sink-valid video.

Before UPDATE, RG/SF opmodes were sleep and both loadv counters were zero.
After commit and 500 ms later: RG_STATUS=0x58008000 (awake, not stalled),
SF_TEST=0x48600 (awake, head-0 owner), RG and postcomp loadv counters advanced
in step from 2 to 0x4d. RG_UNDERFLOW remained 1: reporting enabled, neither
pixel nor loadv underflow sticky bit set, zero underflowed frames.
SF_DP_IRQ stayed zero. No error flags were cleared by this diagnostic.
SF_DP_LINKCTL=0x000f4101: enabled, TU64, asynchronous, enhanced framing,
four-lane mask. SF_DP_MN=0x8000: N=32768, no M delta/modification. This is
configuration, **not measured Mvid/Nvid or on-wire MSA**. Published
MSA_DEBUG_MAUD/NAUD registers are audio, not video, and were not used.
Raster sync/blank assembly and armed values matched. Supervisor head word
was 0x11100 after commit; it is not treated as actual output blank status.

The retained AUX read again received a matching GSP response with outer
status/private status 0 and inner RM NV_ERR_GENERIC; fail-stop retained backing
and supervisor stopped the CPU. No retry, extra UPDATE, reset or orderly unload.
Internal progress and absence of sampled underflow do not prove visible pixels
or exclude later faults after the diagnostic stops the driver.

### Combined baseline / active-ID / subscribed hotplug diagnostic

`work/nvidia-vfio-20261102-045358/serial.log`: operator confirms black throughout
and no cable, monitor-power or input manipulation. One authorized 60 s run,
exit 137 after TERM/kill grace, no orderly STOP/unload. Native build and selected
format/diff checks passed; no unit tests. Audit: `DISPLAY-AUDIT.md`.

Both scoped event subscriptions succeeded (HOTPLUG and DP_IRQ). GET_ACTIVE
before route returned zero for all four heads in both any/NV-client scopes.
Before our first UPDATE, head 0 had clock/displayID zero, owner=0xf (unowned),
RG/SF asleep, no raster/loadv progress and no image/LUT DMA. Thus no active
inherited display was observed; this does not prove every unused field was reset.

After commit, GET_ACTIVE reported expected 0x100 in both scopes at each timed
sample. Raster and paired loadv counters advanced; no sampled underflow or
SF_DP_IRQ error bits. The optional post-commit AUX was not issued. Observation
completed and normal service entered at 13491644 us without recorded panic.

Crucially, subscribed RM notifications reported:

| clock_us | Notification |
|---|---|
| 10913407 | HPD plug=0, unplug=0x300 (includes selected display 0x100) |
| 12558082 | HPD plug=0x100, unplug=0x200 |
| 12589633 | DP_IRQ displayId=0x100 |
| 18928759 | HPD plug=0, unplug=0x300 |
| 19439113 | HPD plug=0x100, unplug=0x200 |

Commit completion was at 10311370 us. The first unplug was about 0.6 s later,
then replug about 2.2 s after completion. RM active-ID remained 0x100 even after
its unplug notification: logical active topology does not prove a connected
sink. These are firmware notifications, not a direct electrical HPD measurement;
0x300 is not assumed to mean two physical monitors. No recovery/retraining or
additional UPDATE was attempted. Notification decoding worked in polling and
normal service. Earlier unregistered event logs could not exclude these events.

Operator suggests the monitor may reconnect while waking from sleep, and recalls
that the one earlier visible-image run may have happened before it fell asleep.
This timing recollection is tentative. Wake-induced hotplug could explain the
late AUX failure, but an invalid output stream causing sink cycling is also
possible; causality and stable scanout remain unproven. A future experiment can
separate wake/settling from the first training/UPDATE instead of retrying an
already-active uncertain display epoch. That experiment was subsequently implemented and run below.

### Pre-first-UPDATE wake/settle gate — black again

`work/nvidia-vfio-20261102-050431/serial.log`: operator confirms black screen.
One authorized 60 s run, exit 137 after TERM/kill grace; no orderly unload.
Native build, selected rustfmt and diff checks passed; unit tests skipped.

Wake began at 10246302 us. DPCD SET_POWER already read 1 (D0), so no power
write was issued. DEFAULT detection and subsequent CACHED connect samples
reported connected. No subscribed HPD/DP_IRQ was recorded during the settling
interval. Final full detection and D0 readback passed. WAKE_READY at 13277725 us,
3031423 us after wake began, before any training or UPDATE.

One training and the unchanged preferred-mode commit completed at 13404869 us.
RM reported unplug=0x300 at 13995904 us (591035 us after completion), then
plug=0x100/unplug=0x200 at 15642363 us (2237494 us after completion), and DP_IRQ
for 0x100 at 15652696 us. A further unplug/replug occurred at 22006273/22517010 us.
Internal raster/loadv progressed, sampled underflow remained clear, and active
ID stayed 0x100. Normal service entered at 16606747 us without recorded panic.

Comparison: in 045358 the first unplug was 602037 us after commit, replug
2246712 us after commit. Delaying training/commit by about 3.1 s shifted the
first unplug by about the same amount; post-commit intervals stayed close.
This supports investigating training/stream activation or sink acquisition,
rather than merely extending a pre-modeset delay. Training and commit are too
close together here to identify which precedes the relevant trigger. D0 and
quiet connected state did not prove the panel was awake; the sleep hypothesis
is not conclusively disproved, but this gate is insufficient. No retraining,
extra UPDATE, notifier reuse or reset was attempted; backing retained. No QEMU
remains. Russian handoff/report: `DISPLAY-REPORT-2026-11-02.md`.

### Prepared follow-up: four checks, no launch

The operator explicitly requested training-only observation, post-replug link
checks, source-backed initial ordering correction and internal CRC together,
but prohibited launching while the host driver is still loaded. Implemented
and native-built; **no GPU access or hardware run performed**. See
`DISPLAY-DIAGNOSTIC.md` for exact sequence, bounds, source basis and limitations.
Unit tests skipped; selected formatting/diff checks passed. Hardware status is
still the black-screen observation from 050431, not a claimed fix.

The operator supplied a photo of the successful moment:
`/home/user/.cache/pi-sandbox/49ec0cce9e90beb45119a93c05fd9e4461738fc2dbe6791102db1643d634d846/pi-clipboard-e317b883-a74c-447a-ad4d-797b2934fdb9.png`.
It visibly shows the test pattern: white/yellow/cyan/green/magenta/red/blue/black
bars, lower checkerboard and red border. This supports the operator's earlier
image confirmation. The photo alone does not establish refresh/timing, event
ordering or why only that run worked. The image is not copied into the repo.

### Four-part diagnostic run 151518 — operator confirms image

`work/nvidia-vfio-20261102-151518/serial.log`: one authorized 60 s run, exit124
on timeout TERM, not graceful STOP/unload. Operator confirms visible image.
DPCD power initially2, one successful conditional D0 write; connected/quiet gate
passes. Training/SST succeeds; no selected HPD/DP_IRQ is logged during the3 s
training-only interval or later in this run. Thus deferred reconnect probes were
not exercised. Ownership publishes at16465981 us, commit completes16541089 us.
Raster/loadv progress with no sampled underflow; GET_ACTIVE remains0x100.
CRC detach completes: status0x018a0001, count394, DONE, no reported overflow.
Initial SF CRC0xfe34a8d1; final entries390..393 consistently0xd1e9b9e9.
Expected pattern CRC is unknown; operator observation establishes visible pixels.
Normal service entered19800997 us, no recorded panic. This is one successful
run, not proof of repeatability or isolation of the causal change: D0 write,
training-only delay, sequence correction and CRC instrumentation differ from
prior runs. User requested instructions for independent launches; no automatic
repeat is authorized or performed.

### Intermittent empty startup logs, then training-only link loss

Operator launches `151635` and `151809` produced empty serial logs and reached
QEMU's timeout, with no captured QMP/stacks. The former used the same root image
as successful151518. Operator supplied only ordinary VFIO vgaarb messages, not
reset errors. These empty logs do not distinguish blocked QEMU/VFIO startup from
a silent guest. No reset failure is established.

One subsequent authorized diagnostic run:
`work/nvidia-vfio-20261102-152343/serial.log`, exit124 at60s, no STOP/unload.
The startup hang did not reproduce: seL4/GSP/RM loaded. The new read-only observer
was **skipped because python3 was absent from doas PATH**, so no QMP/stacks were
captured. No claim of QMP verification. No second run was made. Interpreter
resolution is subsequently fixed to use pinned Nix Python and fail before
launch if unavailable; bash/Python syntax and native Python package resolution
checked only, not hardware-run with that fix. No QEMU remains after this run.

This run nevertheless isolates the HPD issue BEFORE ANY UPDATE:

- DPCD power already1, no D0 write. Wake gate passes13292794 us.
- Training begins13296260 us, successful reply13307467 us; AUX config
  `[14,84,00,0a,0a,0a,0a]`, status`[77,77,01,03,66,66]`; SST reply13329897 us.
- Training-only observation begins13334637 us; raster/head asleep, no UPDATE.
- HPD unplug0x300 at13978689 us, replug0x100/unplug0x200 at15609599 us.
- Deferred probe at15815121 us: fresh connection=true; both AUX calls succeed.
  Config`[14,04,00,00,00,00,00]`, status`[00,00,00,02,00,00]`:
  configured=false,equalized=false,retrain_candidate=true,current_generation=true.
- Training-only interval ends16335085 us, revision1→3. Fail-stop Error::State,
  supervisor stops driver CPU; backing retained. No ownership/modeset/CRC
  activation or detach was submitted. CRC backing was prepared but not enabled.

Unplug is about0.64s after the successful training/SST sequence, without head
activation. Head timing/window UPDATE is therefore not necessary for this HPD
cycle. This supports investigating sink wake/link loss and a source-backed
pre-first-UPDATE revalidation/retraining state machine. It does not prove the
physical cause, authorize automatic recovery, or explain empty QEMU logs. No
operator pixel observation for152343 is recorded yet.

### Prepared after 152343 — build-only, not new hardware evidence

Operator approved implementation of one bounded pre-first-publication recovery.
It requires a confirmed single unplug/replug after successful training, quiet
connection, fresh RM DPCD/RAW EDID with unchanged capabilities/preferred timing,
and valid replies confirming lost config/EQ. A second loss or any RPC error
stops. First PUT (including NOP) permanently prohibits recovery. No DMA/ring/
notifier reuse, reset or active-head recovery. See DISPLAY-DIAGNOSTIC.md.
Native build passed (`work/nvidia-reconnect-build.log`); tests skipped as requested.
**No hardware launch:** neither this recovery nor the pinned-Python observer fix
has hardware validation. Most recent physical evidence at that point was 152343.

## 2026-11-02 15:42:34 — visible image after one pre-publication recovery

One authorized launch: `work/nvidia-vfio-20261102-154234/serial.log`.
Root: `/nix/store/9zjwmsygk6w923wmb0jx1089xsfj894r-rust_devmgr-demo-root-0.1.0`.
**Operator confirmed an image and manually sent SIGTERM.** QEMU reports signal15
from btop; script exited0, not timeout124. No QEMU remained on inspection.
This is not graceful guest STOP/unload and does not prove safe restart.

- Power already D0; no wake write. First training begins13460409 us, succeeds
  with config`[14,84,00,0a,0a,0a,0a]`, status`[77,77,01,03,66,66]`.
- Before any UPDATE: unplug0x300 at14143330 us, replug0x100/unplug0x200 at
  15777620 us. Deferred probe confirms lost config/EQ in successful replies:
  config`[14,04,00,00,00,00,00]`, status`[00,00,00,02,00,00]`.
- Recovery waits for quiet; fresh connection/DPCD/D0 and RAW EDID384 bytes pass.
  Exact preferred timing and capabilities unchanged. Fresh link again lost.
- Sole recovery allowance consumed17844502 us. New training starts17847454 us:
  config`[14,84,00,02,02,02,02]`, status`[77,77,01,03,22,22]`.
- Second training-only observation lasts3 s, revision3 unchanged. Fresh link
  at20907578 us is configured/equalized; READY20911500 us. No selected HPD/IRQ
  is logged after this recovery through the end of the run.
- First PUT20917342 us; ownership20929772 us; commit completes21005375 us.
  CRC detach reaches DONE, count394, no reported overflow; final SF entries
  390..393 are`0xd1e9b9e9`, matching the earlier visible151518 run. Expected
  pattern CRC remains unknown; operator observation establishes visibility.
  Normal service begins24265174 us; backing retained.
- Pinned-Python observer now works: `qemu-diagnostic.log` contains /proc state,
  kernel stacks and QMP status/vCPU/register snapshots at3/15/35 s. QMP reports
  running. Empty-serial startup failure did not reproduce; its cause is unresolved.

This demonstrates one successful bounded pre-publication recovery with visible
output, not repeatability or a physical explanation of the original HPD cycle.
No reset/rebind, active-head recovery or automatic repeat was performed.

## 2026-11-02 15:45:40 — shell launch stopped by a signal before serial output

Operator's independent shell launch: `work/nvidia-vfio-20261102-154540/`.
Serial is0 bytes. Observer found QEMU pid78419 at3/15 s with all three threads
in `State: T (stopped)` / `do_signal_stop`. Main thread's recorded syscall is
ioctl on fd0 (`0x402c542b`, terminal settings). QMP socket connects, but greeting
times out. No vCPU thread is present in these snapshots. No QEMU remained when
inspected afterward. The exact stopping signal was not captured.

This run is stopped, not evidence of a VFIO reset wait or executing silent guest.
The combination strongly suggests terminal job control: timeout's default new
process group plus QEMU `-serial stdio` terminal configuration can trigger SIGTTOU.
It does not retroactively establish the cause of earlier empty-serial runs.
The `<nixpkgs>.bashInteractive` lookup warning is followed by shell fallback and
is not evidence of this stop.

Prepared fix: use `timeout --foreground`, preserving the foreground job group
and the unchanged QEMU60s/+3s limit. This limits QEMU itself, not descendants;
the observer remains independently bounded. Bash syntax/diff checks passed.
No new QEMU/hardware launch or target signal was issued to validate this fix.
