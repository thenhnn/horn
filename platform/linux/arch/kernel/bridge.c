// SPDX-License-Identifier: GPL-2.0-only
#include <linux/kernel.h>
#include <linux/sched.h>
#include <linux/ptrace.h>
#include <linux/signal.h>
#include <linux/mm.h>
#include <asm/mmu_context.h>
#include <asm/tlbflush.h>
#include <sysdep/ptrace.h>
#include <kern_util.h>
#include <skas.h>
#include <horn.h>

#define LOAD(name, host) r.name = regs->gp[HOST_##host]
#define SAVE(name, host) regs->gp[HOST_##host] = r.name

void horn_enter_user(void *ptr)
{
    struct uml_pt_regs *regs = ptr;
    interrupt_end();
    for (;;) {
        struct horn_registers r;
        unsigned long address, code;
        int reason;
        regs->is_user = 1;
        if (um_tlb_sync(current->mm)) {
            panic("Horn userspace page-table synchronization failed");
        }
        if (horn_thread_select(current->mm->context.id.pid, &current->thread.horn_thread)) {
            force_sig(SIGKILL);
            interrupt_end();
            continue;
        }
        LOAD(ip, IP);
        LOAD(sp, SP);
        LOAD(flags, EFLAGS);
        LOAD(ax, AX);
        LOAD(bx, BX);
        LOAD(cx, CX);
        LOAD(dx, DX);
        LOAD(si, SI);
        LOAD(di, DI);
        LOAD(bp, BP);
        LOAD(r8, R8);
        LOAD(r9, R9);
        LOAD(r10, R10);
        LOAD(r11, R11);
        LOAD(r12, R12);
        LOAD(r13, R13);
        LOAD(r14, R14);
        LOAD(r15, R15);
        LOAD(fs, FS_BASE);
        LOAD(gs, GS_BASE);
        if (horn_fp_load(regs->fp)) {
            horn_fp_init(regs->fp);
            force_sig(SIGSEGV);
            interrupt_end();
            continue;
        }
        reason = horn_run(current->mm->context.id.pid, &r, &address, &code);
        if (horn_fp_save(regs->fp)) {
            panic("Horn FP snapshot failed");
        }
        SAVE(ip, IP);
        SAVE(sp, SP);
        SAVE(flags, EFLAGS);
        SAVE(ax, AX);
        SAVE(bx, BX);
        SAVE(cx, CX);
        SAVE(dx, DX);
        SAVE(si, SI);
        SAVE(di, DI);
        SAVE(bp, BP);
        SAVE(r8, R8);
        SAVE(r9, R9);
        SAVE(r10, R10);
        SAVE(r11, R11);
        SAVE(r12, R12);
        SAVE(r13, R13);
        SAVE(r14, R14);
        SAVE(r15, R15);
        SAVE(fs, FS_BASE);
        SAVE(gs, GS_BASE);
        // Foreign SYSCALL saves the faulting IP and raw Linux RAX. RDX is merely
        // seL4's selector and is never a Linux syscall discriminator.
        switch (reason) {
        case 1:
            regs->gp[HOST_IP] += 2;
            regs->gp[HOST_ORIG_AX] = regs->gp[HOST_AX];
            handle_syscall(regs);
            break;
        case 2: {
            struct faultinfo fi = {.cr2 = address, .error_code = code, .trap_no = 14};
            regs->faultinfo = fi;
            segv(fi, regs->gp[HOST_IP], 1, regs, NULL);
            break;
        }
        case 3:
            if (address == 16 || address == 19) {
                force_sig_fault(SIGFPE, horn_fp_exception_code(regs->fp, address),
                                (void __user *)regs->gp[HOST_IP]);
            } else if (address == 0) {
                force_sig_fault(SIGFPE, FPE_INTDIV, (void __user *)regs->gp[HOST_IP]);
            } else {
                force_sig(SIGILL);
            }
            break;
        case 0:
        case 4:
            break;
        default:
            panic("Horn unknown execution exit %d", reason);
        }
        horn_timer_interrupt();
        interrupt_end();
    }
}
