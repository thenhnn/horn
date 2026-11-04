use crate::{TimeManager, protocol::ClockCalibration};
use horn_ipc::{Decoder, Encoder, Wire, WireCaps};

/// ABI 1 uses the IDL wire format, not a Rust record's native layout.
/// Initialized before publication; never modified while clients can read it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TscClockPage {
    bytes: [u8; ClockCalibration::SIZE],
}
const _: () = assert!(ClockCalibration::SIZE == 48);
impl From<ClockCalibration> for TscClockPage {
    fn from(value: ClockCalibration) -> Self {
        let mut page = Self {
            bytes: [0; ClockCalibration::SIZE],
        };
        value.encode(&mut Encoder::new(&mut page.bytes));
        page
    }
}
impl TscClockPage {
    fn calibration(&self) -> Option<ClockCalibration> {
        let mut caps = WireCaps::default();
        let c = ClockCalibration::decode(&mut Decoder::new(&self.bytes, &mut caps)).ok()?;
        (c.abi == 1 && c.span_cycles != 0 && c.span_us != 0).then_some(c)
    }
    pub fn is_valid(&self) -> bool {
        self.calibration().is_some()
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
    fn now(page: &Self::SharedPage) -> Option<u64> {
        let c = page.calibration()?;
        if !c.fast_tsc {
            return None;
        }
        let delta = cycles().checked_sub(c.base_cycles)?;
        let elapsed = u64::try_from(delta as u128 * c.span_us as u128 / c.span_cycles as u128)
            .unwrap_or(u64::MAX);
        Some(c.base_us.saturating_add(elapsed))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn value() -> ClockCalibration {
        ClockCalibration {
            abi: 1,
            fast_tsc: false,
            base_cycles: 0,
            base_us: 42,
            span_cycles: 10,
            span_us: 1,
        }
    }
    #[test]
    fn wire_boolean_roundtrip_and_abi_validation() {
        let page = TscClockPage::from(value());
        assert!(page.is_valid());
        assert_eq!(TscTimeManager::now(&page), None);
        assert!(!page.calibration().unwrap().fast_tsc);
        let fast = TscClockPage::from(ClockCalibration {
            fast_tsc: true,
            ..value()
        });
        assert!(fast.calibration().unwrap().fast_tsc);
        assert!(!TscClockPage::from(ClockCalibration { abi: 2, ..value() }).is_valid());
        assert!(
            !TscClockPage::from(ClockCalibration {
                span_cycles: 0,
                ..value()
            })
            .is_valid()
        );
        assert!(
            !TscClockPage::from(ClockCalibration {
                span_us: 0,
                ..value()
            })
            .is_valid()
        );
    }
}
