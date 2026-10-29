/* SPDX-License-Identifier: GPL-2.0-only */
/* Integer-only PID 1 for the first architecture slice. No libc or host ABI. */
.global _start
.section .text
_start:
 mov $39,%eax
 syscall
 cmp $1,%rax
 jne failed
 mov $1,%eax
 mov $1,%edi
 lea greeting(%rip),%rsi
 mov $(greeting_end-greeting),%edx
 syscall
 cmp $(greeting_end-greeting),%rax
 jne failed
 mov $228,%eax
 mov $1,%edi
 lea before(%rip),%rsi
 syscall
 test %rax,%rax
 jne failed
 mov $35,%eax
 lea duration(%rip),%rdi
 xor %esi,%esi
 syscall
 test %rax,%rax
 jne failed
 mov $228,%eax
 mov $1,%edi
 lea after(%rip),%rsi
 syscall
 test %rax,%rax
 jne failed
 mov after(%rip),%rax
 sub before(%rip),%rax
 imul $1000000000,%rax,%rax
 add after+8(%rip),%rax
 sub before+8(%rip),%rax
 cmp $500000000,%rax
 jl failed
 mov $1,%eax
 mov $1,%edi
 lea ready(%rip),%rsi
 mov $(ready_end-ready),%edx
 syscall
 cmp $(ready_end-ready),%rax
 jne failed
idle:
 mov $35,%eax
 lea duration(%rip),%rdi
 xor %esi,%esi
 syscall
 test %rax,%rax
 je idle
failed:
 mov $1,%eax
 mov $2,%edi
 lea error(%rip),%rsi
 mov $(error_end-error),%edx
 syscall
 mov $60,%eax
 mov $1,%edi
 syscall
 ud2
.section .rodata
greeting: .ascii "Linux /init running as PID 1\n"
greeting_end:
ready: .ascii "/init: monotonic clock and sleep work\n"
ready_end:
error: .ascii "/init: startup failed\n"
error_end:
.balign 8
duration: .quad 1,0
.section .bss
.balign 8
before: .skip 16
after: .skip 16
.section .note.GNU-stack,"",@progbits
