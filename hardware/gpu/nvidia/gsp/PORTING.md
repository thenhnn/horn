# Native GSP source/provenance

This crate is **GPL-2.0-only**, not covered by the surrounding original
BSD-2-Clause NVIDIA guards. LICENSE contains the upstream GPL-2.0 text.

NOVA reference: drm/rust/kernel, drm-rust-next,
`3497a403264b094ebd61b8b755165d56e7d9dc37`.
Only checked format/hardware/protocol code is being adapted, not Linux
registration, DMA allocation/free, kernel threads, or driver Drop semantics.

| Local | Upstream drivers/gpu/nova-core/ | Adaptation |
| --- | --- | --- |
| src/abi.rs | gsp/fw/r570_144/bindings.rs | checked-in layouts; core::ffi, remove Linux MaybeZeroable derives, explicit Rust 2024 unsafe blocks; no build-time transformation |
| src/boot.rs | fb.rs, gsp.rs, gsp/fw.rs, firmware/radix3.rs | checked AD106 framebuffer/WPR, radix3, LibOS/args/log/queue builders; numerical spans are not grants; zero PTEs beyond committed queue pages |
| src/gin.rs | irq/{gsp,interrupt_tree,regs,hal/tu102}.rs, regs.rs | AD106 GIN155/shared single-MSI acknowledgment; host-routed latch selection, retrigger/rearm, explicit masking; no Drop or INTx |
| src/queue.rs | gsp/cmdq.rs | bounded 63-page rings, volatile-word backend, private snapshots, publication barriers, peer-progress/index checks and fail-stop corruption; no automatic retry or deadline renewal |
| src/wire.rs | gsp/{fw,cmdq,commands}.rs, gsp/cmdq/continuation.rs | bounded 64-KiB frames/checksums, distinct transport/inner sequences, bounded outgoing continuations and static-info payload validation; not a live RPC transport |
| src/tlv.rs | firmware/tlv.rs | no kernel dependencies; fixed 32-tag bound; duplicate rejection; exact version |
| src/falcon.rs | falcon.rs, falcon/{gsp,sec2}.rs, falcon/hal/ga102.rs, fb/{regs,hal/ga100}.rs, gpu/hal/tu102.rs, gsp/hal/tu102.rs, regs.rs | AD106-only reset/DMA/RSA3K/boot, explicit retained sysmem-flush grant, mailbox/FRTS checks; absolute clock deadlines, global software quarantine |
| src/vbios.rs | vbios.rs, firmware/fwsec.rs, firmware.rs | bounded snapshot, IFR/PCIR/NPDE/BIT, production V3 descriptor only, FRTS/SB staging; no ROM execution |
| src/falcon/tests.rs | original Horn checks | recorded register order/arithmetic/deadline guards only; no GPU/firmware/RPC simulation evidence |
| src/firmware.rs | firmware/{gsp,riscv,booter}.rs | borrowed inputs; checked AD106 extents; production signature staging; no DMA or implicit Drop |

NVIDIA copyright notices are preserved on adapted modules. New checks/platform
adapters also use GPL-2.0-only. No files in work/nova are build dependencies.

## Native execution status

The opt-in task connects pinned firmware embedding, accounted staging, production
FRTS/SB/load/unload preparation, sysmembar, FRTS → ROM → SEC2 handoff, private
shared-queue snapshots, bounded cold-boot CPU sequencer, trusted PCI system-info
and registry, and GIN155 single MSI. Unknown sequencer writes fail closed; PCI
config, fuse and interrupt ownership are not delegated to firmware requests.
READY requires actual GspInitDone and a validated GetGspStaticInfo reply.
MMIO is mapped as one 4-KiB lease per register page, cached five at a time
plus two fixed GIN/EOI windows, and each lease is closed before reuse. Eager
64-KiB register blocks exhausted the device-page claim budget before firmware
execution and were replaced; window/bank exhaustion now reports its own
diagnostic instead of only a client panic.

The 64-MiB/64-object DMA ceiling is unchanged. Boot mode explicitly reserves
256-MiB driver RAM, 192-MiB devmgr RAM, larger image launch windows, a 64-MiB
root heap and a 20-bit root CSpace; read-only mode remains available with `nvidia.boot = false`.
Normal STOP requires shutdown RPC, suspended mailbox, SB/unloader and broker
isolation acknowledgment. Failures abort without retry or implicit DMA release.
Validated on the physical board (2026-10-30, GPU passed through to the guest):
FLR, PROM read, production FWSEC/FRTS/WPR2, SEC2 booter, GSP RISC-V start, the
firmware-driven CPU sequencer, GspInitDone and a validated GetGspStaticInfo
reply (`NVIDIA GeForce RTX 4060 Ti`, five FB regions). No GPU/firmware-peer
emulator was used.

