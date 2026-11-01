# Display core: consumption evidence and notifier probe

## Reference

Linux drm/rust/kernel `3497a403264b094ebd61b8b755165d56e7d9dc37`, the
checkout used for the Nova adaptation. These are source observations, not a
trace of running nouveau. Paths relative to `drivers/gpu/`:

- `drm/nouveau/nvkm/subdev/gsp/rm/r570/{disp,client}.c`: PB preparation,
  channel allocation and 120-byte SDK root parameters.
- `drm/nouveau/nvkm/subdev/gsp/rm/r535/disp.c`: instance lifecycle,
  channel user registers and `r535_dmac_bind` (RAMHT context/client tag).
- `drm/nouveau/nvkm/core/ramht.c`: handle/channel hash and entry encoding.
- `drm/nouveau/nvkm/engine/dma/usergv100.c`: 24-byte DMA descriptor,
  coherent target, small pages, 256-byte address units.
- `drm/nouveau/nvkm/engine/disp/{ga102,gv100}.c`: 8-KiB RAMHT and core user ID 0.
- `drm/nouveau/dispnv50/{disp,corec37d,corec57d}.c`: coherent sysmem ring,
  UPDATE notifier and required subsequent window bounds/ownership setup.
- `drm/nouveau/dispnv50/{headc57d,headc37d,wndwc67e}.c`: pending head/window path.
- `drm/nova-core/mm/{pramin.rs,hal/tu102.rs,regs.rs}`: BAR0 PRAMIN window.
- `drm/nova/driver.rs`: no complete KMS path in this inspected revision.

No reference headers/generated declarations are vendored. Firmware and build-time
Cargo bindgen headers remain pinned at **570.144**.

## Operator evidence

[HARDWARE.md](HARDWARE.md) records allocation with 120-byte roots, a display
subdevice and a 4-KiB ring. These simultaneous changes fixed allocation but do
not isolate the old INVALID_OFFSET cause. The successful recipe is unconditional;
there is no experimental module/flag or 108-byte root variant.

A subsequent operator run confirmed both coherent-sysmem command prefixes:

- NOP: PUT=GET=4, raw byte pointers 0x10.
- Notifier-disabled UPDATE: PUT=GET=11, raw byte pointers 0x2c.

This establishes **consumption**, not UPDATE completion, modeset or scanout.
Internal-client controls and zero `hObjectBuffer` follow nouveau. The allocation
`offset` is initial GET/PUT, not a physical PB address.

## Notifier prefix: confirmed by operator

One accounted 4-KiB coherent sysmem PB is zeroed and preloaded once. A separate
accounted ReadWrite DMA page starts with a zero/NOT_BEGUN notifier at offset zero.
After core allocation, before any PUT publication, the driver initializes the
owned instance's RAMHT and binds the notifier descriptor through BAR0 PRAMIN.
Every PRAMIN write is read back; a mismatch fails closed. No BAR1 or speculative
context-DMA RPC is used. The aperture selector is restored only after success.

The three preloaded prefixes are published in order:

1. Four NOPs; require GET=4.
2. Notifier-disabled, non-interlocked UPDATE; require GET=11.
3. SET_CONTEXT_DMA_NOTIFIER, enable notifier WRITE, zero interlocks, UPDATE,
   disable notifier; require GET=22 **and** GPU-written FINISHED status.

All polling shares one absolute two-second deadline, bounded by the boot
deadline. Firmware sequencer/fatal events and saved IRQ badges are serviced;
GIN acknowledgment precedes kernel ACK. No ring/notifier reuse, wrap, reset,
retry, speculative DMA release or guessed resume position is attempted.
Errors propagate to driver quarantine; backing remains retained.

Method headers retain byte methods in bits 13:2. MMIO PUT/GET use the legacy
NV507C/NV917C byte-pointer layout (11:2), not the logical C77D class fields (9:0).
Bindgen evaluates method/status fields and published window register definitions.

## Preferred mode: preflight only

The EDID's actual preferred base DTD is selected, not merely the first sorted
mode. Exact vertical blanking comes from the validated DTD, never from rounded
refresh rate. Missing totals/polarity, interlace, borders, inconsistent geometry
or insufficient RGB8 sink bandwidth reject the target; no 720p fallback.

