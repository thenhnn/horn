# Linux on Horn: interactive shell and static musl userspace

The chosen direction is a clean `ARCH=horn` paravirtualized Linux port.
The previous `ARCH=sel4` port and l4h are references, not host implementations
to import wholesale. The in-progress port builds Linux 6.18 reproducibly with
its own architecture directory and native Horn transport (not UML os-Linux).

## Interactive shell

Run `just shell-linux` inside the development shell (or
`nix-shell --run 'just shell-linux'`). This boots the pinned static BusyBox
init/ash initramfs through `platform/linux/shell.nix`, with a root `linux# `
prompt on the virtio console. Ctrl-C reaches Linux's foreground process; Ctrl-A X
exits QEMU. Kernel/supervisor diagnostics go to `work/linux-debug.log`.
`just shell-linux tcg` explicitly selects software emulation instead of KVM.
The existing `just shell` still starts the native Horn shell.

BusyBox init mounts proc, sysfs and devtmpfs and respawns a login shell on
`/dev/hvc0`, with a real controlling TTY. Utilities, pipes and redirections run
ordinary Linux processes. Files are RAM-backed; there are no disks, Linux network
interfaces, authentication or persistence. This is a trusted local root console.
Terminal geometry, when reported by Horn, is applied through `hvc_resize()`
and Linux's normal TTY/SIGWINCH path. Unknown geometry keeps the 80x24 fallback;
`stty` still works. SIZE_CHANGED uses a generation-aware one-shot watch and
coalesced geometry work outside HVC's lock. End-to-end host-window resize through
the pinned QEMU stdio backend has not been validated. Linux still has one logical CPU.

Input is notification-driven: the architecture event dispatcher routes console
notifications through an IRQ domain and the standard HVC IRQ handler/notifiers,
without a synthetic polling timer. Clockevents retain a separate handler within
the common interrupt-context entry. A bounded RX buffer preserves input while
waiting for writable output; the same subscription covers both readiness
interests. The boot test covers commands, pipelines, TTY ioctls, Ctrl-C, shell
respawn, idle wakeup and a large output burst with concurrent input. Background
job-control testing is deferred.

## Musl regression initramfs

`platform/linux/module.nix` retains the regression entry point by default;
`linux.initMode = "shell"` selects BusyBox instead.
The static musl C `/init` runs as PID 1.
`init/demo.c` is an ordinary Linux C program, compiled with the pinned Nixpkgs
`pkgsStatic` musl toolchain. No Horn-specific userspace headers, custom startup,
libc patches or new kernel changes are required for this demo. It exercises:

- libc startup/arguments, getpid and printf/stdio;
- initialized C thread-local storage and libc errno;
- malloc/realloc/free, checking preserved allocation contents;
- fopen/fgets/fclose on an initramfs file;
- monotonic clock_gettime and nanosleep with an elapsed-time check;
- exec of `/task-test`, then `/fp-probe`, as the same PID 1.

The musl task test covers fork/exec/wait status, bidirectional private COW,
64 fork/reap cycles, and a contained child PROT_NONE fault. Four rounds of three
pthreads exercise distinct TLS/errno and TIDs, contended mutex/condition variables,
join return values, per-thread signals and FP rounding-mode isolation.

The follow-on assembly probe (`init/init.S`) tests signals/sigreturn with
intentionally clobbered FP/SIMD registers: x87, MXCSR, YMM and, when available,
ZMM/opmask preservation. It stays alive rather than exiting PID 1.
Linux uses one logical CPU and 32 MiB of resident `VmMemory` backing.
This is bring-up code, not a validated execution-resource or isolation
implementation. Resource rollback, stop/fault races and negative isolation tests
still need an independent audit.

Linux retains the standard 30-second READY deadline. Diagnostic QEMU runs
are bounded separately; the combined userspace regression allows 60 seconds.
Single-CPU user fork and pthread creation run the regression above.
This does not establish complete clone/ptrace/cancellation semantics or a security
audit. Linux SMP is not implemented. FP state is saved into each task's dynamically sized register area,
including full standard XSAVE signal frames and legacy/null FP sigreturn.
Kernel FP sections use the separate native TCB with preemption exclusion. The ABI is x86-64 only, without IA32/x32.

Build the experimental image with:
```sh
nix-build nix -A sel4.out -A root-task.package --argstr profile pc-dev \
  --arg modules '[ ./platform/linux/module.nix ]' --no-out-link
```