Still unproven on hardware: MSI/GIN interrupt delivery, the STOP/unload path,
repeat-boot determinism, and everything past boot (RM client, GPU memory,
channels, faults). A successful boot is not a working GPU driver.

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

## Generated RM interface (build input)

NVIDIA `open-gpu-kernel-modules` tag **570.144**, commit
`8ec351aeb96a93a4bb69ccc12a542bf8a8df2b6f`, is a hash-pinned npins source
(`nvidia-open-gpu-r570144`), matching the firmware ABI epoch. `build.rs` uses the
Cargo `bindgen` build-dependency to read the unmodified upstream headers listed
in `wrapper.h`. Generated Rust lives only in OUT_DIR, not in the repository.
Neither generation nor compilation depends on a work/ checkout or a network
fetch from build.rs. Nix supplies the headers and build-host libclang via
`hardware/gpu/nvidia/bindings.nix`; `nix-shell` supplies the same environment to
Cargo. Run `python3 tools/rust-generate.py` inside nix-shell after changing build
dependencies, as for other workspace crates.

The allowlist covers RM allocation/control envelopes, their parameter types,
class/command constants, CE methods and NV status codes. Clang targets the
x86_64 C ABI used by this AD106 driver, not the build host or the seL4 target
triple. Bindgen emits compile-time C/Rust size, alignment and offset checks.
Small C enum expressions evaluate selected NVIDIA range/function-like macros;
no upstream structs or numeric bit positions are copied into the wrapper.

`rm::codec` serializes individual fields using generated offsets and sizes into
zero-filled buffers. It never copies a Rust struct's padding or casts an
unaligned firmware reply to a reference. Exhaustive field destructuring catches
header field additions. NvP64 fields are opaque address bits, never dereferenced.
The SDK root parameters are **120 bytes** (including pOsPidInfo); the separate
`ALLOC_ROOT` RPC's `rpc_alloc_root_v07_00` is **108 bytes**. Do not assume that
the latter is the parameter type for generic `GSP_RM_ALLOC`. Nouveau r570 uses
the SDK type there. Typed `Root` now unconditionally uses that SDK type;
the operator's [core allocation run](../HARDWARE.md) accepted it for both
clients. The old 108-byte recipe is removed.

`rm::Client` binds class/command IDs to generated parameter types using small
mapping macros. It owns no GPU backing and adds no capability authority. The
native adapter retains the single-outstanding-request/event/deadline policy and
checks the outer RPC result before interpreting the inner RM reply. The client
checks client/parent/object/command/class/size echoes, then returns typed output
parameters. Adding an operation means allowlisting its headers/types, providing
a field codec, and declaring the command/type association—not adding offsets.

The firmware-boot structs in src/abi.rs remain the checked-in NOVA port; this
migration covers the RM API, not every pre-boot ABI. Driver-owned page tables
and FERMI_VASPACE_A remain the GPU VA model. Earlier context-DMA experiments used
unverified RPC payloads; their failures do not prove that firmware lacks the
corresponding operations.

## Display notifier and preferred timing

`src/display/instance.rs` follows nouveau `r535_dmac_bind`,
`nvkm_ramht_hash/update`, `gv100_dmaobj_bind/new` and
`ga102_disp.ramht_size`: an 8-KiB RAMHT at instance offset zero and a
24-byte coherent/small-page DMA descriptor at offset 0x2000. The core channel
user ID is zero. Binding uses the descriptor offset shifted by 9 and the
display client's low 14 bits, not the internal client or a physical pointer.
PRAMIN aperture handling follows Nova `mm/pramin.rs` and the TU102 HAL.
Each write has a readback check; only the owned instance is accessed. The
notifier is a retained, accounted ReadWrite DMA page; BAR1 is not used.

The third preloaded core prefix follows `corec37d_update(ntfy=true)`.
GET=PUT alone cannot satisfy it: the GPU-written notifier at offset zero must
report FINISHED within the original bounded polling deadline. No notifier
slot or ring space is reused. An operator run now confirms FINISHED status
0x80000000 at GET=PUT=22 on the physical AD106; see ../HARDWARE.md.

`src/display/timing.rs` translates exact DTD geometry to nouveau head timing
coordinates and checks RGB8 sink bandwidth. `edid.rs` keeps piaf's source and
sync interpretation but reads exact base-DTD vertical blanking: piaf's
rounded refresh rate must not be used to reconstruct totals. Unknown totals,
sync polarity, borders, interlace or insufficient bandwidth reject the
preferred target instead of choosing a different mode. Source-cap/watermark
checks, window setup, scanout and DP training remain pending.
