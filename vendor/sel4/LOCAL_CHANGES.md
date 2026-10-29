# Local changes

The upstream revision/tree in `upstream.json` identify the pristine import,
not this locally modified source tree.

The x86-64 ExecuteNever extension derives from l4h patch
`kernel/patches/0009-x86_64-add-seL4_X86_ExecuteNever-for-user-mappings.patch`,
https://github.com/losfair/l4h at
`9e50b64755ec2bed0d897b4d82ccdf7a7db02f09`.

It adds the public VM attribute, PTE execute-disable plumbing and NXE setup.
Horn reads EFER directly instead of retaining the patch's static NXE cache.
Unsupported NX requests return InvalidArgument (argument 2), rather than
silently granting executable access. No snapshot-fix patches are imported here.

The debug/release process boot test attempts instruction fetch from a present,
RW+NX IPC page and checks the resulting instruction-fetch protection fault.
This covers 4 KiB mappings in QEMU, not huge pages, SMP, or formal verification
of the modified kernel.

## Foreign syscall trapping (x86-64)

`seL4_TCBFlag_trapSyscalls` is opt-in through the existing TCB SetFlags API.
It is supported only on x86-64 with SYSCALL entry; other configurations ignore
the bit and do not report it in the resulting flags. Newly created TCBs remain
native. Every SYSCALL of a marked thread becomes an UnknownSyscall fault before
native fastpaths, VM-entry or debug dispatch. The fault's syscall field retains
the native RDX selector; a Linux dispatcher obtains the Linux number from saved
RAX. User INT traps are reported as general-protection exceptions, not SYSCALLs.
There is no mixed ABI and no change to debug syscall access for native threads.

The flag/dispatch concept and INT handling derive from l4h patch `0001` at the
revision cited above. Unlike l4h patch `0004`, Horn retains SKIM: raw RSP uses
supervisor-only per-core scratch, consumed and cleared before C/locking, and
foreign contexts return through IRET. The scratch layout and IRET approach
derive from the earlier arrfindustries seL4 port's
`src/arch/x86/64/traps.S`, `src/arch/x86/c_traps.c`, and
`include/arch/x86/arch/64/mode/model/smp.h` (local checkout inspected at Git HEAD
`32df7c8f082d322fb0c1f9e2848455c16d3c07a0`; this identifies the checkout, not a
claim of a pristine tree). No manager, Linux-mode invocation, FPU transport,
deadline-timer or user-event extension is imported.

SYSCALL entry also clears DF before C, while retaining the user's flags in the
saved context. RCX/R11 reflect SYSCALL's architectural clobbers. The foreign
context is marked for IRET before taking the kernel lock, including a possible
SMP IPI stall. UnknownSyscall replies already bypass the native reply fastpath;
VM-fault replies also use the existing full-context restore path.

`tests/boot/test_syscall_trap.py` is a dedicated trusted root fixture with shared
VSpace/CSpace, not an isolation or resource-accounting test. It checks raw
RSP/arguments/DF and RCX/R11, replies with a changed RAX and resumes repeatedly,
collisions with every ordinary MCS selector plus debug/unknown selectors, INT
#GP, and native IPC after clearing the flag. Two application TCBs exercise
per-CPU scratch. Native debug output and the existing IDL boot fixture remain
regression coverage. No formal verification, migration, Linux boot or
same-process Horn runtime threading is claimed.

## Extended FP/vector state (x86-64)

The frame-capability ReadFPU/WriteFPU mechanism derives from l4h patch
`0002-tcb-add-seL4_TCB_ReadFPU-and-seL4_TCB_WriteFPU.patch` at the revision
above (patch commit `54d19b3358a25cc4b8e5c14da285c9bcc3f7c934`). This is
a separate extension; it does not import l4h's Linux/manager ABI.

Transfers require the target TCB cap and a readable (import) or writable
(export), non-device RAM frame. A 64-byte-aligned image must fit completely.
The existing remote-TCB stall and local/remote FPU release are used; self-target
calls restore a previously live owner. VCPU-bound targets are rejected.

Unlike the reference patch, the initial interface accepts only FXSAVE or
standard XSAVE/XSAVEOPT and the x87/SSE/AVX/AVX-512 component set. Compacted
formats and opaque future components return IllegalOperation. Import is staged
under the kernel lock, never validated directly from shared user memory:
unsupported XSTATE_BV bits, nonzero XCOMP_BV or reserved header bytes reject the
request without changing the target. MXCSR is masked with the actual CPU mask
(including supported AMD extensions), not a hardcoded 16-bit mask.
The transfer capacity is bounded to 4096 bytes; future larger/component formats
require explicit implementation rather than silent truncation.

`KernelXSaveFeatureSet` remains required. The new
`KernelXSaveOptionalFeatureSet` is one all-or-nothing optional group chosen by
the first CPU. Other CPUs must support that selection, the same MXCSR mask and
identical enabled-component offsets/sizes. Capacity is checked against CPUID;
save/restore use the selected runtime mask. Defaults remain unchanged.
This enables a required mask 7 and optional mask 224 with bounded 4096-byte
storage, without requiring AVX-512 on every machine.

New threads receive a constructed architectural init image, not firmware vector
contents. Larger TCB objects use SDK-visible sizes. Rootserver objects are
allocated in descending alignment order, preserving exact size accounting
without assuming a TCB fits in a page.

On x86-64, save/restore use the REX.W (64-suffixed) instructions so the legacy
area contains full-width x87 FIP/FDP, matching the 64-bit signal/ptrace ABI.
The native fixture restores nonzero high bits in x87 FIP/FDP through hardware
and checks their full-width round trip (plain FLD alone is not portable to AMD
CPUs which update those pointers only on exceptions).

The small KVM `tests/boot/test_fpu_state.py` fixture checks fresh state,
self and live remote read/write, AVX and available AVX-512 (ZMM/opmask),
offset/capability bounds, read-only source/destination rights, invalid-header
rejection and MXCSR sanitization. AVX-512 was exercised, not only configured.
It is a trusted native fixture, not Linux signal/ptrace or resource isolation
coverage. No broad profile matrix or formal verification is claimed.

