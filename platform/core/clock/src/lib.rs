//! Clock-page ABI: immutable calibration, never a periodically updated counter.
#![no_std]
#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ClockPage {
    pub version: u64,
    pub flags: u64,
    pub base_cycles: u64,
    pub base_us: u64,
    pub span_cycles: u64,
    pub span_us: u64,
}
impl From<protocol::ClockCalibration> for ClockPage {
    fn from(c: protocol::ClockCalibration) -> Self {
        Self {
            version: c.version,
            flags: c.flags,
            base_cycles: c.base_cycles,
            base_us: c.base_us,
            span_cycles: c.span_cycles,
            span_us: c.span_us,
        }
    }
}
impl ClockPage {
    /// Valid only with invariant TSC and the current CPU-0-pinned task profile.
    pub fn fast_now(&self) -> Option<u64> {
        if self.version != 1 || self.flags & 1 == 0 || self.span_cycles == 0 {
            return None;
        }
        #[cfg(target_arch = "x86_64")]
        {
            let cycles = cycles();
            let delta = cycles.checked_sub(self.base_cycles)?;
            return Some(self.base_us.saturating_add(
                (delta as u128 * self.span_us as u128 / self.span_cycles as u128) as u64,
            ));
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            None
        }
    }
}
#[cfg(target_arch = "x86_64")]
pub fn cycles() -> u64 {
    unsafe {
        core::arch::x86_64::_mm_lfence();
        let v = core::arch::x86_64::_rdtsc();
        core::arch::x86_64::_mm_lfence();
        v
    }
}
#[cfg(target_arch = "x86_64")]
pub fn invariant_tsc() -> bool {
    core::arch::x86_64::__cpuid(0x80000000).eax >= 0x80000007
        && core::arch::x86_64::__cpuid(0x80000007).edx & (1 << 8) != 0
}
