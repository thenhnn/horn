# Linux on Horn: first milestone

The chosen direction is a clean `ARCH=horn` paravirtualized Linux port.
The previous `ARCH=sel4` port and l4h are references, not host implementations
to import wholesale. This directory records the direction; it does not yet
contain a Linux port or builder.

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
