# Four-part display diagnostic

Run history is in HARDWARE.md:151518 has operator-confirmed pixels and CRC DONE;
152343 demonstrates HPD/link loss during the no-UPDATE training-only interval.
154234 exercised one bounded pre-publication recovery and the operator confirmed
an image, then manually sent SIGTERM. The pinned-Python observer captured all
three snapshots. Neither repeatability nor safe unload/restart is established.

This follows the operator's request to combine all four checks. **Do not launch
until the operator explicitly permits it after unloading the host driver.**
No host GPU access, reset, unbind/rebind or VFIO launch is part of preparation.
The preferred EDID mode, image, pitch, format and identity LUTs are unchanged.
Unit tests remain skipped at the operator's request.

## Sequence and source basis

Sources: nouveau `3497a403264b094ebd61b8b755165d56e7d9dc37`, notably
`dispnv50/disp.c::nv50_disp_atomic_commit_tail`,
`head.c::nv50_head_flush_set{,_wndw}`, `crcc57d.c`, `crcc37d.c/.h`, `crc.c`,
and pinned NVIDIA 570.144 SDK headers (`clc77d.h`, `clc37dcrcnotif.h`).
No copied C notifier structures or handwritten ABI offsets are introduced.

1. Allocate and initialize all DMA/VRAM objects; install the complete immutable
   RAMHT, including a dedicated CRC context. Capture the existing baseline.
2. Existing bounded pre-training D0/connected/quiet gate. D0 is written only
   when necessary. This gate did not fix the previous black screen.
3. Initial RM training and SST configuration, with existing strict replies
   and link-status validation. Relevant notification revision changes during
   training cancel activation.
4. Observe for at least **3 s after training/SST, before any core UPDATE**.
   Service events and take passive raster/formatter state. A pending link probe
   may extend this observation, bounded by another 2 s and the boot deadline.
   Any selected HPD/IRQ/state revision change invalidates the trained snapshot:
   no ownership/modeset may use it. Limited pre-publication recovery below
   can instead establish a new trained snapshot for one confirmed reconnect.
5. With a current validated snapshot, publish the immutable core prefixes. **Head timing/output and
   SF CRC state now precede the ownership UPDATE**, rather than following it.
   Training now also precedes ownership. Every core prefix drains events and
   rechecks the exact trained activity revision; neither a quiet replug nor a
   successfully observed lost link can rehabilitate the old snapshot. This
   corrects the audited initial ordering difference. Cursor/OLUT remain after ownership, matching nouveau's
   initial EFI GOP format-mismatch workaround. Window surface assembly follows
   ownership; final publication is still window/core interlocked.
6. Observe post-commit at 0/500/3000 ms, including live CRC samples and deferred
   connection/link probes. Then detach the CRC context using a separately
   prebuilt core-only UPDATE and a fresh notifier slot. No second modeset.
7. Build-only live-write animation: after successful CRC detach/DONE, schedule
   the first color change 2 s later in normal service. Repaint the same image's
   center through PRAMIN about once per second, with no extra UPDATE or count cap.
   No hardware result for this addition yet.
8. Normal service continues to decode events and perform bounded deferred
   probes. No automatic training, recovery, IRQ-vector acknowledgement or
   power write is introduced by those probes.

The ownership UPDATE can now activate head timing *before* final window
publication. Correlate HPD with both `CORE_SUBMIT kind=ownership clock_us=...`
and `COMMIT_DONE`, not only the latter. Training-only logs delimit the earlier
no-UPDATE interval. This diagnostic changes several things together; a visible
image alone would not identify which addition helped.

## Bounded pre-publication recovery

Implemented in `task/src/boot/modeset/recovery.rs`; run154234 exercised this path
with operator-confirmed visible output. See HARDWARE.md for timings and limits.
The entire prepare-link phase, including initial training/observation and any
recovery, is bounded by 30 s and the boot deadline. Only an explicit
`Observation::Invalidated` after successful training can enter recovery.
No catch-and-retry of Error::State, failed RPC, timeout or AUX error.

- Require exactly one selected unplug after the trained snapshot, then a later
  selected plug. DEFAULT connection samples or IRQ alone do not establish this
  pair. A second unplug is terminal, including during the reconnect wait.
- Wait up to 8 s for reconnect and 2 s without relevant activity. Require core
  and window PUT/GET zero and no prior publication. The first core PUT, even
  NOP, irreversibly closes recovery before attempting the doorbell.
