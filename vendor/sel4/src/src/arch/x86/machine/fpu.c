/*
 * Copyright 2014, General Dynamics C4 Systems
 *
 * SPDX-License-Identifier: GPL-2.0-only
 */

#include <model/statedata.h>
#include <arch/machine/fpu.h>
#include <arch/machine/cpu_registers.h>
#include <arch/object/structures.h>
#include <arch/machine/fpu.h>

/*
 * Setup the FPU register state for a new thread.
 */
void Arch_initFpuContext(user_context_t *context)
{
    context->fpuState = x86KSnullFpuState;
}

/*
 * Initialise the FPU for this machine.
 */
BOOT_CODE bool_t Arch_initFpu(void)
{
    /* Enable FPU / SSE / SSE2 / SSE3 / SSSE3 / SSE4 Extensions. */
    write_cr4(read_cr4() | CR4_OSFXSR);

    /* Enable the FPU in general. */
    write_cr0((read_cr0() & ~CR0_EMULATION) | CR0_MONITOR_COPROC | CR0_NUMERIC_ERROR);
    enableFpu();

    /* Initialize the fpu state */
    finit();

    /* All processors must agree on the MXCSR bits accepted by state import. */
    BOOT_BSS static uint8_t fx[512] ALIGN(16);
    uint32_t mxcsr_mask;
    asm volatile("fxsave %0" : "=m"(fx));
    memcpy(&mxcsr_mask, fx + 28, sizeof(mxcsr_mask));
    if (!mxcsr_mask) {
        mxcsr_mask = 0xffbf;
    }
    if (x86KSmxcsrMask && x86KSmxcsrMask != mxcsr_mask) {
        printf("Incompatible MXCSR masks across CPUs\n");
        return false;
    }
    x86KSmxcsrMask = mxcsr_mask;

    if (config_set(CONFIG_XSAVE)) {
        uint64_t xsave_features;
        uint32_t xsave_instruction;
        uint64_t desired_features = config_ternary(CONFIG_XSAVE, CONFIG_XSAVE_FEATURE_SET, 1);

        /* check for XSAVE support */
        if (!(x86_cpuid_ecx(1, 0) & BIT(26))) {
            printf("XSAVE not supported\n");
            return false;
        }
        /* enable XSAVE support */
        write_cr4(read_cr4() | CR4_OSXSAVE);
        /* First CPU chooses the optional group; later CPUs must match. */
        BOOT_BSS static uint32_t component_size[64];
        BOOT_BSS static uint32_t component_offset[64];
        bool_t first = x86KSxsaveFeatureSet == 0;
        /* check feature mask */
        xsave_features = ((uint64_t)x86_cpuid_edx(0x0d, 0x0) << 32) | x86_cpuid_eax(0x0d, 0x0);
        uint64_t optional = config_ternary(CONFIG_XSAVE, CONFIG_XSAVE_OPTIONAL_FEATURE_SET, 0);
        if (first) {
            if ((xsave_features & optional) == optional) {
                desired_features |= optional;
            }
        } else {
            desired_features = x86KSxsaveFeatureSet;
        }
        if (!(desired_features & 1) ||
            ((desired_features & 4) && !(desired_features & 2)) ||
            ((desired_features & 0xe0) && (desired_features & 0xe7) != 0xe7)) {
            printf("Invalid XCR0 feature dependencies\n");
            return false;
        }
        if ((xsave_features & desired_features) != desired_features) {
            printf("Requested feature mask is 0x%llx, but only 0x%llx supported\n", desired_features, (long long)xsave_features);
            return false;
        }
        /* enable feature mask */
        write_xcr0(desired_features);
        /* validate the xsave buffer size and instruction */
        if (x86_cpuid_ebx(0x0d, 0x0) > CONFIG_XSAVE_SIZE) {
            printf("XSAVE buffer set set to %d, but needs to be at least %d\n", CONFIG_XSAVE_SIZE, x86_cpuid_ebx(0x0d, 0x0));
            return false;
        }
        if (x86_cpuid_ebx(0x0d, 0x0) < CONFIG_XSAVE_SIZE) {
            printf("XSAVE buffer set set to %d, but only needs to be %d.\n"
                   "Warning: Memory may be wasted with larger than needed TCBs.\n",
                   CONFIG_XSAVE_SIZE, x86_cpuid_ebx(0x0d, 0x0));
        }
        for (unsigned int i = 2; i < 64; i++) {
            if (!(desired_features & (1ull << i))) {
                continue;
            }
            uint32_t size = x86_cpuid_eax(0x0d, i);
            uint32_t offset = x86_cpuid_ebx(0x0d, i);
            if (offset > CONFIG_XSAVE_SIZE || size > CONFIG_XSAVE_SIZE - offset) {
                printf("XSAVE component does not fit configured storage\n");
                return false;
            }
            if (!first && (component_size[i] != size || component_offset[i] != offset)) {
                printf("Incompatible XSAVE component layout across CPUs\n");
                return false;
            }
            component_size[i] = size;
            component_offset[i] = offset;
        }
        x86KSxsaveFeatureSet = desired_features;
        /* check if a specialized XSAVE instruction was requested */
        xsave_instruction = x86_cpuid_eax(0x0d, 0x1);
        if (config_set(CONFIG_XSAVE_XSAVEOPT)) {
            if (!(xsave_instruction & BIT(0))) {
                printf("XSAVEOPT requested, but not supported\n");
                return false;
            }
        } else if (config_set(CONFIG_XSAVE_XSAVEC)) {
            if (!(xsave_instruction & BIT(1))) {
                printf("XSAVEC requested, but not supported\n");
                return false;
            }
        } else if (config_set(CONFIG_XSAVE_XSAVES)) {
            if (!(xsave_instruction & BIT(3))) {
                printf("XSAVES requested, but not supported\n");
                return false;
            }
        }
        /* Init MXCSR */
        unsigned int mxcsr = MXCSR_INIT_VALUE;
        asm volatile("ldmxcsr %0" :: "m"(mxcsr));
    }
    return true;
}
