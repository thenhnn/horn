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
each reply checked against our own handle, class and parameter size. No GPU
memory, VA space, channel or engine has been allocated yet.
Ready-to-ready took ~3.2 s; the 60.6 MiB staging takes ~0.34 s.

What it does **not** establish: MSI/GIN interrupt delivery (events are enabled
after READY, no interrupt has been observed), the STOP/unload path, boot
determinism beyond these two runs, and anything above the GSP — RM clients, GPU
memory, channels, faults, display.

Raw serial logs (gitignored, local to this checkout):

- `work/evidence/gsp-boot-2026-10-30-a.log`
- `work/evidence/gsp-boot-2026-10-30-b.log`
- original runs under `work/nvidia-vfio-20261030-1830*/serial.log`

Reproduce: `platform/drivers/nvidia/run-vfio.sh` with the GPU already bound to
`vfio-pci` (`platform/drivers/nvidia/host-gpu.sh status`).
