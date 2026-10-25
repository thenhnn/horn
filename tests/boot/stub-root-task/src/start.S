.section .text.start,"ax",@progbits
.global _start
.type _start,@function
_start:
    /* seL4 passes BootInfo in rdi. Preserve it while establishing the stack. */
    lea stack_top(%rip), %rsp
    xor %ebp, %ebp
    cld
    call root_main
    ud2
.size _start, .-_start

.section .bss,"aw",@nobits
.balign 16
stack_bottom:
    .skip 65536
stack_top:

.section .note.GNU-stack,"",@progbits
