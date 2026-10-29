// SPDX-License-Identifier: GPL-2.0-only
#include <linux/mm.h>
#include <linux/sched.h>
#include <asm/mmu_context.h>
#include <os.h>
#include <horn.h>
int init_new_context(struct task_struct *t,struct mm_struct *mm) {
 unsigned long id=horn_space_create();
 if (!id) return -ENOMEM;
 mm->context.id.pid=id;
 return 0;
}
void destroy_context(struct mm_struct *mm) { horn_space_destroy(mm->context.id.pid); }
int map(struct mm_id *mm,unsigned long v,unsigned long len,int prot,int fd,unsigned long long off) {
 for(unsigned long i=0;i<len;i+=PAGE_SIZE) {
  /* x86 executable/write mappings imply read. PROT_NONE is unmapped
   * so access/dirty tracking returns through the Linux fault path. */
  int e = prot ? horn_map(mm->pid,v+i,(off+i)>>PAGE_SHIFT,prot | 1)
               : horn_unmap(mm->pid,v+i,PAGE_SIZE);
  if(e) return e;
 }
 return 0;
}
int unmap(struct mm_id *mm,unsigned long v,unsigned long n) { return horn_unmap(mm->pid,v,n); }
int syscall_stub_flush(struct mm_id *mm) { return 0; }
