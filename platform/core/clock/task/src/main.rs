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
use horn_supervisor::protocol as v;
use v::{
    VmAddressSpaceClient as _, VmFactoryClient as _, VmMemoryClient as _, VmRegionClient as _,
};
horn_runtime::task!(main);

type Io = Sel4Transport<Sel4CapSpace>;
fn hardware_now(io: &mut Io, ctx: &horn_runtime::Context) -> u64 {
    ctx.interface::<ClockSource>("source")
        .unwrap()
        .source_now(io)
        .unwrap()
}

struct SharedPage {
    _memory: OwnedCap<v::VmMemoryObjectTag>,
    handle: OwnedCap<v::VmMemoryObjectTag>,
    _region: OwnedCap<v::VmRegionObjectTag>,
}
impl SharedPage {
    fn new(ctx: &horn_runtime::Context, io: &mut Io, page: TscClockPage) -> Option<Self> {
        let factory = ctx.object::<v::VmServiceTag>("horn.vm").ok()?;
        let space = ctx
            .object::<v::VmAddressSpaceObjectTag>("horn.address-space")
            .ok()?;
        let memory = factory
            .allocate(&mut *io, 4096, v::VmBacking::Resident, v::VmProtection(3))
            .ok()?;
        let region = match space.reserve(
            &mut *io,
            v::VmRegionRequest {
                address: 0,
                length: 4096,
                alignment: 4096,
                placement: v::VmPlacement::Anywhere,
                ceiling: v::VmProtection(3),
            },
        ) {
            Ok(region) => region,
            Err(_) => {
                memory.close(&mut *io).expect("clock backing rollback");
                return None;
            }
        };
        let result = (|| {
            let address = region.info(&mut *io).ok()?.address;
            let mut updates = List::default();
            updates.len = 2;
            updates.data[0] = v::VmUpdate::Prepare(v::VmRange {
                offset: 0,
                length: 4096,
            });
            updates.data[1] = v::VmUpdate::Map(v::VmMap {
                offset: 0,
                length: 4096,
                memory_offset: 0,
                protection: v::VmProtection(3),
            });
            region
                .apply(
                    &mut *io,
                    Some(Cap::from_ref(memory.cap_ref())),
                    Parcel::inline(updates),
                )
                .ok()?;
            // Initialized before READY; there are no writers after publication.
            unsafe {
                (address as *mut TscClockPage).write(page);
            }
            memory.delegate(&mut *io, v::VmMemoryRights::READ).ok()
        })();
        if result.is_none() {
            region.destroy(&mut *io).expect("clock mapping rollback");
            memory.close(&mut *io).expect("clock backing rollback");
            return None;
        }
        Some(Self {
            _memory: memory,
            handle: result.unwrap(),
            _region: region,
        })
    }
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
    shared: Option<SharedPage>,
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
            let source = self.ctx.interface::<ClockSource>("source").unwrap();
            if let Some(deadline) = next {
                source
                    .source_arm(
                        &mut self.io,
                        hardware.saturating_add(deadline.saturating_sub(now)),
                        &self.ctx.cap("signal").unwrap(),
                    )
                    .unwrap();
            } else {
                source.source_cancel(&mut self.io).unwrap();
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
impl ClockInterfaceServer for Caller<'_> {
    fn now(&mut self) -> Result<u64, ClockError> {
        if self.badge != 16 {
            return Err(ClockError::Denied);
        }
        Ok(self.service.now())
    }
    fn shared_page(&mut self) -> Result<Cap<VmMemoryObjectTag>, ClockError> {
        if self.badge != 16 {
            return Err(ClockError::Denied);
        }
        let shared = self
            .service
            .shared
            .as_ref()
            .ok_or(ClockError::Unavailable)?;
        // Copy one non-closeable READ handle; requests don't allocate new VM records.
        let handle: Cap<VmMemoryObjectTag> = Cap::from_ref(shared.handle.cap_ref());
        self.service
            .ctx
            .copy(&mut self.service.io, handle)
            .map_err(|_| ClockError::Exhausted)
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
impl AlarmInterfaceServer for Caller<'_> {
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
    let start = hardware_now(&mut io, &ctx);
    let t0 = cycles();
    ctx.interface::<ClockSource>("source")
        .unwrap()
        .source_arm(&mut io, start + 100_000, &ctx.cap("signal").unwrap())
        .unwrap();
    ctx.wait("events").unwrap();
    let t1 = cycles();
    let end = hardware_now(&mut io, &ctx);
    assert!(end > start && t1 > t0);
    // Platform contract: invariant TSC is synchronized across CPUs.
    // No offset probing, watchdog, or live-migration support is provided.
    let fast_tsc = invariant_tsc();
    let page = TscClockPage::from(ClockCalibration {
        abi: 1,
        fast_tsc,
        base_cycles: t1,
        base_us: end,
        span_cycles: t1 - t0,
        span_us: end - start,
    });
    horn_runtime::logging::debug!(
        "{}",
        if fast_tsc {
            "local TSC enabled (fast path)"
        } else {
            "hardware clock (via IPC)"
        }
    );
    let shared = SharedPage::new(&ctx, &mut io, page);
    if shared.is_none() {
        horn_runtime::logging::warn!("shared clock page unavailable; RPC remains available");
    }
    let mut inbox = ctx.inbox("service").unwrap();
    let mut service = Service {
        ctx,
        io,
        page,
        shared,
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
