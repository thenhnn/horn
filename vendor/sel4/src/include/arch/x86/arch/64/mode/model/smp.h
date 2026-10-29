/*
 * Copyright 2020, Data61, CSIRO (ABN 41 687 119 230)
 *
 * SPDX-License-Identifier: GPL-2.0-only
 */

#pragma once

#include <config.h>

#ifdef ENABLE_SMP_SUPPORT

typedef struct nodeInfo {
    void *stackTop;
    void *irqStack;
    /* This is the address (+8 bytes) of the 'Error' register of the user
     * context of the current thread. This address is designed so that the
     * 'syscall' trap code can do
     * movq %gs:16, %rsp
     * pushq $-1
     * pushq %rcx (rcx holds NextIP)
     * pushq %r11 (r11 holds RFLAGS)
     * etc etc
     * This value needs to be updated every time we switch to the user
     */
    word_t currentThreadUserContext;
    cpu_id_t index;
    /* Raw SYSCALL entry scratch; SKIM uses the same offset in nodeSkimScratch. */
    word_t syscallUserRSP;
    PAD_TO_NEXT_CACHE_LN(sizeof(void *) + sizeof(void *) + 2 * sizeof(word_t) + sizeof(cpu_id_t));
} nodeInfo_t;
compile_assert(nodeInfoSyscallRSPOffset, OFFSETOF(nodeInfo_t, syscallUserRSP) == 32)
compile_assert(nodeInfoIsCacheSized, (sizeof(nodeInfo_t) % L1_CACHE_LINE_SIZE) == 0)

extern nodeInfo_t node_info[CONFIG_MAX_NUM_NODES] ALIGN(L1_CACHE_LINE_SIZE);

#ifdef CONFIG_KERNEL_SKIM_WINDOW
/* IRET uses word 0; SYSCALL entry uses syscallUserRSP's offset. Match
 * node_info's stride to select the same CPU before and after switching CR3. */
extern char nodeSkimScratch[CONFIG_MAX_NUM_NODES][sizeof(nodeInfo_t)] ALIGN(L1_CACHE_LINE_SIZE) VISIBLE SKIM_BSS;
compile_assert(nodeInfoAndScratchSameSize, sizeof(node_info) == sizeof(nodeSkimScratch))
/* this will be declared in the linker script as the offset between node_info and
 * nodeSkimScratch */
extern char nodeSkimScratchOffset[];
#endif

static inline CONST cpu_id_t getCurrentCPUIndex(void)
{
    cpu_id_t index;
    asm("movq %%gs:%c[offset], %[result]"
        : [result] "=r"(index)
        : [offset] "i"(OFFSETOF(nodeInfo_t, index)));
    return index;
}

BOOT_CODE void
mode_init_tls(cpu_id_t cpu_index);

#endif /* ENABLE_SMP_SUPPORT */
