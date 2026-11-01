# Hardware evidence

Facts observed on the physical board. Nothing here is inferred from builds,
unit tests or parsers.

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
