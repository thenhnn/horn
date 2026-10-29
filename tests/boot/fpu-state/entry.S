/* SPDX-License-Identifier: BSD-2-Clause */
.section .bss
.balign 64
stack: .skip 32768
stack_top:
.section .text
.global _start
_start:
 lea stack_top(%rip),%rsp
 xor %rbp,%rbp
 call probe_main
 ud2
.section .note.GNU-stack,"",@progbits
