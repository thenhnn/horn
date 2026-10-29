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
    FirmwareStatus(u32),
    Quarantined,
    Backend,
    Unavailable,
    State,
    Timeout,
    ClockRegressed,
    ClockStopped,
}
pub type Result<T> = core::result::Result<T, Error>;
