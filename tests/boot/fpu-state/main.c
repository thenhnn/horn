/* SPDX-License-Identifier: BSD-2-Clause */
/* Trusted native fixture for the extended-state kernel API, not Linux. */
#include <sel4/sel4.h>
#include <interfaces/sel4_client.h>
#include <stdint.h>
#include <stddef.h>
seL4_IPCBuffer *__sel4_ipc_buffer;
static seL4_BootInfo *boot;
static seL4_CPtr next_slot, untyped;
static unsigned char worker_stack[16384] __attribute__((aligned(64)));
static volatile unsigned worker_ready;
static uint64_t worker_features;
static void worker(void) {
 __asm__ volatile("vpcmpeqd %ymm0,%ymm0,%ymm0");
 if ((worker_features&0xe7)==0xe7) {
  __asm__ volatile("vpternlogd $255,%zmm0,%zmm0,%zmm0; vpternlogd $255,%zmm16,%zmm16,%zmm16");
 }
 worker_ready=1;
 for(;;) __asm__ volatile("pause");
}
void *memcpy(void *d,const void *s,size_t n) { unsigned char *p=d; const unsigned char *q=s; while(n--) *p++=*q++; return d; }
void *memset(void *d,int c,size_t n) { unsigned char *p=d; while(n--) *p++=c; return d; }
static void print(const char *s) { while(*s) seL4_DebugPutChar(*s++); }
static void fail(const char *s) { print("FPU_STATE_FAIL ");print(s);print("\n"); for(;;) seL4_TCB_Suspend(seL4_CapInitThreadTCB); }
#define CHECK(c,s) do { if(!(c)) fail(s); } while(0)
#define OK(e,s) CHECK((e)==seL4_NoError,s)
static seL4_CPtr object(seL4_Word type) {
 seL4_CPtr slot=next_slot++;
 CHECK(slot<boot->empty.end,"slots");
 OK(seL4_Untyped_Retype(untyped,type,type==seL4_SchedContextObject ? seL4_MinSchedContextBits : 0,seL4_CapInitThreadCNode,0,0,slot,1),"retype");
 return slot;
}
static unsigned component(unsigned id) {
 unsigned a,b,c,d;
 __asm__ volatile("cpuid":"=a"(a),"=b"(b),"=c"(c),"=d"(d):"a"(13),"c"(id));
 return b;
}
static void bytes(unsigned char *p,unsigned n,unsigned char value) { for(unsigned i=0;i<n;i++) CHECK(p[i]==value,"vector bytes"); }
void probe_main(seL4_BootInfo *bi) {
 boot=bi;__sel4_ipc_buffer=bi->ipcBuffer;next_slot=bi->empty.start;
 for(unsigned i=0;i<bi->untyped.end-bi->untyped.start;i++)
  if(!bi->untypedList[i].isDevice && bi->untypedList[i].sizeBits>=20) {untyped=bi->untyped.start+i;break;}
 CHECK(untyped,"untyped");
 seL4_CPtr pt=object(seL4_X86_PageTableObject), frame=object(seL4_X86_4K), other=object(seL4_TCBObject);
 OK(seL4_X86_PageTable_Map(pt,seL4_CapInitThreadVSpace,0x800000,0),"pt");
 OK(seL4_X86_Page_Map(frame,seL4_CapInitThreadVSpace,0x800000,seL4_AllRights,0),"map");
 unsigned char *raw=(void *)0x800000;
 unsigned lo,hi;
 __asm__ volatile("xgetbv":"=a"(lo),"=d"(hi):"c"(0));
 uint64_t mask=((uint64_t)hi<<32)|lo;
 CHECK((mask&7)==7,"AVX enabled");
 CHECK(seL4_FPUStateSize==4096,"bounded capacity");
 OK(seL4_TCB_ReadFPU(other,frame,0),"new state");
 CHECK(*(uint16_t *)raw==0x037f && *(uint32_t *)(raw+24)==0x1f80,"initial controls");
 CHECK(*(uint64_t *)(raw+512)==0,"initial xstate");
 bytes(raw+160,256,0);
 /* Restore full-width pointers through hardware. AMD may update FIP/FDP
  * only on exceptions, so a plain FLD at a high address is not a portable test. */
 __asm__ volatile("fld1");
 OK(seL4_TCB_ReadFPU(seL4_CapInitThreadTCB,frame,0),"x87 snapshot");
 *(uint64_t *)(raw+512)|=1;
 *(uint16_t *)raw &= ~1u;
 *(uint16_t *)(raw+2) |= 0x81; /* AMD pointer state is meaningful for exceptions. */
 *(uint64_t *)(raw+8)=0x1000000000ull;
 *(uint64_t *)(raw+16)=0x1000000040ull;
 OK(seL4_TCB_WriteFPU(seL4_CapInitThreadTCB,frame,0),"full-width x87 restore");
 OK(seL4_TCB_ReadFPU(seL4_CapInitThreadTCB,frame,0),"full-width x87 save");
 CHECK(*(uint64_t *)(raw+8)==0x1000000000ull,"64-bit x87 FIP");
 CHECK(*(uint64_t *)(raw+16)==0x1000000040ull,"64-bit x87 FDP");
 *(uint16_t *)raw=0x037f;*(uint16_t *)(raw+2)&=~0xffu;
 OK(seL4_TCB_WriteFPU(seL4_CapInitThreadTCB,frame,0),"clear pending exception");
 __asm__ volatile("vpcmpeqd %ymm0,%ymm0,%ymm0");
 if((mask&0xe7)==0xe7) {
  __asm__ volatile("vpternlogd $255,%%zmm0,%%zmm0,%%zmm0; vpternlogd $255,%%zmm16,%%zmm16,%%zmm16; mov $255,%%eax; kmovw %%eax,%%k1" ::: "eax");
 }
 OK(seL4_TCB_ReadFPU(seL4_CapInitThreadTCB,frame,0),"self read");
 bytes(raw+160,16,255);bytes(raw+component(2),16,255);
 if((mask&0xe7)==0xe7) { bytes(raw+component(6),32,255);bytes(raw+component(7),64,255);CHECK(*(uint16_t *)(raw+component(5)+8)==255,"opmask"); }
 CHECK(seL4_TCB_ReadFPU(other,frame,1)==seL4_InvalidArgument,"alignment");
 CHECK(seL4_TCB_ReadFPU(other,frame,64)==seL4_InvalidArgument,"capacity");
 CHECK(seL4_TCB_ReadFPU(other,frame,~(seL4_Word)0)==seL4_InvalidArgument,"overflow");
 CHECK(seL4_TCB_ReadFPU(other,pt,0)==seL4_InvalidCapability,"cap type");
 *(uint64_t *)(raw+520)=1ull<<63;
 CHECK(seL4_TCB_WriteFPU(seL4_CapInitThreadTCB,frame,0)==seL4_InvalidArgument,"compact rejection");
 OK(seL4_TCB_ReadFPU(seL4_CapInitThreadTCB,frame,0),"unchanged after invalid import");
 bytes(raw+160,16,255);bytes(raw+component(2),16,255);
 *(uint64_t *)(raw+528)=1;
 CHECK(seL4_TCB_WriteFPU(other,frame,0)==seL4_InvalidArgument,"reserved rejection");
 *(uint64_t *)(raw+528)=0;
 *(uint64_t *)(raw+512)|=1ull<<63;
 CHECK(seL4_TCB_WriteFPU(other,frame,0)==seL4_InvalidArgument,"feature rejection");
 *(uint64_t *)(raw+512)&=mask;
 memset(raw+160,0x5a,16);memset(raw+component(2),0xa5,16);
 *(uint32_t *)(raw+24)=0xffffffff;
 OK(seL4_TCB_WriteFPU(seL4_CapInitThreadTCB,frame,0),"self write");
 OK(seL4_TCB_ReadFPU(seL4_CapInitThreadTCB,frame,0),"self roundtrip");
 bytes(raw+160,16,0x5a);bytes(raw+component(2),16,0xa5);
 CHECK((*(uint32_t *)(raw+24) & ~*(uint32_t *)(raw+28))==0,"MXCSR sanitized");
 OK(seL4_TCB_WriteFPU(other,frame,0),"other write");
 OK(seL4_TCB_ReadFPU(other,frame,0),"other read");
 bytes(raw+160,16,0x5a);bytes(raw+component(2),16,0xa5);
 seL4_CPtr ro=next_slot++;
 OK(seL4_CNode_Copy(seL4_CapInitThreadCNode,ro,seL4_WordBits,seL4_CapInitThreadCNode,frame,seL4_WordBits,seL4_CapRights_new(0,0,1,0)),"readonly copy");
 CHECK(seL4_TCB_ReadFPU(other,ro,0)==seL4_InvalidCapability,"readonly destination");
 OK(seL4_TCB_WriteFPU(other,ro,0),"readonly source");
 worker_features=mask;
 seL4_CPtr sc=object(seL4_SchedContextObject);
 OK(seL4_SchedControl_ConfigureFlags(bi->schedcontrol.start+1,sc,5000,10000,0,0,0),"remote sc");
 OK(seL4_TCB_Configure(other,seL4_CapInitThreadCNode,seL4_CNode_CapData_new(0,seL4_WordBits-bi->initThreadCNodeSizeBits).words[0],seL4_CapInitThreadVSpace,0,0,0),"remote configure");
 OK(seL4_TCB_SetSchedParams(other,seL4_CapInitThreadTCB,100,100,sc,0),"remote sched");
 seL4_UserContext regs={.rip=(seL4_Word)worker,.rsp=(seL4_Word)(worker_stack+sizeof worker_stack-8),.rflags=0x202};
 OK(seL4_TCB_WriteRegisters(other,1,0,sizeof(regs)/8,&regs),"remote start");
 while(!worker_ready) __asm__ volatile("pause");
 OK(seL4_TCB_ReadFPU(other,frame,0),"remote live read");
 bytes(raw+160,16,255);bytes(raw+component(2),16,255);
 memset(raw+160,0x5a,16);memset(raw+component(2),0xa5,16);
 OK(seL4_TCB_WriteFPU(other,frame,0),"remote live write");
 OK(seL4_TCB_ReadFPU(other,frame,0),"remote roundtrip");
 bytes(raw+160,16,0x5a);bytes(raw+component(2),16,0xa5);
 OK(seL4_TCB_Suspend(other),"remote stop");
 print((mask&0xe7)==0xe7 ? "FPU_STATE_PASS avx=true avx512=true\n" : "FPU_STATE_PASS avx=true avx512=false\n");
 for(;;) seL4_TCB_Suspend(seL4_CapInitThreadTCB);
}
