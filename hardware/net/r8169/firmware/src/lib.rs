#![no_std]

use core::future::Future;

pub trait PhyIo {
    fn phy_read(&mut self, reg: u16) -> u16;
    fn phy_write(&mut self, reg: u16, val: u16);
    fn mcu_read(&mut self, reg: u16) -> u16;
    fn mcu_write(&mut self, reg: u16, val: u16);
}

pub trait Timer {
    fn sleep_ms(&mut self, ms: u16) -> impl Future<Output = ()>;
}

#[derive(Debug)]
pub struct Firmware {
    pub name: &'static str,
    pub version: &'static str,
    id: u16,
}

impl Firmware {
    pub async fn run<I: PhyIo + ?Sized, T: Timer + ?Sized>(&self, io: &mut I, timer: &mut T) {
        generated::run(self.id, io, timer).await;
    }
}

#[allow(clippy::possible_missing_else)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/firmware.rs"));
}

pub use generated::FIRMWARE;

mod chip_data {
    include!(concat!(env!("OUT_DIR"), "/chips.rs"));
}

pub use chip_data::MacVersion;

#[derive(Debug)]
pub struct ChipInfo {
    pub mac_version: MacVersion,
    pub name: &'static str,
    pub firmware: Option<&'static Firmware>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchError {
    UnknownXid(u32),
    NeedsExtendedXid,
    UnknownExtendedXid(u32),
}

pub fn match_chip(
    xid: u32,
    gmii: bool,
    extended_xid: Option<u32>,
) -> Result<&'static ChipInfo, MatchError> {
    let rule = chip_data::CHIPS
        .iter()
        .find(|rule| xid & rule.mask == rule.value)
        .ok_or(MatchError::UnknownXid(xid))?;
    if let Some(chip) = &rule.chip {
        return Ok(match chip.mac_version as u8 {
            42 if !gmii => &chip_data::RTL8106EUS,
            46 if !gmii => &chip_data::RTL8107E,
            _ => chip,
        });
    }
    let xid = extended_xid.ok_or(MatchError::NeedsExtendedXid)?;
    chip_data::EXTENDED_CHIPS
        .iter()
        .find(|rule| xid & rule.mask == rule.value)
        .and_then(|rule| rule.chip.as_ref())
        .ok_or(MatchError::UnknownExtendedXid(xid))
}

#[cfg(test)]
mod tests;
