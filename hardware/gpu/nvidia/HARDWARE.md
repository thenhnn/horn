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
