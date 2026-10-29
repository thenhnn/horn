/* SPDX-License-Identifier: GPL-2.0-only */
/* Static PID 1: syscalls, time and FP/SIMD across kernel entry and signals. */
.global _start
.section .text
_start:
 xor %ecx,%ecx
 xgetbv
 mov %eax,%r12d
 and $7,%eax
 cmp $7,%eax
 jne failed
 fninit
 fld1
 vpcmpeqd %ymm0,%ymm0,%ymm0
 mov %r12d,%eax
 and $0xe7,%eax
 cmp $0xe7,%eax
 jne 1f
 vpternlogd $255,%zmm0,%zmm0,%zmm0
 vpternlogd $255,%zmm31,%zmm31,%zmm31
 mov $255,%eax
 kmovw %eax,%k7
1:
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
 /* Self-deliver SIGUSR1. Handler clobbers FP state; sigreturn restores it. */
 mov $13,%eax
 mov $10,%edi
 lea action(%rip),%rsi
 xor %edx,%edx
 mov $8,%r10d
 syscall
 test %rax,%rax
 jne failed
 mov $234,%eax
 mov $1,%edi
 mov $1,%esi
 mov $10,%edx
 syscall
 test %rax,%rax
 jne failed
 cmpl $1,signal_seen(%rip)
 jne failed
 fstpl fp_check(%rip)
 movabs $0x3ff0000000000000,%rax
 cmp %rax,fp_check(%rip)
 jne failed
 stmxcsr fp_check(%rip)
 cmpl $0x1f80,fp_check(%rip)
 jne failed
 vmovdqu %ymm0,vector_check(%rip)
 lea vector_check(%rip),%rsi
 mov $4,%ecx
2:
 cmpq $-1,(%rsi)
 jne failed
 add $8,%rsi
 loop 2b
 mov %r12d,%eax
 and $0xe7,%eax
 cmp $0xe7,%eax
 jne 4f
 vmovdqu64 %zmm0,vector_check(%rip)
 lea vector_check(%rip),%rsi
 mov $8,%ecx
3:
 cmpq $-1,(%rsi)
 jne failed
 add $8,%rsi
 loop 3b
 vmovdqu64 %zmm31,vector_check(%rip)
 lea vector_check(%rip),%rsi
 mov $8,%ecx
5:
 cmpq $-1,(%rsi)
 jne failed
 add $8,%rsi
 loop 5b
 kmovw %k7,%eax
 cmp $255,%eax
 jne failed
 mov $1,%eax
 mov $1,%edi
 lea avx512_ready(%rip),%rsi
 mov $(avx512_ready_end-avx512_ready),%edx
 syscall
4:
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
handler:
 /* Linux handlers start with clean FP state, not the interrupted image. */
 vptest %ymm0,%ymm0
 jnz failed
 stmxcsr fp_check(%rip)
 cmpl $0x1f80,fp_check(%rip)
 jne failed
 mov %r12d,%eax
 and $0xe7,%eax
 cmp $0xe7,%eax
 jne 6f
 vmovdqu64 %zmm31,vector_check(%rip)
 cmpq $0,vector_check(%rip)
 jne failed
 cmpq $0,vector_check+56(%rip)
 jne failed
 kmovw %k7,%eax
 test %eax,%eax
 jne failed
 vpxord %zmm31,%zmm31,%zmm31
 kxorw %k7,%k7,%k7
6:
 vpxor %ymm0,%ymm0,%ymm0
 fninit
 fldz
 ldmxcsr changed_mxcsr(%rip)
 movl $1,signal_seen(%rip)
 ret
restorer:
 mov $15,%eax
 syscall
 ud2
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
ready: .ascii "/init: FP/SIMD state survives syscalls and signals\n"
 .ascii "/init: monotonic clock and sleep work\n"
ready_end:
error: .ascii "/init: startup failed\n"
error_end:
avx512_ready: .ascii "/init: AVX-512 state preserved\n"
avx512_ready_end:
.balign 8
action: .quad handler,0x04000000,restorer,0
changed_mxcsr: .long 0x3f80
.balign 8
duration: .quad 1,0
.section .bss
.balign 8
before: .skip 16
after: .skip 16
fp_check: .skip 8
signal_seen: .skip 4
.balign 64
vector_check: .skip 64
.section .note.GNU-stack,"",@progbits
