// SPDX-License-Identifier: GPL-2.0-only
//! Named timerd authority; no TSC guessing or calibrated spin-loop waits.
use crate::target;
use horn_devmgr::protocol::*;
use horn_driver::DeviceSession;
use horn_ipc::*;
use horn_nvidia_gsp::{Error, Result};
const ALARM: u64 = 4; // private signal granted by trusted driver launch policy
pub struct Time {
    alarm: Cap<AlarmObjectTag>,
    pending_irq: bool,
    last: u64,
}
impl Time {
    pub fn new(session: &mut DeviceSession) -> Result<Self> {
        let alarm =
            ClockInterfaceClient::new(session.transport(), target(DEVICE_DRIVER_CAPS_CLOCK_SLOT))
                .alarm()
                .map_err(|_| Error::Backend)?;
        Ok(Self {
            alarm,
            pending_irq: false,
            last: 0,
        })
    }
    pub fn now(&mut self, session: &mut DeviceSession) -> Result<u64> {
        let now =
            ClockInterfaceClient::new(session.transport(), target(DEVICE_DRIVER_CAPS_CLOCK_SLOT))
                .now()
                .map_err(|_| Error::Backend)?;
        if now < self.last {
            return Err(Error::ClockRegressed);
        }
        self.last = now;
        Ok(now)
    }
    pub fn cancel(&mut self, session: &mut DeviceSession) -> Result<()> {
        AlarmInterfaceClient::new(session.transport(), Cap::from_ref(self.alarm.as_ref()))
            .cancel()
            .map_err(|_| Error::Backend)
    }
    pub fn sleep_until(&mut self, session: &mut DeviceSession, deadline: u64) -> Result<()> {
        if self.now(session)? >= deadline {
            return Ok(());
        }
        AlarmInterfaceClient::new(session.transport(), Cap::from_ref(self.alarm.as_ref()))
            .arm(deadline, &target(DEVICE_DRIVER_CAPS_ALARM_SIGNAL_SLOT))
            .map_err(|_| Error::Backend)?;
        loop {
            if self.now(session)? >= deadline {
                return self.cancel(session);
            }
            let (_, badge) =
                sel4::cap::Notification::from_bits(DEVICE_DRIVER_CAPS_WAKE_SLOT as _).wait();
            self.pending_irq |= badge & horn_supervisor::IRQ != 0;
            if badge & horn_supervisor::STOP != 0 {
                self.cancel(session)?;
                return Err(Error::Quarantined);
            }
            if badge & !(ALARM | horn_supervisor::IRQ | horn_supervisor::STOP) != 0 {
                self.cancel(session)?;
                return Err(Error::State);
            }
            // Early/stale notifications do not finish or renew the deadline.
        }
    }
    #[allow(dead_code)] // Used by the not-yet-connected boot backend.
    pub fn take_irq(&mut self) -> bool {
        core::mem::take(&mut self.pending_irq)
    }
}
