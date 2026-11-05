// SPDX-License-Identifier: GPL-2.0-only
#include <linux/clockchips.h>
#include <linux/clocksource.h>
#include <linux/interrupt.h>
#include <linux/sched/clock.h>
#include <os.h>
#include <horn.h>
int signals_enabled;
static int clock_ready, delivering;

void block_signals(void) { signals_enabled = 0; }

void unblock_signals(void)
{
    signals_enabled = 1;
    horn_timer_interrupt();
}

int um_get_signals(void) { return signals_enabled; }

int um_set_signals(int v)
{
    int old = signals_enabled;
    signals_enabled = v;
    if (v) {
        horn_timer_interrupt();
    }
    return old;
}

int um_set_signals_trace(int v) { return um_set_signals(v); }

void block_signals_hard(void) { block_signals(); }

void unblock_signals_hard(void) { unblock_signals(); }

void init_IRQ(void) {}

void do_IRQ(int irq, struct uml_pt_regs *regs) { generic_handle_irq(irq); }

static u64 clock_read(struct clocksource *cs) { return horn_now_ns(); }
static struct clocksource source = {.name = "horn",
                                    .rating = 300,
                                    .read = clock_read,
                                    .mask = CLOCKSOURCE_MASK(64),
                                    .flags = CLOCK_SOURCE_IS_CONTINUOUS};

static int next_event(unsigned long delta, struct clock_event_device *dev)
{
    horn_deadline(horn_now_ns() + delta);
    return 0;
}

static int shutdown(struct clock_event_device *dev)
{
    horn_deadline(0);
    return 0;
}
static struct clock_event_device event = {.name = "horn",
                                          .features = CLOCK_EVT_FEAT_ONESHOT,
                                          .rating = 300,
                                          .set_next_event = next_event,
                                          .set_state_shutdown = shutdown};

void __init time_init(void)
{
    clocksource_register_hz(&source, 1000000000);
    event.cpumask = cpumask_of(0);
    clockevents_config_and_register(&event, 1000000000, 1000000, 0x7fffffff);
    clock_ready = 1;
}

void horn_timer_interrupt(void)
{
    if (!clock_ready || !signals_enabled || delivering) {
        return;
    }
    int clock = horn_clock_pending();
    int console = horn_console_pending();
    if (!clock && !console) {
        return;
    }
    delivering = 1;
    signals_enabled = 0;
    struct pt_regs *old = set_irq_regs(&current->thread.regs);
    irq_enter();
    if (clock) {
        event.event_handler(&event);
    }
    if (console) {
        horn_console_interrupt();
    }
    irq_exit();
    set_irq_regs(old);
    signals_enabled = 1;
    delivering = 0;
}

unsigned long long sched_clock(void) { return horn_now_ns(); }
