# Native NVIDIA / AD106 sketch

**Not a working GPU driver. No firmware has been uploaded or GPU passed through.**
Linux bring-up is parked; this is a separate native Horn driver effort.
The first hardware milestone is **GSP boot + a validated GetGspStaticInfo reply**,
not rendering, CUDA, display, or Mesa compatibility.

## Scope and code

Only Ada **AD106**, initially the user's Palit RTX 4060 Ti. Exact PCI device and
subsystem IDs are host-confirmed: **10de:2803 / 1569:f306**. The
`PALIT_RTX_4060_TI` allowlist exists, but no assignment rule is enabled.
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

Seven focused host tests exercise those guards, not hardware behavior. There is
no service executable, activation module, public GPU IDL, firmware pin or GSP
boot implementation yet. In particular there is no fake successful `boot()`.
See [DESIGN.md](DESIGN.md) for the port plan, blockers and exact source map.

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
