// SPDX-License-Identifier: GPL-2.0-only
//! Native AD106 GSP port. Parsing is not evidence of firmware boot/authentication.
#![no_std]
extern crate alloc;
mod abi;
pub mod boot;
pub mod falcon;
pub mod firmware;
#[cfg(test)]
mod format_tests;
pub mod gin;
#[cfg(test)]
mod gin_tests;
pub mod gmmu;
pub mod queue;
#[cfg(test)]
mod queue_tests;
pub mod rm;
pub mod tlv;
pub mod vbios;
pub mod wire;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Malformed,
    Version,
    Range,
    Unsupported,
    Exhausted,
    Checksum,
    FirmwareStatus(FirmwareStatus),
    Quarantined,
    Backend,
    Unavailable,
    State,
    Timeout,
    ClockRegressed,
    ClockStopped,
    /// Which validation step rejected the input. A local diagnostic tag, not a
    /// wire status and never a hardware result.
    Step(&'static str),
}
pub type Result<T> = core::result::Result<T, Error>;

/// NV status codes we can name. Anything else keeps its number, so an unknown
/// firmware failure is still actionable from a log line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FirmwareStatus(pub u32);

impl FirmwareStatus {
    fn name(self) -> &'static str {
        match self.0 {
            0x0000_0001 => "INVALID_OPERATION",
            0x0000_001a => "INSUFFICIENT_RESOURCES",
            0x0000_001f => "INVALID_ARGUMENT",
            0x0000_0023 => "INVALID_CLIENT",
            0x0000_0031 => "INVALID_OBJECT",
            0x0000_0033 => "INVALID_OBJECT_HANDLE",
            0x0000_0036 => "INVALID_OBJECT_PARENT",
            0x0000_0039 => "INVALID_OWNER",
            0x0000_003b => "INVALID_PARAMETER",
            0x0000_0040 => "INVALID_STATE",
            0x0000_0051 => "NO_MEMORY",
            0x0000_0055 => "NOT_READY",
            0x0000_0056 => "NOT_SUPPORTED",
            0x0000_0057 => "OBJECT_NOT_FOUND",
            0x0000_0065 => "TIMEOUT",
            _ => "UNKNOWN",
        }
    }
}

impl core::fmt::Debug for FirmwareStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}({:#x})", self.name(), self.0)
    }
}