`NVIDIA_DISPLAY_PREFERRED ... state=preflight-only` reports exact totals,
clock, pitch and required buffer size. This does **not** program a mode or prove
source-side DP support. Typed RM connector/SOR/DP control mappings are prepared.

## Current next stage — ownership and surface assembly (hardware-untested)

The new change adds corec57d usage bounds and corec37d static ownership:
window pairs 0/1 → head 0, 2/3 → head 1, etc., restricted by RM's present
window/head topology. A fourth core prefix requires FINISHED in a fresh
notifier slot at byte 16. With eight windows it ends at PUT=89.

A bounded RM allocation holds the preferred-size ARGB8888 pattern (bars,
checkerboard, red border). All VRAM, including pitch/allocation padding, is
initialized and read back through banked BAR0 PRAMIN; firmware events are
serviced between rows. No CE, BAR1, fixed VRAM placement or GPU VA is used.
Expect SCANOUT_BEGIN/PROGRESS/READY; the CPU upload may take several seconds.

The first operator run of this stage allocated a 16-MiB surface, then the local
metadata guard rejected it before upload. Format/attr were not logged, so that
run cannot identify the rejected field. The corrected policy now explicitly
requests and verifies contiguous VIDMEM, PITCH layout and no compression.
Allocation `format` is a PTE kind, not ARGB or a pitch/block-linear selector:
TU102 RM can choose uncompressed GENERIC_MEMORY (6) even for pitch-layout
IMAGE memory. Only published PITCH/GENERIC_MEMORY kinds are accepted; layout
and physicality are checked independently. Detailed reply/rejection logs now
include kind, attr, attr2 and requested attributes. No unknown/tiled/compressed
kind is allowed through, and no failed allocation is retried.

Both channels are allocated before RAMHT publication. Window 0's bounded
VRAM descriptor is bound under channel user ID 1; core notifier uses ID 0.
After ownership completion the window publishes NOP then surface assembly.
The C67E recipe follows wndwc67e_image_set (no legacy MEMORY_LAYOUT bit),
identity CSC and opaque composition. Upstream image.handle is an array:
the trailing PUSH_MTHD argument 1 is the element count, not another handle.

New target markers:
- NVIDIA_DISPLAY_CORE_COMPLETE kind=ownership, notifier_offset=16.
- NVIDIA_DISPLAY_SCANOUT_READY: CPU upload/readback only, not active scanout.
- NVIDIA_DISPLAY_WINDOW_ALLOC_OK and WINDOW_FETCHED kind=assembly.
- NVIDIA_DISPLAY_PIPELINE_PREPARED ... dp=untrained modeset=not-submitted.

There is deliberately **no window UPDATE** yet: no interlock is left waiting
for an unimplemented DP/head commit. Fetching assembly is not validation of
an active image. Still pending: source caps/watermarks, SOR acquisition, DP
training, head timings/viewport/LUT policy and coordinated modeset UPDATE.
CE/GMMU experiments remain parked. **No visible image is expected yet.**

## Operator run

Only the operator releases/rebinds the host GPU. Once both functions are already
under vfio-pci:

```sh
hardware/gpu/nvidia/run-vfio.sh
```

Relevant markers:

- `NVIDIA_DISPLAY_PREFERRED`: metadata preflight only.
- `NVIDIA_DISPLAY_CORE_ALLOC_OK`: core object allocated.
- `NVIDIA_DISPLAY_RAMHT_OK`: PRAMIN upload/readback, not GPU lookup proof.
- `NVIDIA_DISPLAY_CORE_REGS`: require untouched PUT/GET, never reset them.
- `NVIDIA_DISPLAY_CORE_SUBMIT kind=nop|update|notifier`: publication.
- `NVIDIA_DISPLAY_CORE_FETCHED`: consumption only, even for the third prefix.
- `NVIDIA_DISPLAY_CORE_COMPLETE kind=update`: FINISHED notifier, the new target.
- `NVIDIA_DISPLAY_CORE_TIMEOUT`, `BAD_REGS`, `FATAL`: fail-stop diagnostics.

The operator has now observed the third prefix at PUT=GET=22 and notifier
FINISHED (0x80000000); see HARDWARE.md. Builds/unit tests are not hardware
evidence. STOP/unload, repeat boots, DP video and visible scanout remain unproven. Do not reset/retry
automatically after failure.
