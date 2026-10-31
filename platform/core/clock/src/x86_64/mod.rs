use crate::TimeManager;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TscClockPage {
    pub abi: u64, // 1
    pub flags: u64,
    pub base_cycles: u64,
    pub base_us: u64,
    pub span_cycles: u64,
    pub span_us: u64,
}

// TODO: make clock calibration independent from x86_64
impl From<crate::protocol::ClockCalibration> for TscClockPage {
    fn from(c: crate::protocol::ClockCalibration) -> Self {
        Self {
            abi: 1,
            flags: c.flags,
            base_cycles: c.base_cycles,
            base_us: c.base_us,
            span_cycles: c.span_cycles,
            span_us: c.span_us,
        }
    }
}

pub fn cycles() -> u64 {
    unsafe {
        core::arch::x86_64::_mm_lfence();
        core::arch::x86_64::_rdtsc()
    }
}

pub fn invariant_tsc() -> bool {
    core::arch::x86_64::__cpuid(0x80000000).eax >= 0x80000007
        && core::arch::x86_64::__cpuid(0x80000007).edx & (1 << 8) != 0
}

pub struct TscTimeManager;

impl TimeManager for TscTimeManager {
    type SharedPage = TscClockPage;

    fn now(clock_page: &Self::SharedPage) -> Option<u64> {
        if clock_page.abi != 1 || clock_page.flags & 1 == 0 || clock_page.span_cycles == 0 {
            return None;
        }

        let cycles = cycles();
        let delta = cycles.checked_sub(clock_page.base_cycles)?;
        return Some(clock_page.base_us.saturating_add(
            (delta as u128 * clock_page.span_us as u128 / clock_page.span_cycles as u128) as u64,
        ));
    }
}
