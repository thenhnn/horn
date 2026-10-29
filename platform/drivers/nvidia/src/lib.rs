// SPDX-License-Identifier: BSD-2-Clause
//! Ada/AD106 driver sketch, NOT a GPU driver ready to activate a device.
//!
//! Only read-only probing, numerical address checks and serialized RPC tracking
//! exist here. No firmware upload, DMA allocation, IRQ programming or GSP boot.
//! Capabilities, backing lifetimes and cleanup remain the platform adapter's job.
#![no_std]

pub mod address;
pub mod probe;
pub mod rpc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    WrongBoard,
    InvalidRange,
    DeviceUnavailable,
    Backend,
    Busy,
    Timeout,
    ClockRegressed,
    SequenceExhausted,
    Quarantined,
    NoPending,
}
