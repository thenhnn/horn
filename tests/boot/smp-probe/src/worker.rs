//! Two private VSpaces/heaps; only the explicitly provisioned test page is shared.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::vec;
use core::sync::atomic::Ordering;
use horn_ipc::*;
use horn_procd::protocol::*;
use horn_smp_probe::{ADDRESS, Shared};
horn_runtime::task!(main);
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let target = ctx.cap("horn.lifecycle").unwrap();
    let mut task = ProcessLifecycleClient::new(&mut io, target);
    let placement = task.placement().unwrap();
    assert!(placement.cpu_count >= 2);
    let before = task.info().unwrap().charged_bytes;
    // Ordinary VM-backed heap: these RPCs cross CPUs for the remote worker.
    let mut bytes = vec![0u8; 128 * 1024];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = i as u8;
    }
    assert!(bytes.iter().enumerate().all(|(i, b)| *b == i as u8));
    assert!(task.info().unwrap().charged_bytes > before);
    drop(bytes);
    let shared = unsafe { &*(ADDRESS as *const Shared) };
    let role = shared
        .cpus
        .iter()
        .position(|&cpu| cpu == placement.cpu as usize)
        .unwrap();
    let apic = core::arch::x86_64::__cpuid(1).ebx >> 24;
    shared.apic[role].store(apic as u64, Ordering::Relaxed);
    shared.ready.fetch_or(1 << role, Ordering::Release);
    while shared.command.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    // No syscalls, yields or blocking in this phase. The parent observes both
    // distinct-CPU workers advancing before it permits either one to exit.
    while shared.command.load(Ordering::Acquire) == 1 {
        shared.progress[role].fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(core::arch::x86_64::__cpuid(1).ebx >> 24, apic);
    assert_eq!(task.placement().unwrap(), placement);
    task.exit(0).unwrap();
    unreachable!()
}
