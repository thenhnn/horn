/*
 * Copyright 2014, General Dynamics C4 Systems
 *
 * SPDX-License-Identifier: GPL-2.0-only
 */

#pragma once

#include <config.h>
#include <types.h>
#include <arch/object/vcpu.h>
#include <object/structures.h>
#include <model/statedata.h>
#include <arch/machine/cpu_registers.h>

#define MXCSR_INIT_VALUE               0x1f80

/* REX.W is required to preserve full FIP/FDP in the legacy save area. */
#ifdef CONFIG_ARCH_X86_64
#define X86_FPU_IMAGE_WIDTH "64"
#else
#define X86_FPU_IMAGE_WIDTH ""
#endif

/* Initialise the FPU. */
bool_t Arch_initFpu(void);

/* Initialise the FPU state of the given user context. */
void Arch_initFpuContext(user_context_t *context);

static inline uint32_t xsave_features_high(void)
{
    return (uint32_t)(x86KSxsaveFeatureSet >> 32);
}

static inline uint32_t xsave_features_low(void)
{
    return (uint32_t)(x86KSxsaveFeatureSet & 0xffffffff);
}

/* Store state in the FPU registers into memory. */
static inline void saveFpuState(tcb_t *thread)
{
    user_fpu_state_t *dest = &thread->tcbArch.tcbContext.fpuState;
#ifdef CONFIG_VTX
    struct vcpu *vcpu = thread->tcbArch.tcbVCPU;

    if (vcpu && vcpu->fpu_active) {
        dest = &vcpu->fpuState;
    }
#endif
    if (config_set(CONFIG_FXSAVE)) {
        asm volatile("fxsave" X86_FPU_IMAGE_WIDTH " %[dest]" : [dest] "=m"(*dest));
    } else if (config_set(CONFIG_XSAVE_XSAVEOPT)) {
        asm volatile("xsaveopt" X86_FPU_IMAGE_WIDTH " %[dest]" : [dest] "=m"(*dest) : "d"(xsave_features_high()), "a"(xsave_features_low()));
    } else if (config_set(CONFIG_XSAVE_XSAVE)) {
        asm volatile("xsave" X86_FPU_IMAGE_WIDTH " %[dest]" : [dest] "=m"(*dest) : "d"(xsave_features_high()), "a"(xsave_features_low()));
    } else if (config_set(CONFIG_XSAVE_XSAVEC)) {
        asm volatile("xsavec" X86_FPU_IMAGE_WIDTH " %[dest]" : [dest] "=m"(*dest) : "d"(xsave_features_high()), "a"(xsave_features_low()));
    } else if (config_set(CONFIG_XSAVE_XSAVES)) {
        asm volatile("xsaves" X86_FPU_IMAGE_WIDTH " %[dest]" : [dest] "=m"(*dest) : "d"(xsave_features_high()), "a"(xsave_features_low()));
    }
}

/* Load FPU state from memory into the FPU registers. */
static inline void loadFpuState(const tcb_t *thread)
{
    const user_fpu_state_t *src = &thread->tcbArch.tcbContext.fpuState;
#ifdef CONFIG_VTX
    const struct vcpu *vcpu = thread->tcbArch.tcbVCPU;

    if (vcpu && vcpu->fpu_active) {
        src = &vcpu->fpuState;
    }
#endif
    if (config_set(CONFIG_FXSAVE)) {
        asm volatile("fxrstor" X86_FPU_IMAGE_WIDTH " %[src]" :: [src] "m"(*src));
    } else if (config_set(CONFIG_XSAVE)) {
        if (config_set(CONFIG_XSAVE_XSAVES)) {
            asm volatile("xrstors" X86_FPU_IMAGE_WIDTH " %[src]" :: [src] "m"(*src), "d"(xsave_features_high()), "a"(xsave_features_low()));
        } else {
            asm volatile("xrstor" X86_FPU_IMAGE_WIDTH " %[src]" :: [src] "m"(*src), "d"(xsave_features_high()), "a"(xsave_features_low()));
        }
    }
}

/* Reset the FPU registers into their initial blank state. */
static inline void finit(void)
{
    asm volatile("finit" :: "m"(control_reg_order));
}

/*
 * Enable the FPU to be used without faulting.
 * Required even if the kernel attempts to use the FPU.
 */
/** MODIFIES: phantom_machine_state */
/** DONT_TRANSLATE */
static inline void enableFpu(void)
{
    asm volatile("clts" :: "m"(control_reg_order));
}

/*
 * Disable the FPU so that usage of it causes a fault
 */
static inline void disableFpu(void)
{
    write_cr0(read_cr0() | CR0_TASK_SWITCH);
}

