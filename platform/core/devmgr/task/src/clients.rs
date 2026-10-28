//! Configured console consumer for the demo image; no shell/conhost or name lookup.
//! Its authority is an explicit console capability, never device control.
use crate::native::target;
use horn_console::protocol as c;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_supervisor::{
    client::{Bootstrap, Io, close_image},
    protocol as s,
};
static ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/console_client.elf"));
const BOOT: Bootstrap = Bootstrap {
    cnode: d::DEVICE_CAPS_SELF_SLOT,
    bits: d::DEVICE_CAPS_SIZE_BITS,
    vm: d::DEVICE_CAPS_VM_SLOT,
    space: d::DEVICE_CAPS_ADDRESS_SPACE_SLOT,
};
pub struct Client {
    job: Cap<s::SupervisedJobTag>,
    process: Cap<s::ProcessObjectTag>,
    pub ready: bool,
    pub finished: bool,
    stopping: bool,
}
impl Client {
    pub fn configured() -> bool {
        !ELF.is_empty()
    }
    pub fn start(io: &mut Io, service: Cap<Endpoint>) -> Self {
        let image = BOOT.upload(io, ELF);
        let mut request = s::ProcessLaunch::default();
        let name = b"console-greeting";
        request.name.len = name.len() as _;
        request.name.data[..name.len()].copy_from_slice(name);
        request.ram_limit = 8 << 20;
        let slots = s::ProcessBootstrap {
            vm: c::CONSOLE_CLIENT_CAPS_VM_SLOT,
            address_space: c::CONSOLE_CLIENT_CAPS_ADDRESS_SPACE_SLOT,
            lifecycle: c::CONSOLE_CLIENT_CAPS_LIFECYCLE_SLOT,
            reply: c::CONSOLE_CLIENT_CAPS_REPLY_SLOT,
        };
        let job = s::SupervisorFactoryClient::new(&mut *io, target(d::DEVICE_CAPS_SUPERVISOR_SLOT))
            .prepare(&image, request, slots)
            .unwrap();
        close_image(io, image);
        let process = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(job.as_ref()))
            .process()
            .unwrap();
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(job.as_ref()))
            .watch(&target(d::DEVICE_CAPS_EVENTS_SLOT))
            .unwrap();
        let ready = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(job.as_ref()))
            .signal(s::ServiceSignal::Ready)
            .unwrap();
        BOOT.install(
            io,
            process,
            c::CONSOLE_CLIENT_CAPS_READY_SLOT,
            ready,
            None,
            Rights::WRITE,
        );
        io.space.delete(ready.as_ref()).unwrap();
        BOOT.install(
            io,
            process,
            c::CONSOLE_CLIENT_CAPS_SERVICE_SLOT,
            service,
            None,
            Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
        );
        let note = s::ProcessControlClient::new(&mut *io, Cap::from_ref(process.as_ref()))
            .kernel_object(s::ProcessKernelObject::Notification)
            .unwrap();
        BOOT.install(
            io,
            process,
            c::CONSOLE_CLIENT_CAPS_EVENTS_SLOT,
            note,
            None,
            Rights::READ,
        );
        BOOT.install(
            io,
            process,
            c::CONSOLE_CLIENT_CAPS_SIGNAL_SLOT,
            note,
            Some(1),
            Rights::WRITE,
        );
        io.space.delete(note.as_ref()).unwrap();
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(job.as_ref()))
            .start()
            .unwrap();
        Self {
            job,
            process,
            ready: false,
            finished: false,
            stopping: false,
        }
    }
    pub fn stop(&mut self, io: &mut Io) {
        if self.finished || self.stopping {
            return;
        }
        match s::SupervisorJobClient::new(io, Cap::from_ref(self.job.as_ref()))
            .request_stop(s::StopMode::Force)
        {
            Ok(()) | Err(CallError::Remote(s::SupervisorError::Busy)) => {}
            error => panic!("dependent console client stop: {:?}", error),
        }
        self.stopping = true;
    }
    pub fn changed(&mut self, io: &mut Io) {
        if self.finished {
            return;
        }
        let state = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(self.job.as_ref()))
            .status()
            .unwrap();
        if state == s::ServiceStatus::Active {
            self.ready = true;
        }
        if state == s::ServiceStatus::AwaitingCleanup {
            // No hardware sessions or external backing belong to this consumer.
            s::SupervisorJobClient::new(&mut *io, Cap::from_ref(self.job.as_ref()))
                .cleanup_done()
                .unwrap();
            io.space.delete(self.process.as_ref()).unwrap();
            io.space.delete(self.job.as_ref()).unwrap();
            self.finished = true;
        }
    }
}
