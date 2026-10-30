# Native NVIDIA / AD106 bring-up

**Not a working GPU driver. No firmware has been uploaded or GPU passed through.**
Linux bring-up is parked; this is a separate native Horn driver effort.
The first hardware milestone is **GSP boot + a validated GetGspStaticInfo reply**,
not rendering, CUDA, display, or Mesa compatibility.

## Scope and code

Only Ada **AD106**, initially the user's Palit RTX 4060 Ti. Exact PCI device and
subsystem IDs are host-confirmed: **10de:2803 / 1569:f306**. The
`PALIT_RTX_4060_TI` allowlist is enforced by an opt-in native probe task;
no NVIDIA assignment is enabled in the default profile.
No match-all NVIDIA driver, guessed VRAM size, or hardcoded BAR physical address.

`horn-nvidia` is an allocation-free `no_std` library in the workspace:

- `src/probe.rs`: host-confirmed board allowlist, AD106 identification via BOOT0/BOOT42,
  read-only bounded UC BAR0 view, privilege-gated GFW progress reads.
- `src/address.rs`: separate IOVA, VRAM offset and GPU VA numerical domains;
  whole-range 47-bit DMA-mask checks. These values grant **no authority**.
- `src/rpc.rs`: one-outstanding-request local ticket/function matching, absolute
  deadlines, no local tag wrap, and permanent quarantine on timeout/clock failure.
  It is **not** a GSP wire parser; calling it cannot establish GSP readiness.

```sh
nix-shell --run 'cargo test --offline -p horn-nvidia'
```

Seven focused host tests exercise those guards, not hardware behavior.
There is still no public GPU API. The native boot/RPC path is now connected but
has not run against hardware. A matching
570.144 firmware package is now pinned; see below.
In particular there is no fake successful `boot()`.
See [DESIGN.md](DESIGN.md) for the port plan, blockers and exact source map.

## GSP firmware inputs

`gsp/` is the separate **GPL-2.0-only** NOVA-derived port, not the original BSD
probe library. See [source/license provenance](gsp/PORTING.md). It parses the
pinned 570.144 TLVs and prepares original production booter signatures. The
VBIOS snapshot parser locates production FWSEC and prepares FRTS/SB working
copies; no board VBIOS snapshot has been exercised yet. The Falcon core now
ports AD106 reset, DMA load, RSA3K BROM parameters and FWSEC-FRTS/SB execution
checks, with a mandatory retained sysmembar DMA page. Its native MMIO/clock/DMA
boot adapter is connected to task execution; this is **not** hardware initialization evidence.
The native read-only task now uses the parser on a real PROM snapshot when GFW
is complete; no such snapshot has yet been read on the physical board.
Every wait has a fixed deadline; uncertain failures quarantine both engines.
The four register/deadline unit checks do not emulate firmware or RPC.

```sh
nix-build nix -A nvidia.firmware --argstr profile pc-dev \
  --arg modules '[ ./platform/drivers/nvidia/module.nix ]' --no-out-link
```

The build runs the native host checker against the **actual** five firmware
inputs and includes NVIDIA's license/WHENCE. It does not run firmware or touch
the GPU. `gsp.bin` is a 63,541,248-byte raw image, not ELF. Firmware is not yet
embedded into the small probe task: ELF/staging/DMA RAM budgets must be raised
explicitly with the boot adapter. Source inputs in `work/` are not dependencies.

## Native read-only task (opt-in)

`task/` builds `nvidia_probe.elf`, using named `DeviceDriverCaps`, the normal
VM-backed runtime and generic supervisor. `module.nix` selects only 10de:2803
display devices; the task rejects any subsystem other than 1569:f306 **before
MMIO**. It reads real BOOT0/BOOT42 and one privilege-gated GFW snapshot through
two 4-KiB UC BAR0 windows (offsets 0 and 0x118000), then explicitly closes them.
Only if that sample reports GFW complete, it copies the bounded 1-MiB BAR0 PROM
shadow (0x300000) through **one reusable 64-KiB window at a time**. Every close
acknowledges all-alias revocation and releases VM/cap resources before the next
map. No PCI ROM BAR is enabled and no x86/EFI code is executed. It never maps
BAR1/BAR3 or enables bus mastering, DMA, MSI delivery, firmware upload or reset.

The combined task binary is **GPL-2.0-only**, because it now uses the separate
NOVA-derived parser; original BSD guard/glue source keeps its own license.
See [GPL text and provenance](gsp/PORTING.md).

`NVIDIA_PROBE_READY` and publication under
`/diagnostics/nvidia/<guest-BDF>` mean **the read-only diagnostic is available**,
not GSP readiness. Its small diagnostic IDL returns the cached initial sample,
including explicit `bus_master=false` / `gsp_booted=false` and VBIOS state
(skipped for incomplete GFW, production FWSEC parsed, or rejected). Parsed size,
engine/ucode IDs and signature bitmap are metadata, **not authentication**.
`NVIDIA_VBIOS_PARSED` likewise never means firmware ran. GFW may still be
locked/in progress; this task does not pretend a single sample is a timed GFW
wait. GSP bring-up needs named clock/alarm grants and the firmware/lifetime
adapter next. A STOP closes the assignment through devmgr's acknowledged
MMIO revocation/teardown before signaling STOPPED.

