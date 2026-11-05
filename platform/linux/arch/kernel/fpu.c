// SPDX-License-Identifier: GPL-2.0-only
#include <linux/kernel.h>
#include <linux/preempt.h>
#include <linux/string.h>
#include <linux/signal.h>
#include <linux/irqflags.h>
#include <asm/fpu/api.h>
#include <horn.h>

void horn_fp_init(void *buffer)
{
    memset(buffer, 0, horn_fp_bytes());
    *(u16 *)buffer = 0x037f;
    *(u32 *)((u8 *)buffer + 24) = 0x1f80;
}

int horn_fp_valid(const void *buffer)
{
    const u64 *header = (const u64 *)((const u8 *)buffer + 512);
    if (header[0] & ~horn_fp_features()) {
        return 0;
    }
    for (unsigned i = 1; i < 8; i++) {
        if (header[i]) {
            return 0;
        }
    }
    return 1;
}

int horn_fp_exception_code(const void *buffer, unsigned vector)
{
    const u8 *raw = buffer;
    unsigned status;
    if (vector == 16) {
        status = *(const u16 *)(raw + 2) & ~*(const u16 *)raw;
    } else {
        unsigned mxcsr = *(const u32 *)(raw + 24);
        status = mxcsr & ~(mxcsr >> 7);
    }
    if (status & 1) {
        return FPE_FLTINV;
    }
    if (status & 4) {
        return FPE_FLTDIV;
    }
    if (status & 8) {
        return FPE_FLTOVF;
    }
    if (status & (2 | 16)) {
        return FPE_FLTUND;
    }
    if (status & 32) {
        return FPE_FLTRES;
    }
    return FPE_FLTINV;
}

/* Kernel and user code have separate seL4 TCBs. Kernel FP sections need
 * preemption exclusion, not a copy of the user's FP registers. */
static bool kernel_fp_active;

bool irq_fpu_usable(void) { return !in_interrupt() && !irqs_disabled() && !kernel_fp_active; }

void kernel_fpu_begin_mask(unsigned int mask)
{
    BUG_ON(!irq_fpu_usable());
    preempt_disable();
    kernel_fp_active = true;
    if (mask & KFPU_387) {
        asm volatile("fninit");
    }
    if (mask & KFPU_MXCSR) {
        unsigned int mxcsr = 0x1f80;
        asm volatile("ldmxcsr %0" ::"m"(mxcsr));
    }
}

void kernel_fpu_end(void)
{
    BUG_ON(!kernel_fp_active);
    kernel_fp_active = false;
    preempt_enable();
}
