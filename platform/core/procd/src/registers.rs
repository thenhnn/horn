//! Generic x86-64 thread register conversion.
use crate::protocol::ExecutionRegisters;

pub(crate) fn encode_context(c: &sel4::UserContext) -> ExecutionRegisters {
    let r = c.inner();
    ExecutionRegisters {
        ip: r.rip,
        sp: r.rsp,
        flags: r.rflags,
        ax: r.rax,
        bx: r.rbx,
        cx: r.rcx,
        dx: r.rdx,
        si: r.rsi,
        di: r.rdi,
        bp: r.rbp,
        r8: r.r8,
        r9: r.r9,
        r10: r.r10,
        r11: r.r11,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        r15: r.r15,
        fs: r.fs_base,
        gs: r.gs_base,
    }
}
pub(crate) fn decode_context(r: &ExecutionRegisters) -> sel4::UserContext {
    let mut c = sel4::UserContext::default();
    let d = c.inner_mut();
    d.rip = r.ip;
    d.rsp = r.sp;
    d.rflags = r.flags;
    d.rax = r.ax;
    d.rbx = r.bx;
    d.rcx = r.cx;
    d.rdx = r.dx;
    d.rsi = r.si;
    d.rdi = r.di;
    d.rbp = r.bp;
    d.r8 = r.r8;
    d.r9 = r.r9;
    d.r10 = r.r10;
    d.r11 = r.r11;
    d.r12 = r.r12;
    d.r13 = r.r13;
    d.r14 = r.r14;
    d.r15 = r.r15;
    d.fs_base = r.fs;
    d.gs_base = r.gs;
    c
}