- Freeze the activity revision, query fresh DEFAULT connection, read DPCD
  capabilities and power via RM. Require capabilities byte-for-byte unchanged,
  compatible with the prebuilt link, and already D0. No additional power write.
- Use SDK SPECIFIC_GET_EDID_V2 with RAW/no-cache flags. Validate echoed IDs and
  flags, returned bounds and complete extension count, then parse EDID. Require
  the original exact preferred timing; no fallback. This proves compatibility,
  not cryptographic or unique monitor identity.
- Read fresh config/status with event draining and revision checks around every
  RPC. Require actual lost configuration/EQ in valid replies. An already-good
  link after reconnect conservatively stops: no blind training or reinterpretation
  of the old token. Activity during validation also stops.
- Consume the per-session allowance and invalidate only the old training token
  and its sticky lost-link flag; issue one new training/SST for the new connection.
  Revisions, event counters and probe budget are not reset. The watch is initialized
  once per Native session.
- Observe another 3 s with existing bounded probes, re-read config/status, drain
  events and recheck the new trained revision before publication. Repeated activity
  or loss, or any failed RPC, is terminal; backing remains retained.

Source basis at pinned nouveau: nouveau_dp_link_check_locked / nouveau_dp_train_link
recheck EQ and request training when lost. The GSP backend r535_dp_train ignores
its retrain bool and calls the same r535_dp_train_target RM DP_CTRL path. Horn
keeps its strict direct-SST recipe, not Linux retries or active-head recovery.
The EDID command, flags, bounds, sizes and offsets come from pinned 570.144
ctrl0073specific.h, bindgen and field-wise codecs.

No UPDATE/ring/notifier reuse, allocation replacement, free/reset, IRQ-vector
acknowledgement or active-head recovery is added. The first publication uses
the original immutable program. Fresh AUX reads do not establish visible video.
Build: `work/nvidia-reconnect-build.log`; unit tests intentionally skipped.

## Deferred reconnect / IRQ checks

Notification callbacks only update a per-session watch; they never issue RPCs.
After queue draining in training-only observation, post-commit observation or
normal service, a connected pending notification is handled outside RPC waits.
Wait for 200 ms without relevant activity, within a 2 s probe deadline, then:

- Perform fresh DEFAULT GET_CONNECT_STATE with strict echo/status validation.
- Recheck connected state/activity revision after servicing queued events.
- Read DPCD link configuration at 0x100 and status at 0x202 through RM AUX.
- Drain/recheck between reads and after them; label stale results invalid.
- Report `configured`, `equalized`, and `retrain_candidate`; never retrain.

The watch revision is an observation-consistency token, **not a DMA epoch or a
claim that every IRQ changes physical connection**. At most four probe attempts
per session; bursts are coalesced. No AUX is deliberately issued while observed
unplugged. Unavoidable event/RPC races remain possible: **any RPC/AUX error is
still fail-stop, including RM GENERIC; it is not swallowed or retried**. A valid
reply describing lost EQ is a diagnostic observation, not malformed transport.

## CRC capture and lifetime

One fresh owned 64-KiB VRAM allocation, fully zeroed before exposure, with an
immutable RW context-DMA binding (core channel, handle 0x4000). Placement follows
nouveau `nv50_crc_ctx_init`; access uses the existing bounded BAR0 PRAMIN path,
now with a bounded read helper. No BAR1, register sweeps or arbitrary VRAM reads.

Select **SF only**, primary CRC, secondary NONE, controlling channel CORE,
no expected buffer collapse and no CRC-during-snooze, as `crcc57d_set_src` does.
The notifier also contains compositor/RG values. Word offsets/entry stride and
status fields come from the pinned SDK via bindgen; 2047 entries / 64 KiB follow
nouveau. One context, no flip, reset, reinitialization or reuse.

Live samples are provisional and may contain unwritten zeros or initial frames
before window publication. After observations, publish the prebuilt capture
source/context disable prefix with a **fresh notifier slot 96**, then require
core completion and CRC DONE within 200 ms. Read count/overflow, initial entries
and a small final SF tail. This is an additional planned capture-control UPDATE,
not a retry or repeated modeset. Neither timeout nor DONE authorizes allocation
free here: backing remains retained. A failure before detach also retains the
still-bound context; no speculative cleanup/reset is attempted.

