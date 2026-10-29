# SPDX-License-Identifier: GPL-2.0-only
obj-y := setjmp_64.o ptrace.o ptrace_64.o syscalls_64.o sys_call_table_64.o sys_call_table_64.o signal.o
obj-y += csum-partial_64.o memcpy_64.o memmove_64.o memset_64.o
targets += user-offsets.s
$(obj)/user-offsets.s: c_flags = -Wp,-MD,$(depfile) -S -D__UM_HOST__ -I$(srctree)/arch/horn/include/shared \
 -idirafter $(srctree)/include -idirafter $(objtree)/include
include/generated/user_constants.h: $(obj)/user-offsets.s FORCE
	$(call filechk,offsets,__USER_CONSTANT_H__)
