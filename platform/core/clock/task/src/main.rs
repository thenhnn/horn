#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use horn_clock::{
    TimeManager,
    protocol::*,
    x86_64::{TscClockPage, TscTimeManager, cycles, invariant_tsc},
};
use horn_ipc::*;
horn_runtime::task!(main);

type Io = Sel4Transport<Sel4CapSpace>;
fn hardware_now(io: &mut Io, ctx: &horn_runtime::Context) -> u64 {
    ClockSourceClient::new(io, ctx.cap("source").unwrap())
        .source_now()
        .unwrap()
}

struct Alarm {
    deadline: Option<u64>,
    signal: Option<Cap<Notification>>,
    _anchor: Cap<Endpoint>,
}

struct Service {
    ctx: horn_runtime::Context,
    io: Io,
    page: TscClockPage,
    alarms: Vec<Alarm>,
    programmed: Option<u64>,
}
impl Service {
    fn now(&mut self) -> u64 {
        TscTimeManager::now(&self.page).unwrap_or_else(|| hardware_now(&mut self.io, &self.ctx))
    }
    fn pump(&mut self) {
        let now = self.now();
        for alarm in &mut self.alarms {
            if alarm.deadline.is_some_and(|d| d <= now) {
                alarm.deadline = None;
                if let Some(note) = alarm.signal.take() {
                    self.io
                        .space
                        .cap::<sel4::cap_type::Notification>(note.as_ref())
                        .unwrap()
                        .signal();
                    self.io.space.delete(note.as_ref()).unwrap();
                }
            }
        }
        let next = self.alarms.iter().filter_map(|a| a.deadline).min();
        if next != self.programmed {
            let hardware = hardware_now(&mut self.io, &self.ctx);
            let now = self.now();
            let mut source = ClockSourceClient::new(&mut self.io, self.ctx.cap("source").unwrap());
            if let Some(deadline) = next {
                source
                    .source_arm(
                        hardware.saturating_add(deadline.saturating_sub(now)),
                        &self.ctx.cap("signal").unwrap(),
                    )
                    .unwrap();
            } else {
                source.source_cancel().unwrap();
            }
            self.programmed = next;
        }
    }
}
struct Caller<'a> {
    service: &'a mut Service,
    badge: u64,
}
impl Caller<'_> {
    fn index(&self) -> Result<usize, ClockError> {
        let i = self.badge.checked_sub(32).ok_or(ClockError::Denied)? as usize;
        if i >= self.service.alarms.len() {
            return Err(ClockError::Denied);
        }
        Ok(i)
    }
}
impl ClockInterface for Caller<'_> {
    fn now(&mut self) -> Result<u64, ClockError> {
        if self.badge != 16 {
            return Err(ClockError::Denied);
        }
        Ok(self.service.now())
    }
    fn calibration(&mut self) -> Result<ClockCalibration, ClockError> {
        if self.badge != 16 {
            return Err(ClockError::Denied);
        }
        let c = self.service.page;
        Ok(ClockCalibration {
            flags: c.flags,
            base_cycles: c.base_cycles,
            base_us: c.base_us,
            span_cycles: c.span_cycles,
            span_us: c.span_us,
        })
    }
    fn alarm(&mut self) -> Result<Cap<AlarmObjectTag>, ClockError> {
        if self.badge != 16 {
            return Err(ClockError::Denied);
        }
        if self.service.alarms.len() == 8 {
            return Err(ClockError::Exhausted);
        }
        let anchor = self
            .service
            .ctx
            .mint(
                &mut self.service.io,
                self.service.ctx.cap::<Endpoint>("service").unwrap(),
                32 + self.service.alarms.len() as u64,
                Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
            )
            .unwrap();
        let exported = self.service.ctx.copy(&mut self.service.io, anchor).unwrap();
        self.service.alarms.push(Alarm {
            deadline: None,
            signal: None,
            _anchor: anchor,
        });
        Ok(Cap::from_ref(exported.as_ref()))
    }
}
impl AlarmInterface for Caller<'_> {
    fn arm(&mut self, deadline_us: u64, signal: Cap<Notification>) -> Result<(), ClockError> {
        let i = self.index()?;
        if signal.as_ref().unwrapped_badge().is_some() {
            return Err(ClockError::Denied);
        }
        let next = self.service.ctx.copy(&mut self.service.io, signal).unwrap();
        if let Some(old) = self.service.alarms[i].signal.replace(next) {
            self.service.io.space.delete(old.as_ref()).unwrap();
        }
        self.service.alarms[i].deadline = Some(deadline_us);
        Ok(())
    }
    fn cancel(&mut self) -> Result<(), ClockError> {
        let i = self.index()?;
        self.service.alarms[i].deadline = None;
        if let Some(old) = self.service.alarms[i].signal.take() {
            self.service.io.space.delete(old.as_ref()).unwrap();
        }
        Ok(())
    }
}
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let placement = horn_supervisor::protocol::ProcessLifecycleClient::new(
        &mut io,
        ctx.cap("horn.lifecycle").unwrap(),
    )
    .placement()
    .unwrap();
    let start = hardware_now(&mut io, &ctx);
    let t0 = cycles();
    ClockSourceClient::new(&mut io, ctx.cap("source").unwrap())
        .source_arm(start + 100_000, &ctx.cap("signal").unwrap())
        .unwrap();
    ctx.wait("events").unwrap();
    let t1 = cycles();
    let end = hardware_now(&mut io, &ctx);
    assert!(end > start && t1 > t0);
    let page = TscClockPage {
        abi: 1,
        // Invariant TSC alone does not establish cross-CPU synchronization.
        // Every client uses the hardware/RPC domain until that is validated.
        flags: (placement.cpu_count == 1 && invariant_tsc()) as u64,
        base_cycles: t1,
        base_us: end,
        span_cycles: t1 - t0,
        span_us: end - start,
    };
    sel4::debug_println!(
        "TIMERD_READY calibration=immutable fast_tsc={} alarm=one-shot",
        page.flags != 0
    );
    let mut inbox = ctx.inbox("service").unwrap();
    let mut service = Service {
        ctx,
        io,
        page,
        alarms: Vec::new(),
        programmed: None,
    };
    service.ctx.notify_ready().unwrap();
    loop {
        service.pump();
        let mut received = inbox.receive();
        let badge = received.badge();
        if badge == 1 {
            service.programmed = None;
            continue;
        }
        let request = received.decode(
            &mut service.io.space,
            clock_host_request_layout,
            ClockHostRequest::decode,
        );
        if let Ok((_, request, reply)) = request {
            let response = clock_host_dispatch(
                request,
                &mut Caller {
                    service: &mut service,
                    badge,
                },
            );
            clock_host_reply(reply, &mut service.io.space, &response).unwrap();
        }
    }
}
