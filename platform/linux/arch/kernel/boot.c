// SPDX-License-Identifier: GPL-2.0-only
#include <linux/init.h>
#include <linux/console.h>
#include <linux/cpu.h>
#include <linux/delay.h>
#include <linux/memblock.h>
#include <linux/mm.h>
#include <linux/seq_file.h>
#include <linux/utsname.h>
#include <linux/sched/task.h>
#include <linux/sched/task_stack.h>
#include <asm/sections.h>
#include <asm/setup.h>
#include <as-layout.h>
#include <kern_util.h>
#include <os.h>
#include <horn.h>

unsigned long uml_physmem = 0x20000000;
unsigned long uml_reserved, high_physmem, start_vm, end_vm, end_iomem;
unsigned long long physmem_size;
unsigned long task_size = 0x10000000;
unsigned long stacksizelim = 8 * 1024 * 1024;
unsigned long brk_start;
unsigned long host_fp_size = 512;
struct cpuinfo_um boot_cpu_data = { .cache_alignment = 64 };
static char command_line[] = "console=hvc0 rdinit=/init lpj=1000000 panic=-1";
static void early_write(struct console *c, const char *s, unsigned int n) { horn_console_write(s, n); }
static struct console horn_early_console = { .name = "horn", .write = early_write, .flags = CON_BOOT | CON_PRINTBUFFER, .index = -1 };
void __init setup_arch(char **cmdline_p)
{
 extern void horn_console_init(void);
 horn_console_init();
 physmem_size = horn_ram_bytes();
 uml_reserved = ALIGN((unsigned long)_end + PAGE_SIZE, PAGE_SIZE);
 high_physmem = uml_physmem + physmem_size;
 end_iomem = high_physmem;
 start_vm = 0x40000000;
 end_vm = 0x44000000;
 brk_start = (unsigned long)_end;
 memblock_add(0, physmem_size);
 memblock_reserve(0, uml_reserved - uml_physmem);
 min_low_pfn = PFN_UP(uml_reserved - uml_physmem);
 max_low_pfn = physmem_size >> PAGE_SHIFT;
 max_pfn = max_low_pfn;
 strscpy(boot_command_line, command_line, COMMAND_LINE_SIZE);
 *cmdline_p = command_line;
 init_utsname()->machine[0] = 0;
 strscpy(init_utsname()->machine, "x86_64", sizeof(init_utsname()->machine));
 arch_task_struct_size = sizeof(struct task_struct) + host_fp_size;
 set_cpu_possible(0, true);
 paging_init();
}
static int start_linux(void *unused) { extern void start_kernel(void); start_kernel(); return 0; }
void horn_start(void)
{
 horn_host_init();
 cpu_tasks[0] = &init_task;
 register_console(&horn_early_console);
 init_task.thread.request.thread.proc = start_linux;
 init_task.thread.request.thread.arg = NULL;
 start_idle_thread(task_stack_page(&init_task), &init_task.thread.switch_buf);
 for (;;) {}
}
void arch_cpu_idle_dead(void) { panic("Horn CPU offline unsupported"); }
void arch_cpu_finalize_init(void) {}
static void *cpu_start(struct seq_file *m, loff_t *p) { return *p == 0 ? &boot_cpu_data : NULL; }
static void *cpu_next(struct seq_file *m, void *v, loff_t *p) { ++*p; return NULL; }
static void cpu_stop(struct seq_file *m, void *v) {}
static int cpu_show(struct seq_file *m, void *v) { seq_puts(m, "processor\t: 0\nmodel name\t: Horn vCPU\n"); return 0; }
const struct seq_operations cpuinfo_op = { .start=cpu_start,.next=cpu_next,.stop=cpu_stop,.show=cpu_show };
