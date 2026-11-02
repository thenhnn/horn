// SPDX-License-Identifier: GPL-2.0-only
//! Per-session observations for one selected sink. Not hardware authority and
//! not a proof that a panel is awake or the main link carries valid video.
use horn_nvidia_gsp::{Error, Result};

#[derive(Clone, Copy, Default)]
pub(crate) struct Watch {
    id: u32,
    pub connected: Option<bool>,
    pub last_activity_us: u64,
}
impl Watch {
    /// One selection per device session, never reset after a failed attempt.
    pub fn begin(&mut self, id: u32, now: u64) -> Result<()> {
        if self.id != 0 || id.count_ones() != 1 {
            return Err(Error::State);
        }
        self.id = id;
        self.last_activity_us = now;
        Ok(())
    }
    pub fn hpd(&mut self, plug: u32, unplug: u32, now: u64) -> Result<()> {
        if plug & unplug != 0 {
            return Err(Error::Malformed);
        }
        if (plug | unplug) & self.id != 0 {
            self.connected = Some(plug & self.id != 0);
            self.last_activity_us = now;
        }
        Ok(())
    }
    pub fn irq(&mut self, id: u32, now: u64) {
        if id & self.id != 0 {
            self.last_activity_us = now;
        }
    }
    pub fn sample(&mut self, id: u32, connected: bool, now: u64) -> Result<()> {
        if self.id == 0 || id != self.id {
            return Err(Error::State);
        }
        if self.connected != Some(connected) {
            self.connected = Some(connected);
            self.last_activity_us = now;
        }
        Ok(())
    }
    pub fn quiet_us(&self, now: u64) -> Result<u64> {
        now.checked_sub(self.last_activity_us).ok_or(Error::State)
    }
    pub fn settled(&self, id: u32, now: u64, quiet_us: u64) -> Result<bool> {
        Ok(self.id == id && self.connected == Some(true) && self.quiet_us(now)? >= quiet_us)
    }
}
