/* SPDX-License-Identifier: GPL-2.0-only */
#define RUNTIME_DISCARD_EXIT
/* Writable during boot; STRICT_KERNEL_RWX is not advertised by this slice. */
#define RO_AFTER_INIT_DATA
#include <asm/vmlinux.lds.h>
#include <asm/page.h>
OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)
jiffies = jiffies_64;
PHDRS { text PT_LOAD FLAGS(5); ro PT_LOAD FLAGS(4); data PT_LOAD FLAGS(6); }
SECTIONS {
 . = 0x20000000;
 __binary_start = .;
 __executable_start = .;
 _text = .;
 .text : { _stext = .; *(.text.entry) TEXT_TEXT SCHED_TEXT LOCK_TEXT IRQENTRY_TEXT SOFTIRQENTRY_TEXT *(.fixup) } :text
 .init.text : { _sinittext = .; INIT_TEXT _einittext = .; } :text
 _etext = .;
 . = ALIGN(PAGE_SIZE);
 .horn : { KEEP(*(.horn)) } :ro
 .lrodata : { *(.lrodata .lrodata.*) } :ro
 RO_DATA(PAGE_SIZE)
 . = ALIGN(PAGE_SIZE);
 _sdata = .;
 .data : {
  INIT_TASK_DATA(4096 * (1 << CONFIG_KERNEL_STACK_ORDER))
  . = ALIGN(8);
  __start_ro_after_init = .;
  *(.data..ro_after_init) JUMP_TABLE_DATA STATIC_CALL_DATA
  __end_ro_after_init = .;
  DATA_DATA CONSTRUCTORS *(.ldata .ldata.*)
 } :data
 . = ALIGN(PAGE_SIZE);
 __init_begin = .;
 .init.data : { INIT_DATA } :data
 .init.setup : { INIT_SETUP(16) } :data
 .initcall.init : { INIT_CALLS } :data
 .con_initcall.init : { CON_INITCALL } :data
 .init.ramfs : { INIT_RAM_FS } :data
 .uml.exitcall : { __uml_exitcall_begin = .; *(.uml.exitcall.exit) __uml_exitcall_end = .; } :data
 . = ALIGN(PAGE_SIZE);
 __init_end = .;
 /* UP has no separate per-CPU allocation. An empty PERCPU_SECTION with
  * these explicit PHDRs creates a misleading file-backed LOAD tail. */
 __per_cpu_start = .;
 __per_cpu_end = .;
 __per_cpu_load = .;
 _edata = .;
 . = ALIGN(PAGE_SIZE);
 __bss_start = .;
 SBSS(0)
 BSS(0)
 .lbss (NOLOAD) : { *(.lbss .lbss.*) } :data
 __bss_stop = .;
 . = ALIGN(PAGE_SIZE);
 _end = .;
 STABS_DEBUG
 DWARF_DEBUG
 ELF_DETAILS
 DISCARDS
}
