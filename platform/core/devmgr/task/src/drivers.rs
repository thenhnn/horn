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
    abi: u8,
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
    job: Cap<s::SupervisedJobTag>,
    process: Cap<s::ProcessObjectTag>,
    device: u64,
    device_id: u64,
    service: Cap<Endpoint>,
    finished: bool,
}
pub struct Drivers {
    records: Vec<Managed>,
    pending: Vec<(&'static DriverConfig, d::PciDevice)>,
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
                    sel4::debug_println!(
                        "DEVMGR_MATCH ambiguous device={} skipped=true",
                        device.id
                    );
                    continue;
                }
                if cfg.clock {
                    this.pending.push((cfg, device));
                } else {
                    this.spawn(service, cfg, &device, None);
                }
            } else {
                sel4::debug_println!("DEVMGR_MATCH device={} driver=none", device.id);
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
        let io = &mut service.io().platform.transport;
        let directory = target(d::DEVICE_CAPS_CLOCKS_SLOT);
        let generation = r::RegistryDirectoryClient::new(&mut *io, directory)
            .list(0, false)
            .unwrap()
            .generation;
        let mut path = List::default();
        path.len = 9;
        path.data[..9].copy_from_slice(b"monotonic");
        let clock = match r::RegistryDirectoryClient::new(&mut *io, directory).lookup(path) {
            Ok(clock) => clock,
            Err(CallError::Remote(r::RegistryError::NotFound | r::RegistryError::Unavailable)) => {
                r::RegistryDirectoryClient::new(&mut *io, directory)
                    .watch(&target(d::DEVICE_CAPS_EVENTS_SLOT), generation)
                    .unwrap();
                return;
            }
            Err(error) => panic!("driver clock lookup: {:?}", error),
        };
        for (cfg, pci) in core::mem::take(&mut self.pending) {
            self.spawn(service, cfg, &pci, Some(clock));
        }
        service.io().space().delete(clock.as_ref()).unwrap();
    }
    fn spawn(
        &mut self,
        service: &mut Service,
        cfg: &DriverConfig,
        pci: &d::PciDevice,
        clock: Option<Cap<Endpoint>>,
    ) {
        let (device, badge) = service
            .claim_driver(pci.id)
            .expect("configured device assignment");
        service
            .driver_dma_policy(badge, cfg.dma)
            .expect("trusted driver DMA policy");
        let io = &mut service.io().platform.transport;
        let image = BOOT.upload(io, cfg.elf);
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
            cfg.badge,
            Rights(cfg.rights),
        );
        io.space.delete(endpoint.as_ref()).unwrap();
        let path = alloc::format!("{}/{}", cfg.namespace, instance);
        let mut published_path = List::default();
        assert!(path.len() <= 96);
        published_path.len = path.len() as _;
        published_path.data[..path.len()].copy_from_slice(path.as_bytes());
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .publish(published_path, &service)
            .unwrap();
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
        if clock.is_some() {
            let mut path = List::default();
            path.len = 9;
            path.data[..9].copy_from_slice(b"monotonic");
            #[cfg(feature = "boot-tests")]
            {
                assert_eq!(
                    s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref())).require(
                        &Cap::from_ref(managed.as_ref()),
                        path.clone(),
                        d::DEVICE_DRIVER_CAPS_CLOCK_SLOT
                    ),
                    Err(CallError::Remote(s::SupervisorError::Denied))
                );
                let mut outside = List::default();
                outside.len = 17;
                outside.data[..17].copy_from_slice(b"../services/clock");
                assert_eq!(
                    s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref())).require(
                        &target(d::DEVICE_CAPS_CLOCKS_SLOT),
                        outside,
                        d::DEVICE_DRIVER_CAPS_CLOCK_SLOT
                    ),
                    Err(CallError::Remote(s::SupervisorError::Invalid))
                );
            }
            s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
                .require(
                    &target(d::DEVICE_CAPS_CLOCKS_SLOT),
                    path,
                    d::DEVICE_DRIVER_CAPS_CLOCK_SLOT,
                )
                .unwrap();
            let signal = mint(
                io,
                sel4::cap::CNode::from_bits(d::DEVICE_CAPS_SELF_SLOT as _),
                Cap::<Notification>::from_ref(wake.as_ref()),
                4,
                Rights::WRITE,
            );
            BOOT.install(
                io,
                process,
                d::DEVICE_DRIVER_CAPS_ALARM_SIGNAL_SLOT,
                signal,
                None,
                Rights::WRITE,
            );
            io.space.delete(signal.as_ref()).unwrap();
        }
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
        if cfg.abi == 2 {
            const LOG: u32 = 30;
            let sink = s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
                .log_sink()
                .unwrap();
            BOOT.install(
                io,
                process,
                LOG,
                sink,
                None,
                Rights(Rights::WRITE.0 | Rights::GRANT_REPLY.0),
            );
            io.space.delete(sink.as_ref()).unwrap();
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
            entries.sort_unstable_by_key(|e| e.name);
            let size = 8 + entries.iter().map(|e| 16 + e.name.len()).sum::<usize>();
            let mut bytes = alloc::vec![0; size];
            libhorn::bindings::encode(&entries, &mut bytes).unwrap();
            let descriptor = BOOT.upload(io, &bytes);
            s::ProcessControlClient::new(&mut *io, Cap::from_ref(process.as_ref()))
                .startup_context(&descriptor, size as u64)
                .unwrap();
            close_image(io, descriptor);
        }
        s::SupervisorJobClient::new(&mut *io, Cap::from_ref(managed.as_ref()))
            .start()
            .unwrap();
        self.records.push(Managed {
            job: managed,
            process,
            device: badge,
            device_id: pci.id,
            service,
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
            let state = job(service, record.job).status().unwrap();
            if state == s::ServiceStatus::AwaitingCleanup {
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
