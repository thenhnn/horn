// SPDX-License-Identifier: GPL-2.0-only
//! Per-session observations for one selected sink. Not hardware authority and
//! not a proof that a panel is awake or the main link carries valid video.
use horn_nvidia_gsp::{Error, Result};

#[derive(Clone, Copy, Default)]
pub(crate) struct Watch {
    id: u32,
    pub connected: Option<bool>,
    pub last_activity_us: u64,
    pub revision: u32,
    pub probe_pending: bool,
    pub probes: u8,
    pub trained_revision: Option<u32>,
    pub link_invalid: bool,
    pub publication_started: bool,
    pub recovery_used: bool,
    last_unplug_revision: u32,
    last_replug_revision: u32,
    unplug_events: u32,
    trained_unplug_events: u32,
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
            self.revision = self.revision.checked_add(1).ok_or(Error::Range)?;
            self.connected = Some(plug & self.id != 0);
            if unplug & self.id != 0 {
                self.last_unplug_revision = self.revision;
                self.unplug_events = self.unplug_events.checked_add(1).ok_or(Error::Range)?;
            } else if self.last_unplug_revision > self.last_replug_revision {
                self.last_replug_revision = self.revision;
            }
            self.probe_pending = self.connected == Some(true);
            self.last_activity_us = now;
        }
        Ok(())
    }
    pub fn irq(&mut self, id: u32, now: u64) -> Result<()> {
        if id & self.id != 0 {
            self.revision = self.revision.checked_add(1).ok_or(Error::Range)?;
            self.probe_pending = self.connected == Some(true);
            self.last_activity_us = now;
        }
        Ok(())
    }
    pub fn sample(&mut self, id: u32, connected: bool, now: u64) -> Result<()> {
        if self.id == 0 || id != self.id {
            return Err(Error::State);
        }
        if self.connected != Some(connected) {
            self.revision = self.revision.checked_add(1).ok_or(Error::Range)?;
            self.connected = Some(connected);
            self.last_activity_us = now;
        }
        Ok(())
    }
    pub fn record_training(&mut self, revision: u32) -> Result<()> {
        if self.publication_started
            || self.trained_revision.is_some()
            || self.link_invalid
            || self.revision != revision
            || self.connected != Some(true)
        {
            return Err(Error::State);
        }
        self.trained_revision = Some(revision);
        self.trained_unplug_events = self.unplug_events;
        Ok(())
    }
    pub fn single_unplug_since_training(&self) -> bool {
        self.unplug_events.checked_sub(self.trained_unplug_events) == Some(1)
    }
    pub fn unplugged_since(&self, previous: u32) -> bool {
        self.last_unplug_revision > previous
    }
    pub fn reconnected_since(&self, previous: u32) -> bool {
        self.connected == Some(true)
            && self.single_unplug_since_training()
            && self.last_unplug_revision > previous
            && self.last_replug_revision > self.last_unplug_revision
    }
    /// Consumes the sole pre-publication recovery allowance, never a failed-RPC
    /// retry. Caller also proves hardware PUT/GET=0 and revalidates sink/mode.
    pub fn begin_recovery(&mut self, previous: u32, revision: u32) -> Result<()> {
        if self.publication_started
            || self.recovery_used
            || !self.link_invalid
            || self.trained_revision != Some(previous)
            || self.revision != revision
            || !self.reconnected_since(previous)
        {
            return Err(Error::State);
        }
        self.recovery_used = true;
        self.trained_revision = None;
        self.link_invalid = false;
        self.probe_pending = false;
        Ok(())
    }
    pub fn quiet_us(&self, now: u64) -> Result<u64> {
        now.checked_sub(self.last_activity_us).ok_or(Error::State)
    }
    pub fn settled(&self, id: u32, now: u64, quiet_us: u64) -> Result<bool> {
        Ok(self.id == id && self.connected == Some(true) && self.quiet_us(now)? >= quiet_us)
    }
}
