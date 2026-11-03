//! Device launch policy and owner-acknowledged external cleanup.
use crate::service::Service;
use alloc::vec::Vec;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_registry::protocol::RegistryDirectoryClient;
use horn_supervisor::{
    IRQ, STOP,
    client::{close_image, mint},
    protocol as s,
};
use owo_colors::OwoColorize;
use s::{ProcessControlClient as _, SupervisorFactoryClient as _, SupervisorJobClient as _};
struct DriverConfig {
    clock: bool,
    name: &'static str,
    rule: horn_devmgr::matching::PciMatch,
    ram: u64,
    dma: horn_devmgr::dma::DmaPolicy,
    elf: &'static [u8],
    namespace: &'static str,
    badge: u64,
    rights: u8,
}
include!(concat!(env!("OUT_DIR"), "/drivers.rs"));
struct Managed {
    job: Option<OwnedCap<s::SupervisedJobTag>>,
    process: Option<OwnedCap<s::ProcessObjectTag>>,
    device: u64,
    device_id: u64,
    service: Cap<Endpoint>,
    finished: bool,
}
pub struct Drivers {
    records: Vec<Managed>,
    pending: Vec<(&'static DriverConfig, d::PciDevice)>,
}
impl Drivers {
    pub fn start(service: &mut Service) -> Self {
        let mut this = Self {
            records: Vec::new(),
            pending: Vec::new(),
        };
        for cfg in DRIVERS {
            cfg.rule.validate().expect("PCI match configuration");
            cfg.dma.validate().expect("DMA policy");
        }
        let devices: Vec<_> = service.devices().cloned().collect();
        for device in devices {
            let mut candidates = DRIVERS.iter().filter(|cfg| cfg.rule.matches(&device));
            let first = candidates.next();
            if let Some(cfg) = first {
                if candidates.next().is_some() {
                    horn_runtime::logging::warn!("ambiguous driver match for device {}", device.id);
                    continue;
                }
                if cfg.clock {
                    this.pending.push((cfg, device));
                } else {
                    this.spawn(service, cfg, &device, None);
                }
            }
        }
        this
    }
    #[cfg(feature = "boot-tests")]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.pending.is_empty()
    }
    /// Arm a generation watch, not a startup spin or a dependency cycle:
    /// timerd itself needs devmgr's already-running HPET source.
    pub fn launch_pending(&mut self, service: &mut Service) {
        if self.pending.is_empty() {
            return;
        }
        use horn_registry::protocol as r;
        let parent = service.io();
        let ctx = &parent.ctx;
        let boot = parent.boot;
        let root = parent.transport.space.root;
        let io = &mut parent.transport;
        let directory = ctx.object::<r::RegistryNodeTag>("clocks").unwrap();
        let generation = r::RegistryDirectory::from_ref(directory.as_ref())
            .list(&mut *io, 0, false)
            .unwrap()
            .generation;
        let mut path = List::default();
        path.len = 9;
        path.data[..9].copy_from_slice(b"monotonic");
        let clock = match r::RegistryDirectory::from_ref(directory.as_ref()).lookup(&mut *io, path)
        {
            Ok(clock) => clock,
            Err(CallError::Remote(r::RegistryError::NotFound | r::RegistryError::Unavailable)) => {
                r::RegistryDirectory::from_ref(directory.as_ref())
                    .watch(&mut *io, &ctx.cap("events").unwrap(), generation)
                    .unwrap();
                return;
            }
            Err(error) => panic!("driver clock lookup: {:?}", error),
        };
        for (cfg, pci) in core::mem::take(&mut self.pending) {
            self.spawn(service, cfg, &pci, Some(&clock));
        }
        drop(clock);
    }
    fn spawn(
        &mut self,
        service: &mut Service,
        cfg: &DriverConfig,
        pci: &d::PciDevice,
        clock: Option<&OwnedCap<Endpoint>>,
    ) {
        let (device, badge) = service
            .claim_driver(pci.id)
            .expect("configured device assignment");
        service
            .driver_dma_policy(badge, cfg.dma)
            .expect("trusted driver DMA policy");
        let parent = service.io();
        let ctx = &parent.ctx;
        let boot = parent.boot;
        let root = parent.transport.space.root;
        let io = &mut parent.transport;
        let image = boot.upload(io, cfg.elf);
        let mut request = s::ProcessLaunch::default();
        let address = pci.address.clone();
        let instance = alloc::format!(
            "{:04x}:{:02x}:{:02x}.{}",
            address.segment,
            address.bus,
            address.device,
            address.function
        );
        let name = alloc::format!("{}@{}", cfg.name, instance);
        assert!(name.len() <= request.name.data.len());
        request.name.len = name.len() as _;
        request.name.data[..name.len()].copy_from_slice(name.as_bytes());
        request.ram_limit = cfg.ram;
        let slots = s::ProcessBootstrap {
            vm: d::DEVICE_DRIVER_CAPS_VM_SLOT,
            address_space: d::DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,
            lifecycle: d::DEVICE_DRIVER_CAPS_LIFECYCLE_SLOT,
            reply: d::DEVICE_DRIVER_CAPS_REPLY_SLOT,
        };
        let managed = ctx
            .object::<s::SupervisorHostTag>("supervisor")
            .unwrap()
            .prepare(&mut *io, &image, request, slots)
            .unwrap();
        close_image(io, image);
        #[cfg(feature = "boot-tests")]
        assert!(matches!(
            managed.cleanup_done(&mut *io,),
            Err(CallError::Remote(s::SupervisorError::Busy))
        ));
        let process = managed.process(&mut *io).unwrap();
        managed
            .watch(&mut *io, &ctx.cap("events").unwrap())
            .unwrap();
        let ready = managed.signal(&mut *io, s::ServiceSignal::Ready).unwrap();
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_DEVICE_SLOT,
            &device,
            None,
            Rights::ALL,
        );
        io.space.delete(device.as_ref()).unwrap();
        let endpoint = process
            .kernel_object(&mut *io, s::ProcessKernelObject::Endpoint)
            .unwrap();
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_SERVICE_SLOT,
            &endpoint,
            None,
            Rights::ALL,
        );
        let service = mint(
            io,
            root,
            &Cap::<Endpoint>::from_ref(endpoint.cap_ref()),
            cfg.badge,
            Rights(cfg.rights),
        );
        drop(endpoint);
        let path = alloc::format!("{}/{}", cfg.namespace, instance);
        let mut published_path = List::default();
        assert!(path.len() <= 96);
        published_path.len = path.len() as _;
        published_path.data[..path.len()].copy_from_slice(path.as_bytes());
        managed.publish(&mut *io, published_path, &service).unwrap();
        let wake = process
            .kernel_object(&mut *io, s::ProcessKernelObject::Notification)
            .unwrap();
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_WAKE_SLOT,
            &wake,
            None,
            Rights::READ,
        );
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT,
            &wake,
            Some(IRQ),
            Rights::WRITE,
        );
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_READY_SLOT,
            &ready,
            None,
            Rights::WRITE,
        );
        if clock.is_some() {
            let mut path = List::default();
            path.len = 9;
            path.data[..9].copy_from_slice(b"monotonic");
            #[cfg(feature = "boot-tests")]
            {
                assert_eq!(
                    managed.require(
                        &mut *io,
                        &Cap::from_ref(managed.cap_ref()),
                        path.clone(),
                        d::DEVICE_DRIVER_CAPS_CLOCK_SLOT
                    ),
                    Err(CallError::Remote(s::SupervisorError::Denied))
                );
                let mut outside = List::default();
                outside.len = 17;
                outside.data[..17].copy_from_slice(b"../services/clock");
                assert_eq!(
                    managed.require(
                        &mut *io,
                        &ctx.cap("clocks").unwrap(),
                        outside,
                        d::DEVICE_DRIVER_CAPS_CLOCK_SLOT
                    ),
                    Err(CallError::Remote(s::SupervisorError::Invalid))
                );
            }
            managed
                .require(
                    &mut *io,
                    &ctx.cap("clocks").unwrap(),
                    path,
                    d::DEVICE_DRIVER_CAPS_CLOCK_SLOT,
                )
                .unwrap();
            let signal = mint(
                io,
                root,
                &Cap::<Notification>::from_ref(wake.cap_ref()),
                4,
                Rights::WRITE,
            );
            boot.install(
                io,
                &process,
                d::DEVICE_DRIVER_CAPS_ALARM_SIGNAL_SLOT,
                &signal,
                None,
                Rights::WRITE,
            );
            io.space.delete(signal.as_ref()).unwrap();
        }
        let stop = mint(
            io,
            root,
            &Cap::<Notification>::from_ref(wake.cap_ref()),
            STOP,
            Rights::WRITE,
        );
        managed.stop_signal(&mut *io, &stop).unwrap();
        io.space.delete(stop.as_ref()).unwrap();
        process
            .bind_notification(&mut *io, &Cap::from_ref(wake.cap_ref()))
            .unwrap();
        drop(wake);
        let stopped = managed.signal(&mut *io, s::ServiceSignal::Stopped).unwrap();
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_STOPPED_SLOT,
            &stopped,
            None,
            Rights::WRITE,
        );
        drop(stopped);
        let runtime = managed.client(&mut *io).unwrap();
        boot.install(
            io,
            &process,
            d::DEVICE_DRIVER_CAPS_SUPERVISOR_SLOT,
            &runtime,
            None,
            Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
        );
        drop(runtime);
        drop(ready);
        {
            const LOG: u32 = d::DEVICE_DRIVER_CAPS_FIRST_FREE_SLOT;
            let sink = managed.log_sink(&mut *io).unwrap();
            boot.install(
                io,
                &process,
                LOG,
                &sink,
                None,
                Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
            );
            drop(sink);
            let mut entries = alloc::vec![
                libhorn::startup::Entry {
                    name: "horn.vm",
                    selector: slots.vm as u64
                },
                libhorn::startup::Entry {
                    name: "horn.address-space",
                    selector: slots.address_space as u64
                },
                libhorn::startup::Entry {
                    name: "horn.lifecycle",
                    selector: slots.lifecycle as u64
                },
                libhorn::startup::Entry {
                    name: "horn.reply",
                    selector: slots.reply as u64
                },
                libhorn::startup::Entry {
                    name: "horn.ready",
                    selector: d::DEVICE_DRIVER_CAPS_READY_SLOT as u64
                },
                libhorn::startup::Entry {
                    name: "horn.log",
                    selector: LOG as u64
                },
                libhorn::startup::Entry {
                    name: "devmgr.device",
                    selector: d::DEVICE_DRIVER_CAPS_DEVICE_SLOT as u64
                },
                libhorn::startup::Entry {
                    name: "devmgr.service",
                    selector: d::DEVICE_DRIVER_CAPS_SERVICE_SLOT as u64
                },
                libhorn::startup::Entry {
                    name: "devmgr.wake",
                    selector: d::DEVICE_DRIVER_CAPS_WAKE_SLOT as u64
                },
                libhorn::startup::Entry {
                    name: "devmgr.irq_signal",
                    selector: d::DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT as u64
                },
                libhorn::startup::Entry {
                    name: "devmgr.stopped",
                    selector: d::DEVICE_DRIVER_CAPS_STOPPED_SLOT as u64
                },
                libhorn::startup::Entry {
                    name: "devmgr.supervisor",
                    selector: d::DEVICE_DRIVER_CAPS_SUPERVISOR_SLOT as u64
                },
            ];
            if cfg.clock {
                entries.push(libhorn::startup::Entry {
                    name: "devmgr.clock",
                    selector: d::DEVICE_DRIVER_CAPS_CLOCK_SLOT as u64,
                });
                entries.push(libhorn::startup::Entry {
                    name: "devmgr.alarm_signal",
                    selector: d::DEVICE_DRIVER_CAPS_ALARM_SIGNAL_SLOT as u64,
                });
            }
            boot.context(io, &process, &mut entries);
        }
        managed.start(&mut *io).unwrap();
        self.records.push(Managed {
            job: Some(managed),
            process: Some(process),
            device: badge,
            device_id: pci.id,
            service,
            finished: false,
        });
        horn_runtime::logging::info!("starting {}", name.bold());
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
                match record
                    .job
                    .as_ref()
                    .unwrap()
                    .request_stop(&mut service.io().transport, s::StopMode::Force)
                {
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
            let state = record
                .job
                .as_ref()
                .unwrap()
                .status(&mut service.io().transport)
                .unwrap();
            if state == s::ServiceStatus::AwaitingCleanup {
                let info = record
                    .process
                    .as_ref()
                    .unwrap()
                    .info(&mut service.io().transport)
                    .unwrap();
                assert_eq!(info.state, s::ProcessState::Stopping);
                #[cfg(feature = "boot-tests")]
                {
                    let orderly = record
                        .job
                        .as_ref()
                        .unwrap()
                        .cooperative(&mut service.io().transport)
                        .unwrap();
                    horn_runtime::logging::debug!(
                        "DEVMGR_DRIVER_REVOKE cpu_stopped=true orderly={}",
                        orderly
                    );
                }
                service
                    .revoke_driver(record.device)
                    .expect("device isolation/drain");
                #[cfg(feature = "boot-tests")]
                {
                    horn_runtime::logging::debug!(
                        "DEVMGR_DRIVER_REVOKE dma_drained=true irq_closed=true mmio_revoked=true"
                    );
                    assert!(service.claim_driver(record.device_id).is_err());
                    horn_runtime::logging::debug!("DEVMGR_ASSIGNMENT_PASS retired=true");
                }
                record
                    .job
                    .as_ref()
                    .unwrap()
                    .cleanup_done(&mut service.io().transport)
                    .unwrap();
                drop(record.process.take());
                drop(record.job.take());
                service
                    .io()
                    .space()
                    .delete(record.service.as_ref())
                    .unwrap();
                record.finished = true;
                horn_runtime::logging::info!("device {} stopped", record.device_id);
            }
        }
    }
}
