# Initial modeset audit and combined diagnostic

Reference: nouveau at `3497a403264b094ebd61b8b755165d56e7d9dc37`.
Paths below are relative to `drivers/gpu/drm/nouveau/` in that tree.
RM ABI comes from the existing pinned open-gpu 570.144 headers via bindgen.
This is a source audit, not hardware evidence or a claim of stable scanout.

## Sequence comparison

| Stage | Nouveau | Horn before this diagnostic |
|---|---|---|
| Output acquisition | `nvkm/engine/disp/rm/r535/disp.c`: `r535_outp_acquire`, DFP_ASSIGN_SOR | Same control; one direct SST output, no audio |
| Training / SST | `nv50_sor_atomic_enable` in `dispnv50/disp.c`, then `r535_sor_dp_sst`: DP_CONFIG_STREAM, TU64, enhanced framing, watermark and blank symbols | Same RM controls and SST fields; no handwritten Mvid programming |
| Ownership | `nv50_disp_atomic_commit_tail`: output enable and head view/mode first, then window ownership in a core-only UPDATE | Ownership committed before DP training and before head mode methods; thus the full initial sequence is NOT identical |
| Window-dependent head state | `nv50_head_flush_set_wndw` in `dispnv50/head.c`: cursor and OLUT after ownership; comment explicitly handles inherited EFI GOP format mismatch | Cursor disable / OLUT after ownership as well; do not infer that this alone resolves all inherited-state issues |
| Final publication | `nv50_disp_atomic_commit_wndw` followed by core update with window interlock; fresh completion checked | Window then core publication with matching interlocks; FINISHED core and BEGUN window do not prove pixels |
| HPD / DP_IRQ | `r535_disp_oneinit` registers two RM event objects; `r535_gsp_device_event_ctor/get` in `nvkm/subdev/gsp/rm/r535/device.c` allocates callback-ex objects and enables repeated notifications | Missing subscriptions: absence of logged events did NOT establish absence of sink HPD/DP_IRQ |

Head mode/view/procamp/output, cursor disable, LUT setup and window surface
methods were compared with `headc37d.c`, `headc57d.c`, `wndwc57e.c` and
`wndwc67e.c`. No evidence yet justifies a different pitch, DMA-relative offset,
pixel format or timing. The reserved legacy 0x2074 method is deliberately not
copied into C77D. The initial sequencing difference remains a hypothesis,
not a proven failure or an excuse to reset/retry an uncertain epoch.

## One-run diagnostic coverage

- GET_ACTIVE for every supported head, both any-established and NV-client-
  established scopes, before routing. The latter is not necessarily limited
  to this particular client. Echo fields are checked. Zero is a valid inactive
  result, not an RM error.
- Head/window assembly and armed shadows plus passive raster/formatter state
  before the very first probe/ownership UPDATE. Allocation and SOR acquisition
  have already happened; this is NOT a pre-firmware or pre-RM baseline.
- Immediate post-commit method shadows, then passive raster/formatter snapshots,
  GET/PUT/notifiers and GET_ACTIVE for head 0 at approximately 0, 500 and
  3000 ms after the immediate shadow snapshot.
  Logical active-ID mismatches are reported, never used to reroute.
- HPD and DP_IRQ subscriptions using generated NV0005 allocation parameters
  and NV2080 notification controls. Decode POST_EVENT headers and notification
  data field-wise using generated offsets/types in RPC waits, display polling
  and the service loop. Report only; no link recovery/retraining/extra UPDATE.
- During observation, service GSP events instead of sleeping through the full
  interval. Keep the existing 2 s commit deadline; observation has a separate
  bounded deadline within the boot deadline.
- Remove the optional post-commit AUX probe that repeatedly returned inner RM
  GENERIC and stopped the driver. Do not swallow/retry any RM/transport error.
  Normal service retains backing after the observations.

The image, LUTs, preferred mode and UPDATE recipe are unchanged in this combined
experiment. New subscriptions/RPCs/logging alter timing; results cannot isolate
which addition matters if the image happens to appear.

## Deliberate limits

CRC is not included: `dispnv50/crcc57d.c::crcc57d_set_src` needs a context-DMA CRC
buffer and head CRC methods, not a passive read. It needs a separate accounted,
immutable-context implementation. SF_DP_MN is configuration, not measured video
Mvid/MSA; MSA_DEBUG_MAUD/NAUD are audio. No source-backed video-MSA readback was
found. No register sweep, error clearing, GPU reset, fallback mode or repeated
modeset is performed. A black screen despite active RM topology would still
leave fetch/color-path and sink-valid stream generation unresolved.

## Follow-up variant: settle before the first UPDATE

The combined run observed unsolicited unplug/replug, including the selected
sink, while the operator did not touch power/input/cable. The earlier successful
image may have happened while the monitor was already awake (tentative recall).
This motivates a pre-modeset wake experiment, not a claim of root cause.

`task/src/boot/modeset/sink.rs` now runs after allocation/baseline and before
any core probe/ownership UPDATE or training. It reads DPCD SET_POWER and only
writes D0 when needed, preserving other bits, as `nouveau_dp_train` does.
Per-session fixed-size state observes only the selected sink's HPD/DP_IRQ;
those events reset its quiet interval. No static/global watch or watch reset.

The explicit experimental gate requires at least 3 s after the power operation,
connected state and 2 s without relevant notification/state change, within an
8 s budget bounded by the boot deadline. An initial DEFAULT GET_CONNECT_STATE
seeds detection; successful CACHED samples every 200 ms observe it without
repeated full detection. Final DEFAULT detection and a D0 readback recheck the
candidate. Malformed replies, retry statuses, RPC/AUX errors or invalidation
at final verification stop the session; no failed operation is retried.
Pending events are drained with a deadline, including events received during
RPC waits. Training and commit recheck the gate; the old unconditional D0 write
in train is removed. No second wake write, training, UPDATE, ring or notifier
reuse occurs on failure.

These delays are experiment policy, not a DisplayPort readiness guarantee.
D0 plus connected/quiet HPD may still leave the panel asleep until video starts.
An unplug/replug after commit would show this gate was insufficient; that does
not authorize retraining the active uncertain epoch. Timing, image and LUT
recipe remain unchanged. Unit tests remain skipped; build/run evidence belongs
in HARDWARE.md.
