# Monotonic clock

The PC system starts **timerd** and publishes `/services/clock`, including builds
without networking or PCI drivers. This is not a network feature.

- **devmgr** owns ACPI HPET discovery, uncached/NX MMIO, kernel IRQ and comparator.
  Timer 0 uses 64-bit FSB/MSI delivery, never legacy/INTx routing.
- **timerd** alone receives `/devices/platform/clock` hardware authority. It owns
  monotonic microseconds, one shared calibration page and eight independently
  badged alarm objects. Clients cannot program hardware or choose IRQ routes.
- ClockInterface v2 provides `now()`, `shared_page()` and `alarm()`. There is no
  copied-calibration RPC compatibility API. `arm(deadline_us, notification)`
  replaces an alarm deadline; `cancel()` disarms it. Wakeups are one-shot hints.

Only the nearest deadline programs HPET. An empty queue disables the comparator;
there is no periodic tick or periodically updated clock-page counter.
Initialization calibrates against one 100-ms HPET wakeup, not a busy wait.

## Shared page and runtime API

Timerd allocates and initializes one resident page before READY and retains its
RW mapping and owner authority. It creates ONE READ-only VM handle without
CLOSE, DELEGATE, REVOKE, MAKE_IMMUTABLE or DMA rights. `shared_page()` returns
ordinary capability copies of that handle, not new server-side handles per client.
Clients can unmap/destroy their own regions and delete local caps, but cannot
close the shared handle for other clients.

The page is not globally immutable: timerd retains write authority. Its contents
are nevertheless static for the published lifetime. There is no sequence counter;
updates or on-the-fly mode switching would require a new synchronization contract.

The first 48 bytes use the IDL wire layout of `ClockCalibration`: ABI version,
`fast_tsc: BOOL`, base cycles/time, and conversion spans. Clients do not interpret
a native Rust record/bool layout. The rest of the 4096-byte backing is zeroed.

`horn-runtime` owns the common client mechanics:

```rust
let mut clock = ctx.clock(&mut io, "clock")?;
let us = clock.now(&mut io)?;
clock.unmap(&mut io)?; // Further now() calls use RPC.
```

It validates the shared handle's READ-only authority, backing size and wire ABI,
maps it read-only/NX in the client's region, and retains local owners. There is no
client-local backing allocation or calibration-copy path. Page acquisition or
mapping failure, unsupported fast time and invalid conversion all use RPC.
Cleanup failure is not treated as successful fallback: retain authority or fail-stop.
Drop only retires local caps; use explicit `unmap` to destroy the remote region
before process exit. No Context or transport is retained in the Clock object.
Netd uses this API rather than maintaining its own page implementation.

## TSC and SMP platform contract

CPUID invariant-TSC enables local time reads on both UP and SMP. The platform is
assumed to supply synchronized TSC frequency/epoch across CPUs. This is an
explicit assumption, NOT synchronization validation: there are no offset probes,
clocksource watchdog or live-migration guarantees. Without invariant-TSC the
page's boolean is false and readers use timerd RPC, backed by HPET.

KVM tests use `-cpu host,migratable=off` to expose host suitability rather than
forcing an unsupported invariant-TSC feature. TCG normally exercises RPC fallback.
This is monotonic elapsed time, not wall-clock time. Runtime fallback cannot catch
a fault caused by timerd death/revocation of a mapped page; fault recovery remains
separate lifecycle work.

## Initial context and platform boundary

Timerd uses `horn_runtime::task!(main)` and explicit named grants. Deployment
supplies `source`, `service`, `events`, `signal` and the standard runtime VM grants.
The inbox guard owns receive cleanup. No ambient RPC context is used.

The Q35 deployment explicitly selects `HORN_CLOCK_ROUTE=qemu-system-msi`.
QEMU HPET writes MSI through the system address space, outside PCI DMA translation;
Q35 provides no HPET DMAR scope. This is not a PCI compatibility-interrupt fallback.
The firmware-described HPET remapping path remains physically unvalidated.

Ordinary QEMU network tests cover shared mapping and HTTP/alarm behavior on UP/SMP,
with TCG fallback and KVM invariant-TSC fast time where available. No clock-probe
crate or physical-device/VFIO execution is required.
