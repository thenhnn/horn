// SPDX-License-Identifier: GPL-2.0-only
#include <linux/sched.h>
#include <linux/sched/task_stack.h>
#include <linux/ptrace.h>
#include <linux/string.h>
#include <asm/processor.h>
#include <kern_util.h>
#include <os.h>
#include <horn.h>

void userspace(struct uml_pt_regs *regs) { horn_enter_user(regs); }

int start_idle_thread(void *stack, jmp_buf *buf)
{
    new_thread(stack, buf, new_thread_handler);
    longjmp(*buf, 1);
    return 0;
}

void new_thread(void *stack, jmp_buf *buf, void (*handler)(void))
{
    unsigned long sp = (unsigned long)stack + THREAD_SIZE - sizeof(void *);
    memset(buf, 0, sizeof(*buf));
    (*buf)[0].__rip = (unsigned long)handler;
    (*buf)[0].__rsp = sp;
}

void switch_threads(jmp_buf *from, jmp_buf *to)
{
    if (!setjmp(*from)) {
        longjmp(*to, 1);
    }
}

void get_safe_registers(unsigned long *gp, unsigned long *fp)
{
    memset(gp, 0, 27 * sizeof(long));
    if (fp) {
        horn_fp_init(fp);
    }
}

void initial_jmpbuf_lock(void) {}

void initial_jmpbuf_unlock(void) {}

void arch_flush_tls(struct task_struct *t) {}

void clear_flushed_tls(struct task_struct *t) {}

int arch_set_tls(struct task_struct *t, unsigned long tls)
{
    t->thread.regs.regs.gp[HOST_FS_BASE] = tls;
    return 0;
}
