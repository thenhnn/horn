// SPDX-License-Identifier: GPL-2.0-only
// One native seL4 TCB, including all hosted Linux kernel-stack switches.
#include <assert.h>
#include <string.h>
#include <sel4/sel4.h>
#include "protocol.h"
#include "horn.h"
#include <horn/startup.h>

seL4_IPCBuffer *__sel4_ipc_buffer;
#ifdef CONFIG_KERNEL_INVOCATION_REPORT_ERROR_IPC
char __sel4_print_error;
#endif
static seL4_CPtr SELF;
static unsigned root_depth, RECV, ALARM;
static unsigned long reserved_end;
static struct {
 unsigned lifecycle, user_signal, clock_signal, console_signal;
 unsigned console, clock, events, ready;
} initial;
#define USER_EVENT 1
#define CLOCK_EVENT 2
static unsigned long pending;
static horn_ExecutionFpInfo_t fp_info;
static unsigned char fp_last[4096];
static int fp_last_valid;
static horn_cap_t cap(unsigned slot) { return (horn_cap_t){slot,HORN_CAP_COPY,15}; }
static void die(void) {
#ifdef CONFIG_PRINTING
 seL4_DebugDumpScheduler();
#endif
 __asm__ volatile("ud2"); __builtin_unreachable(); }
