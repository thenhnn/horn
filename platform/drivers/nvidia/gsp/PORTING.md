# Native GSP source/provenance

This crate is **GPL-2.0-only**, not covered by the surrounding original
BSD-2-Clause NVIDIA guards. LICENSE contains the upstream GPL-2.0 text.

NOVA reference: drm/rust/kernel, drm-rust-next,
`3497a403264b094ebd61b8b755165d56e7d9dc37`.
Only checked format/hardware/protocol code is being adapted, not Linux
registration, DMA allocation/free, kernel threads, or driver Drop semantics.

| Local | Upstream drivers/gpu/nova-core/ | Adaptation |
| --- | --- | --- |
| src/tlv.rs | firmware/tlv.rs | no kernel dependencies; fixed 32-tag bound; duplicate rejection; exact version |
| src/falcon.rs | falcon.rs, falcon/{gsp,sec2}.rs, falcon/hal/ga102.rs, fb/{regs,hal/ga100}.rs, gpu/hal/tu102.rs, gsp/hal/tu102.rs, regs.rs | AD106-only reset/DMA/RSA3K/boot, explicit retained sysmem-flush grant, mailbox/FRTS checks; absolute clock deadlines, global software quarantine |
| src/vbios.rs | vbios.rs, firmware/fwsec.rs, firmware.rs | bounded snapshot, IFR/PCIR/NPDE/BIT, production V3 descriptor only, FRTS/SB staging; no ROM execution |
| src/falcon/tests.rs | original Horn checks | recorded register order/arithmetic/deadline guards only; no GPU/firmware/RPC simulation evidence |
| src/firmware.rs | firmware/{gsp,riscv,booter}.rs | borrowed inputs; checked AD106 extents; production signature staging; no DMA or implicit Drop |

NVIDIA copyright notices are preserved on adapted modules. New checks/platform
adapters also use GPL-2.0-only. No files in work/nova are build dependencies.

## Runtime integration still required

The library is not connected to the supervised native task. No register writes
or DMA in this module have run against the board yet. A successful build is not
authentication, reset, DMA-drain, interrupt-delivery or GSP-readiness evidence.

- The adapter must supply actual scoped UC BAR0 views, a named clock/alarm,
  committed per-SID DMA grants and barriers, with acknowledged isolation before
  reclaiming backing after uncertainty. Numeric IOVAs are not authority.
- A sysmembar page must be registered before either engine's first reset and
  retained through cleanup. Existing WPR2 is rejected, not automatically reset.
  The soft GA102 RESET_READY wait is not a general timeout/retry exception.
- PROM acquisition needs bounded, reusable MMIO leases (there are currently
  eight retained exports). No huge BAR mapping or PCI option-ROM execution.
- Explicit ELF/runtime/DMA budgets must cover the 60.6-MiB image, working copies,
  bootloader/signatures/radix/LibOS/WPR metadata, logs and queues.
- GIN source155 must use the assigned shared single-MSI route and correct
  device acknowledgment/rearm, not a fabricated MSI-X vector index.
- Source-ordered GSP ROM/SEC2 handoff, r570_144 wire queues/framing/status and
  continuations, real GspInitDone and validated GetGspStaticInfo remain pending.
  None of the Falcon status/mailbox helpers may substitute for that gate.

## Pinned firmware (not GPL)

Temporary upstream distribution:
https://github.com/ttabi/linux-firmware-nova/tree/b5758fe4d378b8ac2476f914ac8e9b8788c3d877
(branch v570.144). ABI epoch **r570_144**.

Upstream WHENCE explicitly aliases ad106 to ad102 and ad102/gsp/gsp.bin to
ga102/gsp/gsp.bin. Five binary inputs plus the original license/WHENCE are
individually hash-pinned in nix/npins/sources.json. Binary URLs resolve the
actual Git LFS objects, **not pointer text**. The package installs unchanged
bytes under nvidia/ad106/gsp and includes LICENCE.nvidia and WHENCE.

The license permits binary redistribution on OSI-licensed operating systems
with the license retained and binaries unchanged; firmware executes only on
NVIDIA hardware. Runtime insertion of the original selected production
signature into a CPU-exclusive booter working copy follows upstream; no
signature bypass, patched distribution, ROM execution or firmware conversion.

gsp.bin is a **63,541,248-byte raw image**, not an ELF container. Its metadata
declares 570.144, a fixed build ID and 4096 bytes of signatures. Bootloader,
booter-load and booter-unload are required together. Structure checking is
neither CPU signature verification nor proof that hardware accepted the image.

The host nvidia-fwcheck program validates the actual package during its Nix
build, using the same no_std parser intended for the task. No giant synthetic
GPU model is used. Firmware is not yet embedded in the probe ELF: that must
accompany the real boot adapter and its explicit ELF/staging/DMA RAM budgets.