Build kernel and root ELF (no GPU/host access performed):
```sh
nix-build nix -A sel4.out -A root-task.package --argstr profile pc-dev \
  --arg modules '[ ./platform/drivers/nvidia/module.nix ]' --no-out-link
```

Both seL4-target task and embedded root build successfully. No passthrough run
has been performed. A missing GPU does not synthesize one or emit probe READY.

The module gives this assignment explicit 64-MiB DMA / 64-object ceilings for
the GSP adapter, without changing virtio defaults. This opt-in module defaults to
`nvidia.boot = true`: assigning the target GPU starts firmware execution.
Set `nvidia.boot = false` for the old read-only diagnostic mode. Boot mode uses
256-MiB task RAM / 192-MiB devmgr RAM and a 32-MiB root heap; use at least 1 GiB
guest RAM for the initial run. No DMA is allocated by read-only mode. Devmgr now supports remapped single MSI and fixed-chunk
contiguous IOVA reservations; GIN handling and actual GPU IRQ/DMA still need
hardware validation.

For the first VFIO run use KVM/Q35 with split irqchip, virtual Intel VT-d
(`intremap=on,caching-mode=on,device-iotlb=off,aw-bits=48`) and put **only the
GPU function on the guest root bus**, not behind a PCIe bridge. Disable guest
option-ROM exposure with `rombar=0`; do not enable legacy `x-vga`. Guest BDF/BARs
are discovered, not copied from the host. Root-bus sibling functions are not
supported by this provisioning profile. Coordinate host display/audio shutdown,
VFIO binding and reset scope explicitly first; this module performs none of
those operations. Preserve serial `NVIDIA_*`, supervisor and IOMMU diagnostics;
a build is not physical-device evidence.

## Reference, not a build dependency

NOVA `drm/rust/kernel`, branch `drm-rust-next`, commit
`3497a403264b094ebd61b8b755165d56e7d9dc37`:

- checkout: `work/nova/` (ignored research material);
- [source at the reviewed revision](https://gitlab.freedesktop.org/drm/rust/kernel/-/tree/3497a403264b094ebd61b8b755165d56e7d9dc37/drivers/gpu/nova-core);
- Linux Rust PCI/DMA/IO/lifetime wrappers are **not** standalone crates to link
  into Horn. Reuse the hardware/protocol implementation with explicit platform
  adapters rather than implementing a fake Linux kernel API;
- NOVA source is GPL-2.0 with individual copyright notices. The current original
  guards are BSD-2-Clause; any actual NOVA source import must retain its license,
  copyright and revision provenance. Do not relabel a port as BSD.

The reviewed GSP wire bindings are **r570_144**, despite newer firmware work
existing elsewhere. Do not mix these structures with r000 or a random installed
host driver's firmware. The TLV packaging and wire ABI are separate concerns.

## Host observations (user report; not guest configuration)

| Item | Observed |
| --- | --- |
| GPU | `01:00.0`, AD106 RTX 4060 Ti, PCI revision a1 |
| Board | Palit, GPU `10de:2803`, subsystem `1569:f306` |
| GPU BAR0 | 16 MiB, memory32, non-prefetchable |
| GPU BAR1 | 8 GiB, memory64, prefetchable |
| GPU BAR3 | 32 MiB, memory64, prefetchable |
| GPU BAR5 | 128 bytes, IO ports; not used by this sketch |
| Audio | `01:00.1`, `10de:22bd` AD106 HDA, 16 KiB memory BAR |
| IOMMU | Host group 12 contains exactly GPU `0000:01:00.0` and audio `0000:01:00.1` |
| Current owners | `nvidia` and `snd_hda_intel`, both bus mastering enabled |
| Interrupt capability | MSI at 0x68, one message, 64-bit, no per-vector mask; no MSI-X advertised |
| Reset | `reset_method`: `flr bus`; PCIe FLReset+; successful reset/rebind not tested |

An 8 GiB BAR aperture does not by itself prove usable VRAM capacity. BAR1 is a
window, not free RAM, and is not mapped in its entirety by the future driver.
Host BAR addresses and BDF are not guest addresses or capability authority.

Read-only inventory commands (PCI/group/reset outputs already supplied):

```sh
sudo lspci -Dnnvv -s 0000:01:00.0
lspci -Dnn -s 0000:01:00.1
ls -l /sys/kernel/iommu_groups/12/devices/
cat /sys/bus/pci/devices/0000:01:00.0/reset_method
nvidia-smi --query-gpu=name,driver_version,vbios_version,memory.total --format=csv
```

Confirm the PCI domain if it is not 0000. No unbind, reset, ROM-enable write,
ACS override, or host configuration change is part of these commands. Group
viability and reset scope include audio: it must not remain an active host DMA
peer while the GPU is assigned. This does not mean the guest must run an audio
driver. SSH provides recovery access, not a guarantee that a GPU reset succeeds.
