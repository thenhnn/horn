/* SPDX-License-Identifier: BSD-2-Clause */
.section .bss
.balign 64
root_stack:
.skip 32768
root_stack_top:
.section .text
.global _start
_start:
    lea root_stack_top(%rip), %rsp
    xor %rbp, %rbp
    call probe_main
    ud2

/* Raw Linux-style SYSCALL: no libsel4 stack wrapper. R13 = selectors,
 * R14 = index, R12 = original RSP. Every return must preserve raw context. */
.global foreign_app
foreign_app:
    mov %rsp, %r12
    xor %r14, %r14
1:
    mov (%r13,%r14,8), %rdx
    mov $39, %rax
    movabs $0x123456789abcdef0, %rbx
    movabs $0x76543210fedcba98, %rdi
    mov $101, %rsi
    mov $102, %r10
    mov $103, %r8
    mov $104, %r9
    std
    syscall
    cld
    cmp %r12, %rsp
    jne 2f
    cmp $-38, %rax
    jne 2f
    inc %r14
    cmp %r15, %r14
    jb 1b
    int $0x80
2:
    ud2
.section .note.GNU-stack,"",@progbits