Validation: `pytest -q tests/boot/test_linux.py` uses KVM (skips if unavailable)
and checks every musl demo stage, successful exec into the FP/SIMD regression,
PID 1 and clock/sleep, then observes two more seconds without faults.
TCG has not been validated for this milestone.
AVX-512 preservation across syscalls, sleep and signal delivery was exercised
under KVM. The task regression also checks per-thread FP rounding state; it does
not constitute exhaustive XSAVE isolation testing or a BLAKE3 benchmark.

The PC kernel enables required x87/SSE/AVX state and an optional AVX-512 state
group. CPUID supplies the active XSAVE size; a 4096-byte capacity bounds TCB and
transfer storage. FP images cross the capability backend in bounded chunks;
partial writes cannot run, and only a complete validated image is committed.
A cache avoids rewriting unchanged state. Compiler targets have not globally
been changed to x86-64-v3 yet; the required AVX state is its OS prerequisite.

Linux applications use the Linux syscall ABI. Their seL4 TCBs opt into
`seL4_TCBFlag_trapSyscalls`: every SYSCALL goes to the Linux fault handler,
regardless of the value in RDX. Linux reads the syscall number from saved RAX.
The Linux kernel itself and native Horn services retain the native seL4 ABI.
Direct application seL4 IPC, mixed RAX dispatch and seL4/Linux scheduler
block/wake integration are deferred. Native debug syscalls are retained.

First executable target: one Linux instance, one logical vCPU, embedded
initramfs, execution of `/init`, and console output through Horn capabilities.
The one-vCPU stage does not imply permanent physical-core assignment. Use
automatic host placement; Linux SMP and host migration/balancing are later
milestones. No disk, network, GPU, ballooning or hardware passthrough initially.

Resource/bootstrap design must use Horn RAM accounts and scoped capability
authority, not synthetic root BootInfo or a copy of the old manager. Linux's
buddy allocator must operate on its admitted RAM locally; kmalloc/free must
not become individual Horn RPCs. Guest tasks, mappings, CSpace and scheduling
resources have bounded ownership and explicit teardown.

## Generic process/thread backend

Linux uses the ordinary process, thread and VM APIs; ProcessExecution and its
specialized RAM/PFN/VSpace manager have been deleted. There is no compatibility
wrapper. Deployment explicitly delegates `horn.process-factory` to Linux. Its
empty guest containers, translation objects and TCBs charge through Linux's RAM
account, with ancestor process/SC grant limits enforced.

Resident RAM is allocated through `VmFactory`, mapped RW/NX at `0x40000000`,
and managed locally by memblock/buddy/slab. The kernel ELF remains a separately
loaded image; it is not part of this RAM backing. Kernel vmalloc aliases occupy
`0x42000000..0x44000000`. The deployment reserves a 32 MiB root metadata heap for
allocator/mapping metadata, separate from Linux's 64 MiB resource-account ceiling.
Root metadata is still not fully prepaid per request.

Each Linux mm owns an empty process/VSpace. Fixed 2 MiB reservations cover
populated portions of the 256 MiB user window; mapping offsets select ordinary
RAM backing pages. Replacement is explicit Unmap/Prepare/Map, not implicit region
overwrite. Kernel RW and user RX aliases coexist under mapping-level W^X.

Each user task owns a separate trapped-syscall TCB, without an IPC buffer.
Fork/pthread creation provisions a TCB, and exit/exec/mm teardown retires it before
space reclamation. One explicitly retained execution context supplies the user
CPU budget across guest spaces; creating a thread does not grant another budget.
Linux kernel stacks still switch on one native kernel TCB. FP import/export uses
the generic stopped-thread interface for Linux's task/signal register images.
The current client bounds guest spaces and user TCBs at 8 and 64 respectively;
factory quotas and available accounted memory may impose tighter bounds.

`tests/boot/test_linux.py` now exercises musl fork/COW/exec/wait, pthread TLS,
synchronization/signals and FP preservation on this generic backend. This is
single-CPU validation, not Linux SMP or exhaustive cleanup-failure validation.

Use the existing HPET-backed Horn clock/deadline service; do not inherit either
reference port's unconditional TSC assumptions. Shared time pages and fast TSC
are optimizations, not prerequisites.

Implemented prerequisite: opt-in x86-64 syscall trapping, including raw stack
preservation with SKIM. See `vendor/sel4/LOCAL_CHANGES.md` and the dedicated
`tests/boot/test_syscall_trap.py` fixture. No other Linux kernel extensions have
been selected or implemented by this milestone.
