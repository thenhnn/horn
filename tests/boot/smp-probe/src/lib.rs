//! Test-only shared-page layout. This is not a cross-process VM sharing API.
#![no_std]
use core::sync::atomic::AtomicU64;
pub const ADDRESS: usize = 0x43ff_f000;
#[repr(C)]
pub struct Shared {
    pub cpus: [usize; 2],
    pub ready: AtomicU64,
    pub command: AtomicU64,
    pub progress: [AtomicU64; 2],
    pub apic: [AtomicU64; 2],
}
impl Shared {
    pub fn new(cpus: [usize; 2]) -> Self {
        Self {
            cpus,
            ready: AtomicU64::new(0),
            command: AtomicU64::new(0),
            progress: [const { AtomicU64::new(0) }; 2],
            apic: [const { AtomicU64::new(0) }; 2],
        }
    }
}
