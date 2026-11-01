# Display core: reference, allocation evidence and command probe

## Reference inspected

Linux drm/rust/kernel `3497a403264b094ebd61b8b755165d56e7d9dc37`, the same
checkout used for the original Nova adaptation. These are source observations,
not a trace captured from a running nouveau driver. Paths below are relative
to `drivers/gpu/` in that revision:

- `drm/nouveau/nvkm/subdev/gsp/rm/r570/disp.c`:
  `r570_disp_chan_set_pushbuf`, `r570_dmac_alloc`, `r570_disp` dispatch.
- `drm/nouveau/nvkm/subdev/gsp/rm/r535/disp.c`:
  surrounding lifecycle, `r535_dmac_init`, `r535_curs_init`, `r535_chan_user`.
- `drm/nouveau/nvkm/subdev/gsp/rm/r535/device.c`: device constructor also
  creates a subdevice, even though display controls use the internal client.
- `drm/nouveau/nvkm/subdev/gsp/rm/r570/client.c`: generic `GSP_RM_ALLOC`
  uses the 120-byte SDK root parameters, not the old `ALLOC_ROOT` RPC layout.
- `drm/nouveau/nvkm/subdev/gsp/rm/ad10x.c`: classes `0xc770`, `0xc77d`.
- `drm/nouveau/dispnv50/disp.c`: `nv50_dmac_create`, `nv50_dmac_kick`,
  `nv50_dmac_free`; 4-KiB ring, coherent sysmem by default on Ada. Pascal's
  automatic VRAM selection is a separate hardware workaround.
- `drm/nouveau/dispnv50/corec57d.c`: full initial bounds/notifier setup.
- `drm/nouveau/dispnv50/corec37d.c`: UPDATE sequence and notifier completion.
- `drm/nouveau/include/nvif/pushc37b.h`: method packet encoding.
- `drm/nova/driver.rs`: render/GEM registration, no KMS setup in this revision.
  This is not a statement about all newer Nova revisions.

No reference source or generated declarations are vendored here. The firmware
and build-time bindgen headers remain pinned at 570.144.

## Allocation experiment: accepted on hardware

The operator's trace in [HARDWARE.md](HARDWARE.md) confirms core allocation
with all three changes together: 120-byte roots, a display subdevice, and a
4-KiB ring. RM rounded the VRAM backing allocation to 64 KiB; the announced
ring size and class were still 4 KiB. This does not isolate which change
resolved INVALID_OFFSET. No commands were submitted in that run.

The successful recipe is now unconditional in `module.nix`. The experimental
flag, separate module, 108-byte root variant and 64-KiB baseline are removed.
Window/cursor and timeout-heavy compute bring-up remain parked.

Two earlier suspicions were not deviations from nouveau: the internal-client
routing of instance/PB controls, and zero `hObjectBuffer` in DMA allocation.
The allocation `offset` is the initial GET/PUT position, not the physical PB
base. Nouveau also sends a PB control with `valid = 0` before cursor PIO
allocation; the old cursor failure therefore did not disprove a PB issue.

## Next probe: command fetch, not modeset

The command probe changes the PB backing from VRAM to one accounted 4-KiB
coherent sysmem allocation, following nouveau's Ada default. It never uses
our untrusted BAR1 aperture. This new backing/submission path has **not** yet
been exercised on hardware; the allocation-only result above was with VRAM.

Before publication the complete ring is zeroed and preloaded once. Then:

1. Publish four NOP words and poll GET until it reaches that PUT.
2. Publish the remaining words: disable notifier, clear cursor/window
   interlocks, and UPDATE with normal handling / RELEASE_ELV, as in
   `corec37d_update(..., ntfy=false)`.
3. Poll GET again. Both stages share one absolute two-second deadline bounded
   by the existing boot deadline. No ring wrap, rewriting, resetting, retry or
   uncertain DMA release is attempted. Errors fail-stop the driver epoch.

Method headers retain byte methods in bits 13:2; they are not method offsets
shifted down to bit zero. Nouveau accesses BAR0 user PUT/GET with the legacy
NV507C byte-pointer layout (11:2). The pinned SDK's NV917C definitions have
that same register layout; bindgen computes their shifts/masks. Do not confuse
this MMIO encoding with logical C77D class PUT/GET fields (9:0). BAR0 core
base and the coherent PCI target selector come from the published display
header. Method numbers and values come from `clc77d.h` through bindgen.

Firmware queue events are serviced while polling, including fatal events and
CPU sequencer requests. IRQ badges saved by clock waits are serviced with
GIN acknowledgment before kernel ACK. The DMA mapping stays retained for the
GPU epoch even after GET advances; GET does not prove the engine is finished
with every side effect.

This is **not** the complete `corec57d_init`: it deliberately omits the
notifier/context-DMA/RAMHT binding, window usage bounds and head assignment.
No timing, surface, DP link, or scanout is programmed. GET advancement means
command consumption, not successful UPDATE completion. A real notifier and
subsequent modeset are separate milestones.

## Operator run

Only the operator releases/rebinds the host GPU. Once both functions are
available under vfio-pci, the ordinary command now runs this path:

```sh
hardware/gpu/nvidia/run-vfio.sh
```

Relevant serial markers:

- `NVIDIA_DISPLAY_CORE_INPUT ... target=coherent-sysmem`: new PB path selected.
- `NVIDIA_DISPLAY_CORE_ALLOC_OK`: allocation only.
- `NVIDIA_DISPLAY_CORE_REGS`: untouched initial raw PUT/GET; nonzero state
  fails instead of guessing a resume position.
- `NVIDIA_DISPLAY_CORE_SUBMIT kind=nop|update`: logical PUT and raw MMIO value.
- `NVIDIA_DISPLAY_CORE_FETCHED kind=nop|update`: GET reached that prefix,
  with notifier disabled. **Not** a modeset/completion success marker.
- `NVIDIA_DISPLAY_CORE_TIMEOUT`, `BAD_REGS`, `FATAL`: diagnostic before
  fail-stop; backing is retained, not reused or freed speculatively.

Builds/tests do not establish new hardware success. STOP/unload and repeated
boots remain unproven; do not automatically reset/retry after failure.
