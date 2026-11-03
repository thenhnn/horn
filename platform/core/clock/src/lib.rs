#![no_std]

#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

pub use protocol::{AlarmInterfaceReceiver as Alarm, ClockInterfaceReceiver as Clock};

pub trait TimeManager {
    // shared page struct between timerd and consumer for efficient time retrieval
    // (might store calibration data for TSC, for example)
    type SharedPage: Copy;
    // TODO: we might want Result here?
    fn now(page: &Self::SharedPage) -> Option<u64>;
}

#[cfg(target_arch = "x86_64")]
pub type DefaultTimeManager = x86_64::TscTimeManager;
#[cfg(target_arch = "x86_64")]
pub type DefaultTimePage = <x86_64::TscTimeManager as TimeManager>::SharedPage;
