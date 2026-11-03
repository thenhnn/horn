// SPDX-License-Identifier: GPL-2.0-only
//! Named timerd authority; no TSC guessing or calibrated spin-loop waits.
use horn_devmgr::protocol::*;
use horn_driver::DeviceSession;
use horn_ipc::*;
use horn_nvidia_gsp::{Error, Result};
const ALARM: u64 = 4; // private signal granted by trusted driver launch policy
pub struct Time {
    clock: Cap<ClockInterfaceTag>,
    signal: Cap<Notification>,
    wake: sel4::cap::Notification,
    alarm: OwnedCap<AlarmObjectTag>,
    pending_irq: bool,
    last: u64,
}
impl Time {
    pub fn new(session: &mut DeviceSession, ctx: &horn_runtime::Context) -> Result<Self> {
        let clock = ctx.cap("devmgr.clock").map_err(|_| Error::Backend)?;
        let signal = ctx.cap("devmgr.alarm_signal").map_err(|_| Error::Backend)?;
        let wake = sel4::cap::Notification::from_bits(
            ctx.cap::<Notification>("devmgr.wake")
                .map_err(|_| Error::Backend)?
                .as_ref()
                .slot as _,
        );
        let alarm = ClockInterfaceReceiver::from_cap(clock)
            .alarm(session.transport())
            .map_err(|_| Error::Backend)?;
        Ok(Self {
            clock,
            signal,
            wake,
            alarm,
            pending_irq: false,
            last: 0,
        })
    }
    pub fn now(&mut self, session: &mut DeviceSession) -> Result<u64> {
        let now = ClockInterfaceReceiver::from_cap(self.clock)
            .now(session.transport())
            .map_err(|_| Error::Backend)?;
        if now < self.last {
            return Err(Error::ClockRegressed);
        }
        self.last = now;
        Ok(now)
    }
    pub fn cancel(&mut self, session: &mut DeviceSession) -> Result<()> {
        self.alarm
            .interface::<AlarmInterfaceReceiver>()
            .cancel(session.transport())
            .map_err(|_| Error::Backend)
    }
    /// Arm without waiting; the bound wake notification also wakes endpoint recv.
    pub fn arm(&mut self, session: &mut DeviceSession, deadline: u64) -> Result<()> {
        self.alarm
            .interface::<AlarmInterfaceReceiver>()
            .arm(session.transport(), deadline, &self.signal)
            .map_err(|_| Error::Backend)
    }
    pub fn sleep_until(&mut self, session: &mut DeviceSession, deadline: u64) -> Result<()> {
        if self.now(session)? >= deadline {
            return Ok(());
        }
        self.arm(session, deadline)?;
        loop {
            if self.now(session)? >= deadline {
                return self.cancel(session);
            }
            let (_, badge) = self.wake.wait();
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