A nonzero, stable CRC does not by itself establish the expected picture. The
expected hardware CRC of the pattern is not yet computed; values at different
taps need not match because their data representation may differ. SF capture
does not measure the electrical DP link or sink acceptance. No SOR capture or
source switching is mixed into this epoch.

## Periodic live framebuffer writes (build-only)

`task/src/boot/scanout/live.rs` is initialized only after successful CRC detach/DONE.
The first repaint is due 2 s later, then approximately every second: opaque
magenta→cyan→yellow→green, repeating without a frame-count or animation-duration
limit. Centered512×256 rectangle (x1024,y592 for2560×1440), white4-pixel border.
No flip, extra UPDATE or repeated CRC capture.

This runs as finite ticks in normal service, not an endless boot loop. The existing
accounted clock alarm wakes the bound notification/endpoint recv. Service re-arms
it after any shared-alarm RPC/probe sleeps. STOP/IRQ/client handling remains in
place; STOP cancels the animation alarm before existing shutdown handling. This
does not establish that GPU STOP/unload is safe. A per-session flag prevents
starting a second animation instance; any partial-write failure is terminal.

Only the retained owned linear image is written, row-by-row through PRAMIN, with
readback of every word. No CE/render writer, allocation free, ring/notifier reuse
or reset. Each individual repaint has a2 s failure deadline, independent of the
boot deadline; the animation itself has no fixed end. Each row drains events and
checks the exact trained revision. Relevant activity/loss or any error fails stop,
retaining backing. No vblank synchronization: tearing is allowed. Delayed ticks
do not replay missed frames in a catch-up burst. QEMU's60s/+3s limit is unchanged.

Readback proves stored bytes, not display-cache visibility or visible change.
The operator must confirm the rectangle's changing colors. The earlier CRC is
for the original image, not the animated framebuffer. Markers: LIVE_ANIMATION_READY
and LIVE_PATCH_BEGIN/DONE with change index/color, timing and `new_update=false`.
Native build passed (`work/nvidia-live-patch-build.log`); unit tests intentionally
skipped. The operator subsequently confirmed that the repeating live writes
work and are visibly slow. CPU PRAMIN's per-word write/readback path remains a
diagnostic backend, not a performant upload interface; no throughput figure
or exact run-log association was supplied with this confirmation.

## Reading the next log

- HPD in `TRAIN_ONLY_BEGIN`..`END`: training/PHY/sink activity precedes head
  activation; no ownership/modeset is attempted on the invalidated snapshot.
- No HPD there, but HPD after ownership/final UPDATE: inspect stream/head/sink
  acquisition and use deferred link status to distinguish lost EQ/configuration.
- `retrain_candidate=true` does not itself authorize display/DMA epoch reuse.
  Only the bounded pre-publication transition above may train once for a new
  connection; deferred probes and active service never do.
- CRC DONE/count and settled tail provide internal-path evidence, not a pixel
  correctness oracle. Zero count, overflow or timeout must remain explicit.
- Any earlier fail-stop means later checks may not be reached. We do not bypass
  safety/error checks merely to collect all four categories.

Build log: `work/nvidia-display-four-build.log`; selected formatting log:
`work/nvidia-display-four-format.log`. See HARDWARE.md for subsequent evidence.

## Read-only QEMU startup observer

`run-vfio.sh` now resolves Python from pinned Nix before launching QEMU, then
starts `diagnose-qemu.py` under the script's existing authorized UID. It gathers
/proc task state/wchan/syscall/kernel stacks and attempts QMP greeting, status,
vCPU thread information and `info registers` at3/15/35s. Results go to the run's
`qemu-diagnostic.log`. Its own50s budget does not change QEMU's60s/+3s limit.
It never attaches, stops/continues the VM, changes PCI state or resets anything.
Kernel stacks are not userspace backtraces. Missing interpreter fails pre-launch;
no more silent PATH-based skip. Run154234 validated the observer on a normal
boot. Independent shell run154540 captured QEMU stopped in do_signal_stop while
performing a terminal ioctl on stdin, not a proven VFIO wait. See HARDWARE.md.
The launcher now uses timeout --foreground to avoid the default separate process
group stopping -serial stdio during TTY configuration. The60s/+3s QEMU limit is
unchanged; foreground mode does not time out descendants. This launcher fix has
only syntax/diff validation so far, no subsequent hardware run.
