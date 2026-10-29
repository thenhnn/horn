/* SPDX-License-Identifier: GPL-2.0-or-later */
#ifndef _ASM_HORN_FPU_API_H
#define _ASM_HORN_FPU_API_H
#include <linux/types.h>
#define KFPU_387 1
#define KFPU_MXCSR 2
void kernel_fpu_begin_mask(unsigned int mask);
#define kernel_fpu_begin() kernel_fpu_begin_mask(KFPU_MXCSR)
void kernel_fpu_end(void);
bool irq_fpu_usable(void);
#endif
