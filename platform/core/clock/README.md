# Monotonic clock

The PC devmgr demo always starts **timerd** and publishes `/services/clock`,
including builds without networking or PCI drivers. It is not a network feature.

- **devmgr** owns ACPI HPET discovery, uncached/NX MMIO, the kernel IRQ and timer
  comparator. Timer 0 uses 64-bit FSB/MSI delivery, never legacy/INTx routing.
- **timerd** alone receives `/devices/platform/clock` backend authority. It owns
  monotonic microseconds, immutable calibration and eight independently badged
  alarm objects. Ordinary clients cannot program hardware or choose IRQ routes.
- Public `now()` has an RPC fallback. `calibration()` returns the immutable
  versioned TSC/base-time conversion; `alarm()` grants one alarm capability.
  `arm(deadline_us, notification)` replaces that object's deadline; `cancel()`
  disarms it. Wakeups are one-shot hints: clients must recheck their condition.

Only the nearest deadline programs HPET. An empty queue disables the comparator;
there is no periodic tick, polled deadline loop or periodically updated counter.
Initialization calibrates against one 100-ms HPET wakeup, not a busy wait.

## Initial context

Timer is the first ABI2 service: `horn_runtime::task!(main)` enters
`main(ctx: horn_runtime::Context)`. Deployment assigns private slots and grants
`source`, `service`, `events` and `signal`; runtime-owned names use `horn.*`.
There is no TimerCaps declaration in the clock RPC IDL, handwritten VmBootstrap,
root selector or slot bookkeeping in timer. The inbox receive guard owns cleanup.
The complete mixed-ABI pc-dev deployment builds; this migration is not yet
boot-validated. The clock/calibration/alarm protocols and limits are unchanged.

## Read-only time page

On single-CPU boots, when CPUID advertises invariant TSC, tasks can read time
locally using the base/ratio and a serialized TSC read. timerd obtains the actual
boot CPU count through its scoped lifecycle capability. On SMP it clears the
fast-path flag for itself and every client: invariant TSC is not a proof of
cross-CPU synchronization. All public time/deadline values then use HPET/RPC. KVM uses
`-cpu host,migratable=off`: it exposes host suitability rather than forcing an
unsupported invariant-TSC flag. TCG normally uses the RPC fallback.

`netd` materializes the calibration once in its own resident page, removes the
writable mapping, installs WRITE/GROW/SHRINK/SEAL seals, then maps it READ-only.
This is an immutable **local copy**, not shared physical backing: the current
native VM backend does not implement memory duplication/cross-process sharing.
It costs one page in the client's RAM account and needs no subsequent updates.
A cross-CPU fast path needs a separate validated synchronization/offset policy.
CPU migration, live migration or mutable calibration also require revisiting it. This is not wall-clock time.

## TODO: measured SMP fast-time optimization

- [ ] Measure clock RPC cost under realistic load before adding a new fast path.
  If justified, validate cross-CPU TSC suitability and implement genuinely shared
  backing: provider-writable, client-read-only time data with base/coefficients,
  sequence counter and a centrally changeable clock mode. Readers must retry
  inconsistent snapshots and fall back to RPC when the fast path is disabled.
  Immutable client-local copies cannot propagate that disable operation. Keep
  HPET/RPC on SMP meanwhile; neither shared VM backing nor this optimization is
  a prerequisite for further SMP or Linux work.

## Platform boundary

The Q35 example explicitly selects `HORN_CLOCK_ROUTE=qemu-system-msi`.
QEMU's HPET writes MSI through the system address space, outside PCI DMA
translation, and Q35 does not provide an HPET DMAR scope. This is not a PCI
compatibility-interrupt fallback: PCI still requires full-SID remapped MSI-X,
and VT-d compatibility interrupts remain disabled.

The backend also recognizes a firmware-described HPET remapping source without
granting it a DMA domain. That physical-hardware path is unvalidated; it must
not be substituted by a guessed requester ID. The PC launcher/test harness
enables `hpet.msi=on`. Preowned/unsupported HPET configurations fail closed.
Readonly-PCI builds grant only the fixed Q35 HPET MMIO page and system vector;
they still cannot pin DMA or assign a PCI device.
