#![no_std]
#![no_main]
extern crate alloc;

use alloc::vec::Vec;
use horn_ipc::*;
use horn_log::protocol::*;
use horn_runtime::{Context, logging};

horn_runtime::task!(main);
const FACTORY: u64 = 16;
const FIRST_SINK: u64 = 32;

struct Binding {
    name: List<u8, 32>,
    _anchor: Cap<Endpoint>,
}
struct Service {
    ctx: Context,
    io: Sel4Transport<Sel4CapSpace>,
    bindings: Vec<Binding>,
}
struct Request<'a> {
    service: &'a mut Service,
    badge: u64,
}
impl LogFactory for Request<'_> {
    fn create_sink(&mut self, name: List<u8, 32>) -> Result<Cap<LogSinkObjectTag>, LogError> {
        if self.badge != FACTORY {
            return Err(LogError::Denied);
        }
        let bytes = name
            .data
            .get(..name.len as usize)
            .ok_or(LogError::Invalid)?;
        if bytes.is_empty() || !bytes.iter().all(|b| (0x21..=0x7e).contains(b)) {
            return Err(LogError::Invalid);
        }
        self.service
            .bindings
            .try_reserve(1)
            .map_err(|_| LogError::Exhausted)?;
        let badge = FIRST_SINK
            .checked_add(self.service.bindings.len() as u64)
            .ok_or(LogError::Exhausted)?;
        let source = self
            .service
            .ctx
            .cap::<Endpoint>("service")
            .map_err(|_| LogError::Invalid)?;
        let anchor = self
            .service
            .ctx
            .mint(
                &mut self.service.io,
                source,
                badge,
                Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
            )
            .map_err(|_| LogError::Exhausted)?;
        let exported = match self.service.ctx.copy(&mut self.service.io, anchor) {
            Ok(cap) => cap,
            Err(_) => {
                self.service
                    .io
                    .space
                    .delete(anchor.as_ref())
                    .map_err(|_| LogError::Output)?;
                return Err(LogError::Exhausted);
            }
        };
        self.service.bindings.push(Binding {
            name,
            _anchor: anchor,
        });
        Ok(Cap::from_ref(exported.as_ref()))
    }
}
impl LogSink for Request<'_> {
    fn write(&mut self, record: LogRecord) -> Result<(), LogError> {
        let index = self.badge.checked_sub(FIRST_SINK).ok_or(LogError::Denied)?;
        let binding = self
            .service
            .bindings
            .get(index as usize)
            .ok_or(LogError::Denied)?;
        let process = core::str::from_utf8(&binding.name.data[..binding.name.len as usize])
            .map_err(|_| LogError::Invalid)?;
        logging::kernel_debug_sink(process, &record)
    }
}
fn main(mut ctx: Context) -> ! {
    let io = ctx.transport().unwrap();
    let mut inbox = ctx.inbox("service").unwrap();
    ctx.notify_ready().unwrap();
    let mut service = Service {
        ctx,
        io,
        bindings: Vec::new(),
    };
    loop {
        let mut received = inbox.receive();
        let badge = received.badge();
        if let Ok((_, request, reply)) = received.decode(
            &mut service.io.space,
            log_host_request_layout,
            LogHostRequest::decode,
        ) {
            let response = log_host_dispatch(
                request,
                &mut Request {
                    service: &mut service,
                    badge,
                },
            );
            log_host_reply(reply, &mut service.io.space, &response).unwrap();
        }
    }
}
