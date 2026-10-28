//! Monotonic time and bounded independent one-shot alarms; no periodic tick.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::vec::Vec;
use horn_clock::{ClockPage, cycles, invariant_tsc, protocol::*};
use horn_ipc::*;
use horn_supervisor::client::{copy, mint};
const SLOTS: TaskSlots = TaskSlots::new(TIMER_CAPS_FIRST_FREE_SLOT, TIMER_CAPS_SIZE_BITS);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{cnode:TIMER_CAPS_SELF_SLOT,factory:TIMER_CAPS_VM_SLOT,address_space:TIMER_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS});
type Io = Sel4Transport<Sel4CapSpace>;
fn root() -> sel4::cap::CNode {
    sel4::cap::CNode::from_bits(TIMER_CAPS_SELF_SLOT as _)
}
fn cap<T: CapType>(slot: u32) -> Cap<T> {
    horn_runtime::bootstrap_cap(root(), slot)
}
fn hardware_now(io: &mut Io) -> u64 {
    ClockSourceClient::new(io, cap(TIMER_CAPS_SOURCE_SLOT))
        .source_now()
        .unwrap()
}
struct Alarm {
    deadline: Option<u64>,
    signal: Option<Cap<Notification>>,
    _anchor: Cap<Endpoint>,
}
struct Service {
    io: Io,
    page: ClockPage,
    alarms: Vec<Alarm>,
    programmed: Option<u64>,
}
impl Service {
    fn now(&mut self) -> u64 {
        self.page
            .fast_now()
            .unwrap_or_else(|| hardware_now(&mut self.io))
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
            let hardware = hardware_now(&mut self.io);
            let now = self.now();
            let mut source = ClockSourceClient::new(&mut self.io, cap(TIMER_CAPS_SOURCE_SLOT));
            if let Some(deadline) = next {
                source
                    .source_arm(
                        hardware.saturating_add(deadline.saturating_sub(now)),
                        &cap(TIMER_CAPS_SIGNAL_SLOT),
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
            version: c.version,
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
        let anchor = mint(
            &mut self.service.io,
            root(),
            cap::<Endpoint>(TIMER_CAPS_SERVICE_SLOT),
            32 + self.service.alarms.len() as u64,
            Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
        );
        let exported = copy(&mut self.service.io, root(), anchor);
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
        let next = copy(&mut self.service.io, root(), signal);
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
fn main() -> ! {
    let mut io = horn_runtime::transport(root(), SLOTS.application()).unwrap();
    let start = hardware_now(&mut io);
    let t0 = cycles();
    ClockSourceClient::new(&mut io, cap(TIMER_CAPS_SOURCE_SLOT))
        .source_arm(start + 100_000, &cap(TIMER_CAPS_SIGNAL_SLOT))
        .unwrap();
    sel4::cap::Notification::from_bits(TIMER_CAPS_EVENTS_SLOT as _).wait();
    let t1 = cycles();
    let end = hardware_now(&mut io);
    assert!(end > start && t1 > t0);
    let page = ClockPage {
        version: 1,
        flags: invariant_tsc() as u64,
        base_cycles: t1,
        base_us: end,
        span_cycles: t1 - t0,
        span_us: end - start,
    };
    sel4::debug_println!(
        "TIMERD_READY calibration=immutable fast_tsc={} alarm=one-shot",
        page.flags != 0
    );
    let mut service = Service {
        io,
        page,
        alarms: Vec::new(),
        programmed: None,
    };
    horn_runtime::notify_ready(TIMER_CAPS_READY_SLOT);
    let endpoint = sel4::cap::Endpoint::from_bits(TIMER_CAPS_SERVICE_SLOT as _);
    let reply = sel4::cap::Reply::from_bits(TIMER_CAPS_REPLY_SLOT as _);
    let receive = root().absolute_cptr(sel4::CPtr::from_bits(SLOTS.public_receive() as _));
    loop {
        service.pump();
        sel4::with_ipc_buffer_mut(|ipc| ipc.set_recv_slot(&receive));
        let (info, badge) = endpoint.recv(reply);
        if badge == 1 {
            service.programmed = None;
            receive.delete().unwrap();
            continue;
        }
        let request = {
            let mut server = Sel4Server::new(root(), endpoint, receive, &mut service.io.space);
            server.decode_received_dynamic(
                info,
                badge,
                reply,
                clock_host_request_layout,
                ClockHostRequest::decode,
            )
        };
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
        receive.delete().unwrap();
    }
}
