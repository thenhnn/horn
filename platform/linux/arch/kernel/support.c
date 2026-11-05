// SPDX-License-Identifier: GPL-2.0-only
#include <linux/kernel.h>
#include <linux/sched.h>
#include <linux/console.h>
#include <linux/errno.h>
#include <linux/delay.h>
#include <os.h>
#include <horn.h>

void os_warn(const char *fmt, ...) {}

void os_info(const char *fmt, ...) {}

void os_dump_core(void) { panic("Horn fatal architecture fault"); }

void os_kill_ptraced_process(int pid, int reap) { panic("Horn process kill not wired"); }

int os_getpid(void) { return 1; }

void os_stop_process(int pid) { panic("Horn process stop not wired"); }

void os_alarm_process(int pid) { panic("Horn alarm transport not wired"); }

void os_kill_process(int pid, int reap) { panic("Horn process kill not wired"); }

void uml_exitcode(int status) { horn_shutdown(status); }

void machine_halt(void) { horn_shutdown(0); }

void machine_power_off(void) { horn_shutdown(0); }

void machine_restart(char *cmd) { horn_shutdown(127); }

void machine_shutdown(void) {}

void arch_suspend_disable_irqs(void) { block_signals(); }

void arch_suspend_enable_irqs(void) { unblock_signals(); }

void timer_set_signal_handler(void) {}

void os_idle_sleep(void) { horn_idle(); }

void __delay(unsigned long loops)
{
    while (loops--) {
        asm volatile("pause");
    }
}

void __const_udelay(unsigned long n) { __delay(n >> 8); }

void __udelay(unsigned long us)
{
    unsigned long long end = horn_now_ns() + us * 1000;
    while (horn_now_ns() < end) {
        asm volatile("pause");
    }
}

void __ndelay(unsigned long ns)
{
    unsigned long long end = horn_now_ns() + ns;
    while (horn_now_ns() < end) {
        asm volatile("pause");
    }
}
