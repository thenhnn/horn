//! Test-only startup fixtures; never compiled into ordinary devmgr builds.
use crate::native::target;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_supervisor::{
    client::{Bootstrap, Io, close_image, mint},
    protocol as s,
};
mod layout {
    include!(concat!(env!("OUT_DIR"), "/probe_layout.rs"));
}
const BOOT: Bootstrap = Bootstrap {
    cnode: d::DEVICE_CAPS_SELF_SLOT,
    bits: d::DEVICE_CAPS_SIZE_BITS,
    vm: d::DEVICE_CAPS_VM_SLOT,
    space: d::DEVICE_CAPS_ADDRESS_SPACE_SLOT,
};
static ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/probe.elf"));
pub struct Observer {
    job: Cap<s::SupervisedJobTag>,
    process: Cap<s::ProcessObjectTag>,
    pub finished: bool,
}
impl Observer {
    pub fn start(io: &mut Io) -> Self {
        let image = BOOT.upload(io, ELF);
        let mut request = s::ProcessLaunch::default();
        let name = b"devmgr-observer";
        request.name.len = name.len() as _;
        request.name.data[..name.len()].copy_from_slice(name);
        request.ram_limit = 8 << 20;
        let slots = s::ProcessBootstrap {
            vm: layout::DEVICE_PROBE_CAPS_VM_SLOT,
            address_space: layout::DEVICE_PROBE_CAPS_ADDRESS_SPACE_SLOT,
            lifecycle: layout::DEVICE_PROBE_CAPS_LIFECYCLE_SLOT,
            reply: layout::DEVICE_PROBE_CAPS_REPLY_SLOT,
        };
        let managed =
            s::SupervisorFactoryClient::new(&mut *io, target(d::DEVICE_CAPS_SUPERVISOR_SLOT))
                .prepare(&image, request, slots)
                .unwrap();
        close_image(io, image);
        let process = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .process()
            .unwrap();
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .watch(&target(d::DEVICE_CAPS_EVENTS_SLOT))
            .unwrap();
        let ready = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .signal(s::ServiceSignal::Ready)
            .unwrap();
        let observer = mint(
            io,
            sel4::cap::CNode::from_bits(d::DEVICE_CAPS_SELF_SLOT as _),
            target::<Endpoint>(d::DEVICE_CAPS_SERVICE_SLOT),
            horn_devmgr::OBSERVER_BADGE,
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        BOOT.install(
            io,
            process,
            layout::DEVICE_PROBE_CAPS_OBSERVER_SLOT,
            observer,
            None,
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        io.space.delete(observer.as_ref()).unwrap();
        BOOT.install(
            io,
            process,
            layout::DEVICE_PROBE_CAPS_READY_SLOT,
            ready,
            None,
            Rights::WRITE,
        );
        io.space.delete(ready.as_ref()).unwrap();
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .start()
            .unwrap();
        Self {
            job: managed,
            process,
            finished: false,
        }
    }
    pub fn changed(&mut self, io: &mut Io) {
        if self.finished {
            return;
        }
        if s::SupervisorJobClient::new(&mut *io, Cap::from_ref(self.job.as_ref()))
            .status()
            .unwrap()
            != s::ServiceStatus::AwaitingCleanup
        {
            return;
        }
        let info = s::ProcessControlClient::new(&mut *io, Cap::from_ref(self.process.as_ref()))
            .info()
            .unwrap();
        assert_eq!(info.state, s::ProcessState::Stopping);
        assert!(matches!(info.exit, Some(s::ProcessExit::Code(0))));
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(self.job.as_ref()))
            .cleanup_done()
            .unwrap();
        io.space.delete(self.process.as_ref()).unwrap();
        io.space.delete(self.job.as_ref()).unwrap();
        self.finished = true;
    }
}
pub fn allocator() {
    let allocation = alloc::boxed::Box::new(42u64);
    assert!(!horn_runtime::is_control(
        (&*allocation as *const u64).cast()
    ));
    sel4::debug_println!("DEVMGR_ALLOCATOR allocator=mimalloc");
}
pub fn before_start() {
    if option_env!("DEVMGR_TEST_EARLY_FAULT") == Some("1") {
        unsafe {
            core::arch::asm!("ud2", options(noreturn));
        }
    }
}
pub fn ready() {
    if option_env!("DEVMGR_TEST_NO_READY") == Some("1") {
        sel4::debug_println!("DEVMGR_READY_WITHHELD test=true");
    } else {
        horn_runtime::notify_ready(d::DEVICE_CAPS_READY_SLOT);
    }
    if option_env!("DEVMGR_TEST_FAULT") == Some("1") {
        unsafe {
            core::arch::asm!("ud2", options(noreturn));
        }
    }
}
