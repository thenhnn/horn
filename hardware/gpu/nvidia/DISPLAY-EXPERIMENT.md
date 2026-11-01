# Display core experiment — prepared, not hardware evidence

## Reference inspected

Linux drm/rust/kernel `3497a403264b094ebd61b8b755165d56e7d9dc37`
(the same checkout used for the original Nova boot adaptation). These are
source observations, not a trace captured from a running nouveau driver.

Paths relative to `drivers/gpu/` in that revision:

- `drm/nouveau/nvkm/subdev/gsp/rm/r570/disp.c`:
  `r570_disp_chan_set_pushbuf`, `r570_dmac_alloc`, `r570_disp` dispatch table.
- `drm/nouveau/nvkm/subdev/gsp/rm/r535/disp.c`:
  `r535_disp_oneinit`, `r535_disp_init`, `r535_dmac_init`, `r535_curs_init`.
  r570 reuses the surrounding lifecycle while overriding the channel wire API.
- `drm/nouveau/nvkm/subdev/gsp/rm/r535/device.c`:
  `r535_gsp_device_ctor` creates a subdevice after the device.
- `drm/nouveau/nvkm/subdev/gsp/rm/ad10x.c`: AD102 display/core class selection
  (`0xc770`, `0xc77d`), matching Horn.
- `drm/nouveau/dispnv50/disp.c`: `kmsChanPush` allocation is `0x1000` bytes.
- `drm/nova/driver.rs`: render registration and GEM, no KMS setup in this
  revision. No display/channel recipe was found in this revision's nova-core.
  This is not a statement about all newer Nova revisions.

No reference source or generated C/Rust declarations are vendored here.
Firmware and bindgen headers remain pinned at 570.144.

## What the comparison establishes

| Step | nouveau r570 | Horn baseline |
| --- | --- | --- |
| Instance announcement | internal client/subdevice | same internal control routing |
| Root parameters in generic alloc | 120-byte `NV0000_ALLOC_PARAMETERS` | 108-byte `rpc_alloc_root_v07_00` from the separate `ALLOC_ROOT` RPC |
| Display object tree | separate client, device **and subdevice** | client and device only |
| Pushbuffer announcement | internal client/subdevice | same internal control routing |
| DMA alloc parent | display root in display client | same |
| `hObjectBuffer` | zero | zero |
| Initial `offset` | saved GET/PUT position; zero on initial creation | zero |
| PB size / ring size class | 4 KiB / default zero | 64 KiB / explicit 64-KiB class |
| Cursor preparation | pushbuffer control with `valid = 0`, then PIO alloc | no preceding control |

Thus, using different clients for the controls and channel objects is **not**
by itself a deviation from nouveau. Nor does `hObjectBuffer = 0` establish a
missing context-DMA object in this path. `offset` is not the physical PB base.
The cursor failure cannot yet be used as evidence against a PB-related issue:
our cursor preparation was incomplete compared with nouveau.

Other differences remain outside the first experiment: nouveau announces
instance memory before constructing the display client; builds a RAMHT in it;
performs additional DP/head/output/notification setup; and has a different
memory manager. This experiment is not a byte-for-byte reproduction of the
complete nouveau lifecycle.

## Opt-in experiment

`display-core.nix` enables `nvidia.display-core-only`; the normal `module.nix`
keeps the previous recipe available as a baseline.

The experimental profile makes three functional changes before core allocation:

1. Use the 120-byte SDK root parameters for both internal and display clients,
   with `pOsPidInfo = NULL`, matching nouveau r570's generic alloc. Preserve the
   108-byte recipe only in the baseline. Successful earlier root allocation does
   not prove those parameter types are interchangeable.
2. Create the display client's `NV20_SUBDEVICE_0`, as the nouveau device
   constructor does. Controls still use the internal client's subdevice.
3. Use a 4-KiB VRAM pushbuffer, `limit = 4095`, and the default 4-KiB size
   class in both pushbuffer control and DMA allocation. Preserve zero initial
   GET/PUT, zero `hObjectBuffer`, and `subDeviceId = BIT(0)`.

It stops the display probe on the first error or on successful core allocation.
It does not attempt window/cursor allocation, GPU VA/GMMU or CE bring-up. The
normal service/IRQ/STOP loop remains in place. There are no retries or frees of
uncertain firmware-owned resources; existing shutdown/isolation policy applies.
No methods are submitted and no mode or link is programmed.

This deliberately tests a small reference-derived recipe, **not one variable
at a time**. Success would justify isolating which change matters; it would not
prove the root format, missing subdevice or 64-KiB ring was individually the cause.

## Operator procedure (only when the GPU is available)

The assistant has not detached, rebound, reset, or launched the host GPU.
The operator performs host release separately. After both GPU functions have
been assigned to vfio-pci by the operator:

```sh
MODULE="$PWD/hardware/gpu/nvidia/display-core.nix" \
  hardware/gpu/nvidia/run-vfio.sh
```

For the current typed-ABI baseline on a separate clean run, omit `MODULE`.
Do not compare only with old pre-bindgen logs: the new baseline has not yet
been exercised on hardware either. Do not automatically reboot/retry after a
failure; STOP/unload and repeat-boot determinism remain unproven.

Relevant serial output:

- `NVIDIA_RM_ROOT_INPUT` records the root parameter length, including for the
  initial internal client before any display request.
- `NVIDIA_DISPLAY_EXPERIMENT recipe=nouveau-r570-core` selects the display probe.
- `NVIDIA_DISPLAY_RM step=subdevice` confirms that allocation succeeded.
- `NVIDIA_DISPLAY_CORE_INPUT` records PB address, size class and selectors.
- `NVIDIA_RM_RPC_FAIL` distinguishes an outer RPC rejection (including private
  status) from an inner RM status reported at the display stage.
- `NVIDIA_DISPLAY_CORE_ALLOC_OK` means **allocation only**, not channel
  retirement, a modeset, scanout, or a visible image.
- `NVIDIA_DISPLAY_RM_FAILED` is failure; `NVIDIA_DISPLAY_EXPERIMENT_DONE
  compute=skipped` only means the probe has finished and compute was skipped.

A successful build is not new hardware evidence. Record the actual run and
exact failure stage in HARDWARE.md only after the operator's experiment.
