// SPDX-License-Identifier: GPL-2.0-only
#include <linux/init.h>
#include <linux/err.h>
#include "../../../drivers/tty/hvc/hvc_console.h"
#include <horn.h>

static struct hvc_struct *console;

static ssize_t get_chars(u32 vterm, u8 *buf, size_t count)
{
    return horn_console_read((char *)buf, count);
}

static ssize_t put_chars(u32 vterm, const u8 *buf, size_t count)
{
    horn_console_write((const char *)buf, count);
    return count;
}

static int notifier_add(struct hvc_struct *hp, int unused)
{
    hp->irq_requested = 1;
    horn_console_enable(1);
    return 0;
}

static void notifier_del(struct hvc_struct *hp, int unused)
{
    horn_console_enable(0);
    hp->irq_requested = 0;
}

static const struct hv_ops ops = {
    .get_chars = get_chars,
    .put_chars = put_chars,
    .notifier_add = notifier_add,
    .notifier_del = notifier_del,
    .notifier_hangup = notifier_del,
};

void horn_console_interrupt(void)
{
    if (console) {
        hvc_kick();
    }
}

void __init horn_console_init(void) { hvc_instantiate(0, 0, &ops); }

static int __init horn_console_device(void)
{
    console = hvc_alloc(0, 0, &ops, 4096);
    if (IS_ERR(console)) {
        int error = PTR_ERR(console);
        console = NULL;
        return error;
    }
    struct winsize size = {.ws_row = 24, .ws_col = 80};
    hvc_resize(console, size);
    horn_ready();
    return 0;
}

device_initcall(horn_console_device);
