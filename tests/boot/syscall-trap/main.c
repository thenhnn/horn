/* SPDX-License-Identifier: BSD-2-Clause */
/* Trusted root fixture. Shared VSpace/CSpace, not a Linux/resource manager. */
#include <sel4/sel4.h>
#include <interfaces/sel4_client.h>
#include <stddef.h>
#include <stdint.h>

seL4_IPCBuffer *__sel4_ipc_buffer;
static seL4_BootInfo *boot;
static seL4_CPtr next_slot, untyped;
static unsigned char stacks[2][16384] __attribute__((aligned(64)));
extern void foreign_app(void);

void *memcpy(void *dest, const void *src, size_t n)
{
    unsigned char *d = dest;
    const unsigned char *s = src;
    while (n--) *d++ = *s++;
    return dest;
}
void *memset(void *dest, int c, size_t n)
{
    unsigned char *d = dest;
    while (n--) *d++ = c;
    return dest;
}
static void print(const char *s)
{
    while (*s) seL4_DebugPutChar(*s++);
}
static void fail(const char *s)
{
    print("SYSCALL_TRAP_FAIL "); print(s); print("\n");
    for (;;) seL4_TCB_Suspend(seL4_CapInitThreadTCB);
}
#define CHECK(c, s) do { if (!(c)) fail(s); } while (0)
#define OK(e, s) CHECK((e) == seL4_NoError, s)

static seL4_CPtr object(seL4_Word type, seL4_Word bits)
{
    CHECK(next_slot < boot->empty.end, "slots");
    seL4_CPtr slot = next_slot++;
    OK(seL4_Untyped_Retype(untyped, type, bits, seL4_CapInitThreadCNode,
                          0, 0, slot, 1), "retype");
    return slot;
}

/* Collide with every ordinary MCS selector, plus debug and unknown paths. */
static const seL4_Word selectors[] = {
    0, 39,
    seL4_SysCall, seL4_SysReplyRecv, seL4_SysNBSendRecv, seL4_SysNBSendWait,
    seL4_SysSend, seL4_SysNBSend, seL4_SysRecv, seL4_SysNBRecv,
    seL4_SysWait, seL4_SysNBWait, seL4_SysYield,
    seL4_SysDebugPutChar,
#ifdef CONFIG_DEBUG_BUILD
    seL4_SysDebugHalt,
#endif
    0x12345678
};
#define SELECTOR_COUNT (sizeof(selectors) / sizeof(selectors[0]))

static void native_done(seL4_CPtr endpoint)
{
    /* Clearing the flag must restore actual native IPC, not just flag readback. */
    seL4_Send(endpoint, seL4_MessageInfo_new(0, 0, 0, 0));
    for (;;) seL4_Yield();
}

