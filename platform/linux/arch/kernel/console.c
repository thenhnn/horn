// SPDX-License-Identifier: GPL-2.0-only
#include <linux/init.h>
#include <linux/err.h>
#include <linux/workqueue.h>
#include "../../../drivers/tty/hvc/hvc_console.h"
#include <horn.h>

static struct hvc_struct *console;

static void resize_console(struct work_struct *work)
{
    struct winsize size = {0};
    if (horn_console_geometry(&size.ws_row, &size.ws_col)) {
        hvc_resize(console, size);
    }
}

static DECLARE_WORK(resize_work, resize_console);

static ssize_t get_chars(u32 vterm, u8 *buf, size_t count)
{
    // HVC calls get_chars with its lock held. Geometry must be applied
    // separately: hvc_resize takes that lock and schedules TTY/SIGWINCH work.
    if (horn_console_take_resize()) {
        schedule_work(&resize_work);
    }
    return horn_console_read((char *)buf, count);
}

static ssize_t put_chars(u32 vterm, const u8 *buf, size_t count)
{
    horn_console_write((const char *)buf, count);
    return count;
}

static int notifier_add(struct hvc_struct *hp, int irq)
{
    int error = notifier_add_irq(hp, irq);
    if (!error) {
        horn_console_enable(1);
        schedule_work(&resize_work);
    }
    return error;
}

static void notifier_del(struct hvc_struct *hp, int irq)
{
    horn_console_enable(0);
    notifier_del_irq(hp, irq);
}

static const struct hv_ops ops = {
    .get_chars = get_chars,
    .put_chars = put_chars,
    .notifier_add = notifier_add,
    .notifier_del = notifier_del,
    .notifier_hangup = notifier_del,
};

void __init horn_console_init(void) { hvc_instantiate(0, 0, &ops); }

static int __init horn_console_device(void)
{
    console = hvc_alloc(0, horn_console_irq(), &ops, 4096);
    if (IS_ERR(console)) {
        int error = PTR_ERR(console);
        console = NULL;
        return error;
    }
    struct winsize size = {.ws_row = 24, .ws_col = 80};
    hvc_resize(console, size);
    resize_console(NULL);
    horn_ready();
    return 0;
}

device_initcall(horn_console_device);
