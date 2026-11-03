//! Test-only startup fixtures; never compiled into ordinary devmgr builds.
use horn_ipc::*;
use horn_supervisor::{
    client::{Io, close_image, mint},
    protocol as s,
};
use s::{ProcessControlClient as _, SupervisorFactoryClient as _, SupervisorJobClient as _};
static ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/probe.elf"));
pub struct Observer {
    job: Option<OwnedCap<s::SupervisedJobTag>>,
    process: Option<OwnedCap<s::ProcessObjectTag>>,
    pub finished: bool,
}
impl Observer {
    pub fn start(parent: &mut crate::native::Io) -> Self {
        let ctx = &parent.ctx;
        let boot = parent.boot;
        let root = parent.transport.space.root;
        let io = &mut parent.transport;
        let image = boot.upload(io, ELF);
        let mut request = s::ProcessLaunch::default();
        let name = b"devmgr-observer";
        request.name.len = name.len() as _;
        request.name.data[..name.len()].copy_from_slice(name);
        request.ram_limit = 8 << 20;
        let slots = s::ProcessBootstrap {
            vm: 1,
            address_space: 2,
            lifecycle: 3,
            reply: 4,
        };
        let managed = ctx
            .object::<s::SupervisorHostTag>("supervisor")
            .unwrap()
            .prepare(&mut *io, &image, request, slots)
            .unwrap();
        close_image(io, image);
        let process = managed.process(&mut *io).unwrap();
        managed
            .watch(&mut *io, &ctx.cap("events").unwrap())
            .unwrap();
        let ready = managed.signal(&mut *io, s::ServiceSignal::Ready).unwrap();
        let observer = mint(
            io,
            root,
            &ctx.cap::<Endpoint>("service").unwrap(),
            horn_devmgr::OBSERVER_BADGE,
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        boot.install(
            io,
            &process,
            6,
            &observer,
            None,
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        io.space.delete(observer.as_ref()).unwrap();
        boot.install(io, &process, 5, &ready, None, Rights::WRITE);
        drop(ready);
        let mut entries = [
            libhorn::startup::Entry {
                name: "horn.vm",
                selector: 1,
            },
            libhorn::startup::Entry {
                name: "horn.address-space",
                selector: 2,
            },
            libhorn::startup::Entry {
                name: "horn.lifecycle",
                selector: 3,
            },
            libhorn::startup::Entry {
                name: "horn.reply",
                selector: 4,
            },
            libhorn::startup::Entry {
                name: "horn.ready",
                selector: 5,
            },
            libhorn::startup::Entry {
                name: "devmgr.observer",
                selector: 6,
            },
        ];
        boot.context(io, &process, &mut entries);
        managed.start(&mut *io).unwrap();
        Self {
            job: Some(managed),
            process: Some(process),
            finished: false,
        }
    }
    pub fn changed(&mut self, io: &mut Io) {
        if self.finished {
            return;
        }
        if self.job.as_ref().unwrap().status(&mut *io).unwrap() != s::ServiceStatus::AwaitingCleanup
        {
            return;
        }
        let info = self.process.as_ref().unwrap().info(&mut *io).unwrap();
        assert_eq!(info.state, s::ProcessState::Stopping);
        assert!(matches!(info.exit, Some(s::ProcessExit::Code(0))));
        self.job.as_ref().unwrap().cleanup_done(&mut *io).unwrap();
        drop(self.process.take());
        drop(self.job.take());
        self.finished = true;
    }
}
pub fn allocator() {
    let allocation = alloc::boxed::Box::new(42u64);
    assert!(!horn_runtime::is_control(
        (&*allocation as *const u64).cast()
    ));
    horn_runtime::logging::debug!("DEVMGR_ALLOCATOR allocator=mimalloc");
}
pub fn before_start() {
    if option_env!("DEVMGR_TEST_EARLY_FAULT") == Some("1") {
        unsafe {
            core::arch::asm!("ud2", options(noreturn));
        }
    }
}
pub fn ready(ctx: &horn_runtime::Context) {
    if option_env!("DEVMGR_TEST_NO_READY") == Some("1") {
        horn_runtime::logging::debug!("DEVMGR_READY_WITHHELD test=true");
    } else {
        ctx.notify_ready().unwrap();
    }
    if option_env!("DEVMGR_TEST_FAULT") == Some("1") {
        unsafe {
            core::arch::asm!("ud2", options(noreturn));
        }
    }
}
