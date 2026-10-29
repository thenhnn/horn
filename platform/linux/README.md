# Linux on Horn: first milestone

The chosen direction is a clean `ARCH=horn` paravirtualized Linux port.
The previous `ARCH=sel4` port and l4h are references, not host implementations
to import wholesale. The in-progress port builds Linux 6.18 reproducibly with
its own architecture directory and native Horn transport (not UML os-Linux).

**Current milestone: embedded /init runs as PID 1.** The static assembly init
exercises trapped getpid/write/clock_gettime/nanosleep, then a real signal and
sigreturn with deliberately clobbered FP/SIMD registers. It checks x87, MXCSR,
YMM and, when available, ZMM/opmask preservation. Linux uses one logical CPU and 32 MiB admitted RAM.
This is bring-up code, not a validated execution-resource or isolation
implementation. Resource rollback, stop/fault races and negative isolation tests
still need an independent audit.

Linux retains the standard 30-second READY deadline. Diagnostic QEMU runs
are limited separately to 15 seconds. The current console is output-only;
user clone remains gated pending MM/TLS/lifecycle validation; Linux SMP is not
implemented. FP state is saved into each task's dynamically sized register area,
including full standard XSAVE signal frames and legacy/null FP sigreturn.
Kernel FP sections use the separate native TCB with preemption exclusion. The ABI is x86-64 only, without IA32/x32.

Build the experimental image with:
```sh
nix-build nix -A sel4.out -A root-task.package --argstr profile pc-dev \
  --arg modules '[ ./platform/linux/module.nix ]' --no-out-link
```

Validation: `pytest -q tests/boot/test_linux.py` uses KVM (skips if unavailable)
and checks PID 1 plus clock/sleep, then observes two more seconds without faults.
TCG has not completed this milestone within the 15-second diagnostic bound.
AVX-512 preservation across syscalls, sleep and signal delivery was exercised
under KVM. This is not a BLAKE3 benchmark or a multi-process FPU validation.

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
resources need bounded ownership and explicit teardown. The current native
one-TCB process bootstrap is not already a Linux/vCPU API.

Use the existing HPET-backed Horn clock/deadline service; do not inherit either
reference port's unconditional TSC assumptions. Shared time pages and fast TSC
are optimizations, not prerequisites.

Implemented prerequisite: opt-in x86-64 syscall trapping, including raw stack
preservation with SKIM. See `vendor/sel4/LOCAL_CHANGES.md` and the dedicated
`tests/boot/test_syscall_trap.py` fixture. No other Linux kernel extensions have
been selected or implemented by this milestone.
