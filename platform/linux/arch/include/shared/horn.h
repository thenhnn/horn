/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef HORN_LINUX_HOST_H
#define HORN_LINUX_HOST_H
/* One native TCB. No host-libc, ptrace, BootInfo or raw object capabilities. */
struct horn_registers {
 unsigned long ip,sp,flags,ax,bx,cx,dx,si,di,bp,r8,r9,r10,r11,r12,r13,r14,r15,fs,gs;
};
unsigned long horn_ram_bytes(void);
int horn_map(unsigned long space, unsigned long va, unsigned long pfn, unsigned int rights);
int horn_unmap(unsigned long space, unsigned long va, unsigned long bytes);
unsigned long horn_space_create(void);
void horn_space_destroy(unsigned long space);
void horn_console_write(const char *bytes, unsigned long size);
unsigned long long horn_now_ns(void);
void horn_deadline(unsigned long long deadline);
void horn_idle(void);
int horn_run(unsigned long space,struct horn_registers *registers,unsigned long *address,unsigned long *code);
int horn_clock_pending(void);
void horn_enter_user(void *regs);
void horn_host_init(void);
unsigned long horn_fp_bytes(void);
unsigned long long horn_fp_features(void);
int horn_fp_save(void *buffer);
int horn_fp_load(const void *buffer);
void horn_fp_init(void *buffer);
int horn_fp_valid(const void *buffer);
int horn_fp_exception_code(const void *buffer, unsigned vector);
void horn_shutdown(unsigned status) __attribute__((noreturn));
void horn_ready(void);
void horn_timer_interrupt(void);
#endif