void __assert_fail(const char *s,const char *f,unsigned l,const char *func) { die(); }
static void trace_error(const char *,unsigned,unsigned,unsigned);
static horn_status_t call(void *ctx,horn_cap_t target,uint64_t label,
 const uint8_t *args,size_t len,horn_cap_list_t *caps,uint8_t *out,size_t capacity,
 size_t *out_len,horn_cap_list_t *result_caps) {
 if(len>HORN_MAX_MESSAGE_BYTES || caps->len>1) return HORN_ERR_INVALID;
 memset(__sel4_ipc_buffer->msg,0,((len+7)/8)*8);
 memcpy(__sel4_ipc_buffer->msg,args,len);
 if(caps->len) seL4_SetCap(0,caps->refs[0].slot);
 seL4_SetCapReceivePath(SELF,RECV,root_depth);
 seL4_MessageInfo_t reply=seL4_Call(target.slot,seL4_MessageInfo_new(label,0,caps->len,(len+7)/8));
 size_t bytes=seL4_MessageInfo_get_length(reply)*8;
 if(bytes<4 || bytes>capacity+11 || seL4_MessageInfo_get_capsUnwrapped(reply) || seL4_MessageInfo_get_extraCaps(reply)>1) return HORN_ERR_TRANSPORT;
 uint32_t status;memcpy(&status,__sel4_ipc_buffer->msg,4);
 if(status) return HORN_ERR_TRANSPORT;
 *out_len=bytes-4<capacity?bytes-4:capacity;
 memcpy(out,(char *)__sel4_ipc_buffer->msg+4,*out_len);
 memset(result_caps,0,sizeof(*result_caps));
 if(seL4_MessageInfo_get_extraCaps(reply)) {horn_cap_t c=cap(RECV);c.mode=HORN_CAP_MOVE;horn_cap_list_push(result_caps,c);}
 for(unsigned i=0;i<caps->len;i++) if(caps->refs[i].mode==HORN_CAP_MOVE)
  if(seL4_CNode_Delete(SELF,caps->refs[i].slot,root_depth)) return HORN_ERR_TRANSPORT;
 return HORN_OK;
}
static horn_transport_t transport={.call=call};
static void trace_error(const char *function,unsigned status,unsigned tag,unsigned error) {
#ifdef CONFIG_PRINTING
 while(*function) seL4_DebugPutChar(*function++);
 unsigned values[]={status,tag,error};
 for(unsigned i=0;i<3;i++) {seL4_DebugPutChar(' ');for(int j=7;j>=0;j--)seL4_DebugPutChar("0123456789abcdef"[(values[i]>>(j*4))&15]);}
 seL4_DebugPutChar(10);
#endif
}
#define CHECK(expr,r) do { horn_status_t status_=(expr); if(status_!=HORN_OK || (r).tag!=HORN_RESULT_OK) {trace_error(__func__,status_,(r).tag,(r).value.err);die();} } while(0)
#define LIFE cap(initial.lifecycle)
static unsigned named(const void *record, size_t size, const char *name) {
 uint64_t selector;
 if(horn_startup_lookup(record,size,(const uint8_t *)name,strlen(name),&selector)!=HORN_STARTUP_OK || selector>UINT32_MAX) die();
 return (unsigned)selector;
}
unsigned long horn_boot_reserved_end(void) { return reserved_end; }
void horn_host_init(const void *record, unsigned long size) {
 if(!record || size>PTRDIFF_MAX || (uintptr_t)record>UINTPTR_MAX-size) die();
 if(horn_startup_validate(record,size)!=HORN_STARTUP_OK) die();
 HornStartup h;memcpy(&h,record,sizeof h);
 if(h.cspace_bits>=32 || h.free_end-h.free_start<2 ||
    h.ipc_buffer>UINTPTR_MAX-4096) die();
 SELF=h.cspace_root;root_depth=h.cspace_bits;
 RECV=h.free_start;ALARM=RECV+1;
 __sel4_ipc_buffer=(void *)(uintptr_t)h.ipc_buffer;
 reserved_end=(uintptr_t)record+size;
 if(reserved_end<h.ipc_buffer+4096) reserved_end=h.ipc_buffer+4096;
 initial.lifecycle=named(record,size,"horn.lifecycle");
 initial.ready=named(record,size,"horn.ready");
 initial.user_signal=named(record,size,"user_signal");
 initial.clock_signal=named(record,size,"clock_signal");
 initial.console_signal=named(record,size,"console_signal");
 initial.events=named(record,size,"events");
 initial.clock=named(record,size,"clock");
 initial.console=named(record,size,"console");
 horn_process_execution_execution_configure_result_t r;
 CHECK(horn_process_execution_execution_configure(&transport,0,LIFE,0x20000000,32*1024*1024,cap(initial.user_signal),&r),r);
 horn_process_execution_execution_fp_info_result_t info;
 CHECK(horn_process_execution_execution_fp_info(&transport,0,LIFE,&info),info);
 fp_info=info.value.ok;
 if(fp_info.bytes<576 || fp_info.bytes>sizeof fp_last || (fp_info.features&7)!=7) die();
 for (;;) {
  horn_process_execution_execution_admit_result_t admitted;
  CHECK(horn_process_execution_execution_admit(&transport,0,LIFE,&admitted),admitted);
  if(admitted.value.ok==32*1024*1024/4096) break;
 }
 horn_clock_interface_alarm_result_t a;
 CHECK(horn_clock_interface_alarm(&transport,0,cap(initial.clock),&a),a);
 if(a.value.ok.slot!=RECV || seL4_CNode_Move(SELF,ALARM,root_depth,SELF,RECV,root_depth)) die();
}
unsigned long horn_fp_bytes(void) { return fp_info.bytes; }
unsigned long long horn_fp_features(void) { return fp_info.features; }
int horn_fp_save(void *buffer) {
 for(unsigned offset=0;offset<fp_info.bytes;) {
  unsigned length=fp_info.bytes-offset; if(length>256) length=256;
  horn_process_execution_execution_fp_read_result_t r;
  CHECK(horn_process_execution_execution_fp_read(&transport,0,LIFE,offset,length,&r),r);
  if(r.value.ok.len!=length) die();
  memcpy((char *)buffer+offset,r.value.ok.data,length);
  offset+=length;
 }
 memcpy(fp_last,buffer,fp_info.bytes);fp_last_valid=1;return 0;
}
int horn_fp_load(const void *buffer) {
 if(fp_last_valid && !memcmp(buffer,fp_last,fp_info.bytes)) return 0;
 fp_last_valid=0;
 for(unsigned offset=0;offset<fp_info.bytes;) {
  horn_list_256_u8_t data={0};data.len=fp_info.bytes-offset;if(data.len>256)data.len=256;
  memcpy(data.data,(const char *)buffer+offset,data.len);
  horn_process_execution_execution_fp_write_result_t r;
  horn_status_t status=horn_process_execution_execution_fp_write(&transport,0,LIFE,offset,data,offset+data.len==fp_info.bytes,&r);
  if(status!=HORN_OK) die();
  if(r.tag!=HORN_RESULT_OK) return -22;
  offset+=data.len;
 }
 memcpy(fp_last,buffer,fp_info.bytes);fp_last_valid=1;return 0;
}
unsigned long horn_ram_bytes(void) { return 32*1024*1024; }
int horn_map(unsigned long space,unsigned long va,unsigned long pfn,unsigned int rights) {
 horn_process_execution_execution_map_result_t r;
 horn_status_t s=horn_process_execution_execution_map(&transport,0,LIFE,space,va,pfn,rights,&r);
 if(s!=HORN_OK) die();return r.tag==HORN_RESULT_OK?0:-12;
}
int horn_unmap(unsigned long space,unsigned long va,unsigned long bytes) {
 horn_process_execution_execution_unmap_result_t r;
 CHECK(horn_process_execution_execution_unmap(&transport,0,LIFE,space,va,bytes,&r),r);return 0;
}
unsigned long horn_space_create(void) {
 horn_process_execution_execution_space_result_t r;
 horn_status_t s=horn_process_execution_execution_space(&transport,0,LIFE,&r);
 if(s!=HORN_OK) die();return r.tag==HORN_RESULT_OK?r.value.ok:0;
}
void horn_space_destroy(unsigned long space) {
 horn_process_execution_execution_destroy_result_t r;
 CHECK(horn_process_execution_execution_destroy(&transport,0,LIFE,space,&r),r);
}
void horn_console_write(const char *data,unsigned long len) {
 while(len) {
  horn_list_256_u8_t bytes={0};bytes.len=len<256?len:256;memcpy(bytes.data,data,bytes.len);
  horn_console_interface_write_result_t r;
  if(horn_console_interface_write(&transport,0,cap(initial.console),bytes,&r)!=HORN_OK) die();
  if(r.tag!=HORN_RESULT_OK) {
   if(r.value.err!=HORN_CONSOLE_ERROR_WOULD_BLOCK) die();
   horn_ConsoleinterfaceWatchArgs_t args={0};
   args.notification.present=true;args.notification.value=cap(initial.console_signal);
   args.interests=HORN_CONSOLE_EVENTS_WRITABLE;
   horn_console_interface_watch_result_t watched;
   CHECK(horn_console_interface_watch(&transport,0,cap(initial.console),args.notification,args.interests,0,&watched),watched);
   while(!(pending&4)) {seL4_Word badge;seL4_Wait(initial.events,&badge);pending|=badge;}
   pending&=~4;continue;
  }
  if(!r.value.ok || r.value.ok>bytes.len) die();data+=r.value.ok;len-=r.value.ok;
 }
}
unsigned long long horn_now_ns(void) {
 horn_clock_interface_now_result_t r;
 CHECK(horn_clock_interface_now(&transport,0,cap(initial.clock),&r),r);
 return r.value.ok*1000;
}
void horn_deadline(unsigned long long deadline) {
 if(deadline) {
  horn_alarm_interface_arm_result_t r;
  CHECK(horn_alarm_interface_arm(&transport,0,cap(ALARM),(deadline+999)/1000,cap(initial.clock_signal),&r),r);
 } else {
  horn_alarm_interface_cancel_result_t r;
  CHECK(horn_alarm_interface_cancel(&transport,0,cap(ALARM),&r),r);
 }
}
int horn_clock_pending(void) {
 seL4_Word badge=0;seL4_Poll(initial.events,&badge);pending|=badge;
 int ready=(pending&CLOCK_EVENT)!=0;pending&=~CLOCK_EVENT;return ready;
}
void horn_idle(void) {
 if(pending&CLOCK_EVENT) return;
 seL4_Word badge;seL4_Wait(initial.events,&badge);pending|=badge;
}
int horn_run(unsigned long space,struct horn_registers *registers,unsigned long *address,unsigned long *code) {
 _Static_assert(sizeof(*registers)==sizeof(horn_ExecutionRegisters_t),"register transport layout");
 horn_ExecutionRegisters_t regs;memcpy(&regs,registers,sizeof(regs));
 horn_process_execution_execution_start_result_t s;
 CHECK(horn_process_execution_execution_start(&transport,0,LIFE,space,regs,&s),s);
 // No Linux code runs between start and stopping the user TCB. Notifications
 // coalesce; an already pending alarm must not be lost before the wait.
 if(!(pending&(USER_EVENT|CLOCK_EVENT))) {seL4_Word badge;seL4_Wait(initial.events,&badge);pending|=badge;}
 horn_process_execution_execution_stop_result_t r;
 CHECK(horn_process_execution_execution_stop(&transport,0,LIFE,&r),r);
 pending&=~USER_EVENT;
 memcpy(registers,&r.value.ok.registers,sizeof(*registers));
 *address=r.value.ok.address;*code=r.value.ok.code;return r.value.ok.reason;
}
void horn_shutdown(unsigned status) {
 horn_process_lifecycle_exit_result_t result;
 (void)horn_process_lifecycle_exit(&transport,0,LIFE,status,&result);
 die();
 __builtin_unreachable();
}
void horn_ready(void) {seL4_Signal(initial.ready);}
