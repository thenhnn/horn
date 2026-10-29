/* SPDX-License-Identifier: GPL-2.0-only */
.section .text.entry,"ax"
.global _start
_start:
 lea horn_boot_stack_top(%rip), %rsp
 xor %rbp, %rbp
 call horn_start
 ud2
.section .bss
.balign 64
horn_boot_stack:
.skip 32768
horn_boot_stack_top:
.section .horn,"a"
.ascii "HORN"
.long 1
.section .note.GNU-stack,"",@progbits
