// SPDX-License-Identifier: GPL-2.0-only
#include <linux/kernel.h>
#include <linux/sched.h>
#include <linux/sched/task_stack.h>
#include <asm/ptrace.h>
#include <os.h>
#include <kern_util.h>
long elf_aux_hwcap;

ssize_t os_getrandom(void *buf, size_t len, unsigned int flags) { return -ENOSYS; }

void initial_thread_cb_skas(void (*proc)(void *), void *arg) { proc(arg); }

unsigned long get_thread_reg(int reg, jmp_buf *buf)
{
    switch (reg) {
    case HOST_IP:
        return (*buf)[0].__rip;
    case HOST_SP:
        return (*buf)[0].__rsp;
    case HOST_BP:
        return (*buf)[0].__rbp;
    default:
        return 0;
    }
}

void show_regs(struct pt_regs *regs)
{
    pr_emerg("Horn regs ip=%lx sp=%lx ax=%lx\n", PT_REGS_IP(regs), PT_REGS_SP(regs),
             PT_REGS_AX(regs));
}

void show_stack(struct task_struct *task, unsigned long *sp, const char *level)
{
    pr_emerg("Horn stack task=%s sp=%px\n", task ? task->comm : "current", sp);
}

void mc_set_rip(void *ctx, unsigned long ip) { panic("Horn has no host signal context"); }

int arch_fixup(unsigned long ip, struct uml_pt_regs *regs) { return 0; }

void arch_examine_signal(int sig, struct uml_pt_regs *regs) {}
