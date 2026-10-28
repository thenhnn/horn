//! Device launch policy and owner-acknowledged external cleanup.
use crate::{native::target, service::Service};
use alloc::vec::Vec;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_supervisor::{
    IRQ, STOP,
    client::{Bootstrap, close_image, mint},
    protocol as s,
};
const BOOT: Bootstrap = Bootstrap {
    cnode: d::DEVICE_CAPS_SELF_SLOT,
    bits: d::DEVICE_CAPS_SIZE_BITS,
    vm: d::DEVICE_CAPS_VM_SLOT,
    space: d::DEVICE_CAPS_ADDRESS_SPACE_SLOT,
};
struct DriverConfig {
    name: &'static str,
    device: u64,
    ram: u64,
    elf: &'static [u8],
}
include!(concat!(env!("OUT_DIR"), "/drivers.rs"));
struct Managed {
    job: Cap<s::SupervisedJobTag>,
    process: Cap<s::ProcessObjectTag>,
    device: u64,
    device_id: u64,
    service: Cap<Endpoint>,
    client: Option<crate::clients::Client>,
    ready: bool,
    finished: bool,
}
pub struct Drivers {
    records: Vec<Managed>,
}
fn job<'a>(
    service: &'a mut Service,
    cap: Cap<s::SupervisedJobTag>,
) -> s::SupervisorJobClient<&'a mut horn_supervisor::client::Io> {
    s::SupervisorJobClient::new(
        &mut service.io().platform.transport,
        Cap::from_ref(cap.as_ref()),
    )
}
impl Drivers {
    pub fn start(service: &mut Service) -> Self {
        let mut this = Self {
            records: Vec::new(),
        };
        this.records
            .try_reserve(DRIVERS.len())
            .expect("driver records");
        for cfg in DRIVERS {
            this.spawn(service, cfg);
        }
        this
    }
    #[cfg(feature = "boot-tests")]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn ready(&self) -> bool {
        self.records.iter().all(|r| {
            (r.ready || r.finished) && r.client.as_ref().is_none_or(|c| c.ready || c.finished)
        })
    }
    fn spawn(&mut self, service: &mut Service, cfg: &DriverConfig) {
        let (device, badge) = service
            .claim_driver(cfg.device)
            .expect("configured device assignment");
        let io = &mut service.io().platform.transport;
        let image = BOOT.upload(io, cfg.elf);
        let mut request = s::ProcessLaunch::default();
        request.name.len = cfg.name.len() as _;
        request.name.data[..cfg.name.len()].copy_from_slice(cfg.name.as_bytes());
        request.ram_limit = cfg.ram;
        let slots = s::ProcessBootstrap {
            vm: d::DEVICE_DRIVER_CAPS_VM_SLOT,
            address_space: d::DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,
            lifecycle: d::DEVICE_DRIVER_CAPS_LIFECYCLE_SLOT,
            reply: d::DEVICE_DRIVER_CAPS_REPLY_SLOT,
        };
        let managed =
            s::SupervisorFactoryClient::new(&mut *io, target(d::DEVICE_CAPS_SUPERVISOR_SLOT))
                .prepare(&image, request, slots)
                .unwrap();
        close_image(io, image);
        #[cfg(feature = "boot-tests")]
        assert!(matches!(
            s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref())).cleanup_done(),
            Err(CallError::Remote(s::SupervisorError::Busy))
        ));
        let process = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .process()
            .unwrap();
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .watch(&target(d::DEVICE_CAPS_EVENTS_SLOT))
            .unwrap();
        let ready = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .signal(s::ServiceSignal::Ready)
            .unwrap();
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_DEVICE_SLOT,
            device,
            None,
            Rights::ALL,
        );
        io.space.delete(device.as_ref()).unwrap();
        let endpoint = s::ProcessControlClient::new(&mut *io, Cap::from_ref(process.as_ref()))
            .kernel_object(s::ProcessKernelObject::Endpoint)
            .unwrap();
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_SERVICE_SLOT,
            endpoint,
            None,
            Rights::ALL,
        );
        let service = mint(
            io,
            sel4::cap::CNode::from_bits(d::DEVICE_CAPS_SELF_SLOT as _),
            Cap::<Endpoint>::from_ref(endpoint.as_ref()),
            horn_console::CLIENT_BADGE,
            Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
        );
        io.space.delete(endpoint.as_ref()).unwrap();
        let wake = s::ProcessControlClient::new(&mut *io, Cap::from_ref(process.as_ref()))
            .kernel_object(s::ProcessKernelObject::Notification)
            .unwrap();
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_WAKE_SLOT,
            wake,
            None,
            Rights::READ,
        );
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT,
            wake,
            Some(IRQ),
            Rights::WRITE,
        );
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_READY_SLOT,
            ready,
            None,
            Rights::WRITE,
        );
        let stop = mint(
            io,
            sel4::cap::CNode::from_bits(d::DEVICE_CAPS_SELF_SLOT as _),
            Cap::<Notification>::from_ref(wake.as_ref()),
            STOP,
            Rights::WRITE,
        );
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .stop_signal(&stop)
            .unwrap();
        io.space.delete(stop.as_ref()).unwrap();
        s::ProcessControlClient::new(&mut *io, Cap::from_ref(process.as_ref()))
            .bind_notification(&Cap::from_ref(wake.as_ref()))
            .unwrap();
        io.space.delete(wake.as_ref()).unwrap();
        let stopped = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .signal(s::ServiceSignal::Stopped)
            .unwrap();
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_STOPPED_SLOT,
            stopped,
            None,
            Rights::WRITE,
        );
        io.space.delete(stopped.as_ref()).unwrap();
        let runtime = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .client()
            .unwrap();
        BOOT.install(
            io,
            process,
            d::DEVICE_DRIVER_CAPS_SUPERVISOR_SLOT,
            runtime,
            None,
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        io.space.delete(runtime.as_ref()).unwrap();
        io.space.delete(ready.as_ref()).unwrap();
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .start()
            .unwrap();
        self.records.push(Managed {
            job: managed,
            process,
            device: badge,
            device_id: cfg.device,
            service,
            client: None,
            ready: false,
            finished: false,
        });
        sel4::debug_println!("DEVMGR_SERVICE_START name={} policy=devmgr", cfg.name);
    }
    pub fn faulted(&mut self, service: &mut Service, faults: crate::service::Faults) {
        for record in &self.records {
            if record.finished {
                continue;
            }
            let sid = (record.device_id - 1) as usize;
            if faults.overflow
                || sid < 256 && faults.requesters[sid / 64] & (1u64 << (sid % 64)) != 0
            {
                match job(service, record.job).request_stop(s::StopMode::Force) {
                    Ok(()) | Err(CallError::Remote(s::SupervisorError::Busy)) => {}
                    error => panic!("faulted driver stop: {:?}", error),
                }
            }
        }
    }
    pub fn changed(&mut self, service: &mut Service) {
        for record in &mut self.records {
            if record.finished {
                continue;
            }
            if let Some(client) = &mut record.client {
                client.changed(&mut service.io().platform.transport);
            }
            let state = job(service, record.job).status().unwrap();
            if state == s::ServiceStatus::Active && !record.ready {
                record.ready = true;
                if crate::clients::Client::configured() {
                    record.client = Some(crate::clients::Client::start(
                        &mut service.io().platform.transport,
                        record.service,
                    ));
                }
            }
            if state == s::ServiceStatus::AwaitingCleanup {
                if let Some(client) = &mut record.client {
                    client.stop(&mut service.io().platform.transport);
                    client.changed(&mut service.io().platform.transport);
                    if !client.finished {
                        continue;
                    }
                }
                let info = s::ProcessControlClient::new(
                    &mut service.io().platform.transport,
                    Cap::from_ref(record.process.as_ref()),
                )
                .info()
                .unwrap();
                assert_eq!(info.state, s::ProcessState::Stopping);
                #[cfg(feature = "boot-tests")]
                {
                    let orderly = job(service, record.job).cooperative().unwrap();
                    sel4::debug_println!(
                        "DEVMGR_DRIVER_REVOKE cpu_stopped=true orderly={}",
                        orderly
                    );
                }
                service
                    .revoke_driver(record.device)
                    .expect("device isolation/drain");
                #[cfg(feature = "boot-tests")]
                {
                    sel4::debug_println!(
                        "DEVMGR_DRIVER_REVOKE dma_drained=true irq_closed=true mmio_revoked=true"
                    );
                    assert!(service.claim_driver(record.device_id).is_err());
                    sel4::debug_println!("DEVMGR_ASSIGNMENT_PASS retired=true");
                }
                job(service, record.job).cleanup_done().unwrap();
                service
                    .io()
                    .space()
                    .delete(record.process.as_ref())
                    .unwrap();
                service.io().space().delete(record.job.as_ref()).unwrap();
                service
                    .io()
                    .space()
                    .delete(record.service.as_ref())
                    .unwrap();
                record.finished = true;
                sel4::debug_println!("DEVMGR_DRIVER_STOPPED device={}", record.device_id);
            }
        }
    }
}