void probe_main(seL4_BootInfo *bi)
{
    boot = bi;
    __sel4_ipc_buffer = bi->ipcBuffer;
    next_slot = bi->empty.start;
    for (seL4_Word i = 0; i < bi->untyped.end - bi->untyped.start; i++) {
        if (!bi->untypedList[i].isDevice && bi->untypedList[i].sizeBits >= 20) {
            untyped = bi->untyped.start + i;
            break;
        }
    }
    CHECK(untyped, "untyped");
    seL4_CPtr endpoint = object(seL4_EndpointObject, 0);
    seL4_CPtr reply = object(seL4_ReplyObject, 0);
    seL4_CPtr pt = object(seL4_X86_PageTableObject, 0);
    OK(seL4_X86_PageTable_Map(pt, seL4_CapInitThreadVSpace, 0x800000, 0), "ipc pt");
    seL4_CPtr tcbs[2], faults[2];
    for (unsigned i = 0; i < 2; i++) {
        seL4_CPtr tcb = tcbs[i] = object(seL4_TCBObject, 0);
        seL4_CPtr sc = object(seL4_SchedContextObject, seL4_MinSchedContextBits);
        seL4_CPtr ipc = object(seL4_X86_4K, 0);
        seL4_Word addr = 0x800000 + i * 4096;
        OK(seL4_X86_Page_Map(ipc, seL4_CapInitThreadVSpace, addr, seL4_AllRights, 0), "ipc map");
        CHECK(next_slot < bi->empty.end, "badge slot");
        seL4_CPtr fault = faults[i] = next_slot++;
        OK(seL4_CNode_Mint(seL4_CapInitThreadCNode, fault, seL4_WordBits,
                          seL4_CapInitThreadCNode, endpoint, seL4_WordBits,
                          seL4_AllRights, i + 1), "fault badge");
        OK(seL4_SchedControl_ConfigureFlags(bi->schedcontrol.start + i % bi->numNodes,
                                           sc, 5000, 10000, 0, i + 1, 0), "sc");
        OK(seL4_TCB_Configure(tcb, seL4_CapInitThreadCNode,
                             seL4_CNode_CapData_new(0, seL4_WordBits - bi->initThreadCNodeSizeBits).words[0],
                             seL4_CapInitThreadVSpace, 0, addr, ipc), "configure");
        OK(seL4_TCB_SetSchedParams(tcb, seL4_CapInitThreadTCB, 100, 100, sc, fault), "sched");
        seL4_TCB_SetFlags_t flags = seL4_TCB_SetFlags(tcb, 0, 0);
        OK(flags.error, "flags query");
        CHECK(flags.flags == 0, "default flags");
        flags = seL4_TCB_SetFlags(tcb, 0, seL4_TCBFlag_trapSyscalls);
        OK(flags.error, "flags set");
        CHECK(flags.flags & seL4_TCBFlag_trapSyscalls, "supported flag");
        seL4_UserContext regs = {
            .rip = (seL4_Word)foreign_app,
            .rsp = (seL4_Word)(stacks[i] + sizeof stacks[i]),
            .rflags = 0x202, .r13 = (seL4_Word)selectors, .r15 = SELECTOR_COUNT
        };
        OK(seL4_TCB_WriteRegisters(tcb, 0, 0, sizeof(regs) / 8, &regs), "initial regs");
        OK(seL4_TCB_Resume(tcb), "resume");
    }

    unsigned phases[2] = {0}, done = 0;
    while (done < 2) {
        seL4_Word badge;
        seL4_MessageInfo_t msg = seL4_Recv(endpoint, &badge, reply);
        CHECK(badge >= 1 && badge <= 2, "badge");
        unsigned i = badge - 1;
        if (seL4_MessageInfo_get_label(msg) == 0) {
            CHECK(phases[i] == SELECTOR_COUNT + 1, "native completion");
            phases[i]++;
            OK(seL4_TCB_Suspend(tcbs[i]), "stop native");
            done++;
            continue;
        }
        unsigned phase = phases[i]++;
        CHECK(phase <= SELECTOR_COUNT, "phase");
        if (phase < SELECTOR_COUNT) {
            CHECK(seL4_MessageInfo_get_label(msg) == seL4_Fault_UnknownSyscall, "syscall fault");
            CHECK(seL4_MessageInfo_get_length(msg) == seL4_UnknownSyscall_Length, "fault length");
            CHECK(seL4_GetMR(seL4_UnknownSyscall_Syscall) == selectors[phase], "selector");
            CHECK(seL4_GetMR(seL4_UnknownSyscall_RAX) == 39, "fault rax");
            CHECK(seL4_GetMR(seL4_UnknownSyscall_RDX) == selectors[phase], "fault rdx");
        } else {
            CHECK(seL4_MessageInfo_get_label(msg) == seL4_Fault_UserException, "int fault");
            CHECK(seL4_GetMR(seL4_UserException_Number) == 13, "int gp");
            CHECK(seL4_GetMR(seL4_UserException_Code) == ((0x80 << 3) | 2), "int error");
        }
        seL4_UserContext regs;
        OK(seL4_TCB_ReadRegisters(tcbs[i], 0, 0, sizeof(regs) / 8, &regs), "read regs");
        CHECK(regs.rsp == (seL4_Word)(stacks[i] + sizeof stacks[i]) && regs.rsp == regs.r12, "raw rsp");
        CHECK(regs.rbx == 0x123456789abcdef0 && regs.rdi == 0x76543210fedcba98, "raw gprs");
        CHECK(regs.rsi == 101 && regs.r10 == 102 && regs.r8 == 103 && regs.r9 == 104, "arguments");
        if (phase < SELECTOR_COUNT) {
            CHECK(regs.r14 == phase && regs.rax == 39 && regs.rdx == selectors[phase], "phase regs");
            CHECK((regs.rflags & (1u << 10)) && regs.r11 == regs.rflags && regs.rcx == regs.rip + 2, "flags clobbers");
            regs.rax = (seL4_Word)-38;
            regs.rip += 2;
        } else {
            seL4_TCB_SetFlags_t flags = seL4_TCB_SetFlags(tcbs[i], seL4_TCBFlag_trapSyscalls, 0);
            OK(flags.error, "flags clear");
            CHECK(!(flags.flags & seL4_TCBFlag_trapSyscalls), "cleared flag");
            regs.rip = (seL4_Word)native_done;
            regs.rsp -= 8; /* SysV C function-entry alignment. */
            regs.rdi = faults[i];
            regs.rflags = 0x202;
        }
        OK(seL4_TCB_WriteRegisters(tcbs[i], 0, 0, sizeof(regs) / 8, &regs), "reply regs");
        seL4_Send(reply, seL4_MessageInfo_new(0, 0, 0, 0));
    }
    print("SYSCALL_TRAP_PASS raw_rsp=true arguments=true selectors=true int_gp=true native=true\n");
    seL4_TCB_Suspend(seL4_CapInitThreadTCB);
    for (;;) {}
}
