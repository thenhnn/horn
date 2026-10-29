# SPDX-License-Identifier: GPL-2.0-only
SUBARCH := x86_64
HEADER_ARCH := x86
ARCH_DIR := arch/horn
HOST_DIR := arch/x86
SHELL := bash
KBUILD_DEFCONFIG := defconfig
KBUILD_LDS := arch/horn/kernel/vmlinux.lds
KBUILD_CFLAGS += -mcmodel=large -m64 -mno-red-zone -fno-builtin -fno-pie \
 -mno-sse -mno-sse2 -mno-mmx -mno-avx -D__arch_um__ \
 -Dlongjmp=kernel_longjmp -Dsetjmp=kernel_setjmp
KBUILD_CPPFLAGS += -I$(srctree)/arch/horn/include/shared \
 -I$(srctree)/arch/horn/include/shared/skas \
 -I$(srctree)/arch/x86/include -I$(srctree)/arch/x86/include/uapi \
 -I$(objtree)/arch/x86/include/generated -I$(objtree)/arch/x86/include/generated/uapi
KBUILD_AFLAGS += -m64
KBUILD_LDFLAGS += -m elf_x86_64 -z noexecstack
LDFLAGS_vmlinux += --no-warn-rwx-segments
core-y += arch/horn/x86/
libs-y += $(HORN_HOST_ARCHIVE)
PHONY += linux
all: vmlinux
archheaders:
	$(Q)$(MAKE) -f $(srctree)/Makefile ARCH=x86 asm-generic archheaders
archprepare:
	$(Q)$(MAKE) $(build)=arch/horn/x86 include/generated/user_constants.h
export HEADER_ARCH SUBARCH
