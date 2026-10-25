#include <sel4/sel4.h>

static void print(const char *text)
{
#ifdef CONFIG_PRINTING
    while (*text) {
        seL4_DebugPutChar(*text++);
    }
#else
    (void)text;
#endif
}

__attribute__((noreturn)) void root_main(seL4_BootInfo *bootinfo)
{
    if (!bootinfo || !bootinfo->ipcBuffer || !bootinfo->numNodes) {
        print("stub-root-task: invalid BootInfo\n");
    } else {
        print("hi im root task\n");
    }

    seL4_CallWithMRs(seL4_CapInitThreadTCB,
                     seL4_MessageInfo_new(TCBSuspend, 0, 0, 0), 0, 0, 0, 0);

    for (;;) {
        seL4_Yield();
    }
}
