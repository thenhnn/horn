// SPDX-License-Identifier: GPL-2.0-only
#include <linux/memblock.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <asm/pgalloc.h>
#include <as-layout.h>
#include <horn.h>
unsigned long *empty_zero_page;
pgd_t swapper_pg_dir[PTRS_PER_PGD];
int kmalloc_ok;
void __init paging_init(void) {
 unsigned long zones[MAX_NR_ZONES] = {0};
 empty_zero_page = memblock_alloc(PAGE_SIZE, PAGE_SIZE);
 if (!empty_zero_page) panic("Horn zero page allocation");
 zones[ZONE_NORMAL] = max_low_pfn;
 free_area_init(zones);
}
pgd_t *pgd_alloc(struct mm_struct *mm) {
 pgd_t *pgd = __pgd_alloc(mm,0);
 if (pgd) memcpy(pgd + USER_PTRS_PER_PGD, swapper_pg_dir + USER_PTRS_PER_PGD,
                (PTRS_PER_PGD-USER_PTRS_PER_PGD)*sizeof(pgd_t));
 return pgd;
}
static const pgprot_t protection_map[16] = {
 [VM_NONE]=PAGE_NONE,
 [VM_READ]=PAGE_READONLY,
 [VM_WRITE]=PAGE_COPY,
 [VM_READ|VM_WRITE]=PAGE_COPY,
 [VM_EXEC]=PAGE_READONLY_EXEC,
 [VM_READ|VM_EXEC]=PAGE_READONLY_EXEC,
 [VM_WRITE|VM_EXEC]=PAGE_COPY_EXEC,
 [VM_READ|VM_WRITE|VM_EXEC]=PAGE_COPY_EXEC,
 [VM_SHARED]=PAGE_NONE,
 [VM_SHARED|VM_READ]=PAGE_READONLY,
 [VM_SHARED|VM_WRITE]=PAGE_SHARED,
 [VM_SHARED|VM_READ|VM_WRITE]=PAGE_SHARED,
 [VM_SHARED|VM_EXEC]=PAGE_READONLY_EXEC,
 [VM_SHARED|VM_READ|VM_EXEC]=PAGE_READONLY_EXEC,
 [VM_SHARED|VM_WRITE|VM_EXEC]=PAGE_SHARED_EXEC,
 [VM_SHARED|VM_READ|VM_WRITE|VM_EXEC]=PAGE_SHARED_EXEC
};
DECLARE_VM_GET_PAGE_PROT
void __init arch_mm_preinit(void) {}
void __init mem_init(void) { kmalloc_ok = 1; }
void free_initmem(void) { /* Image backing remains admitted and reserved. */ }
void free_initrd_mem(unsigned long s, unsigned long e) {}
void map_memory(unsigned long va, unsigned long phys, unsigned long len, int r, int w, int x) {
 for (unsigned long i=0;i<len;i+=PAGE_SIZE)
  if (horn_map(0,va+i,(phys+i)>>PAGE_SHIFT,(r?1:0)|(w?2:0)|(x?4:0))) panic("Horn kernel mapping va=%lx phys=%lx r=%d w=%d x=%d",va+i,phys+i,r,w,x);
}
int phys_mapping(unsigned long phys, unsigned long long *offset) { *offset=phys; return 0; }
int os_map_memory(void *v,int fd,unsigned long long off,unsigned long len,int r,int w,int x) { map_memory((unsigned long)v,off,len,r,w,x); return 0; }
int os_unmap_memory(void *v,int n) { return horn_unmap(0,(unsigned long)v,n); }
int os_protect_memory(void *v,unsigned long n,int r,int w,int x) { return -EOPNOTSUPP; }
