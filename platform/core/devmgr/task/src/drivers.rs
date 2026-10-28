//! Device policy owns matching/assignment and external cleanup. Supervisor sees
//! only generic jobs, notification sinks and an explicit cleanup acknowledgment.
use alloc::vec::Vec;
use horn_devmgr::protocol as d;
use horn_ipc::*;
use horn_supervisor::{client::{Bootstrap,close_image,mint},protocol as s,IRQ,STOP};
use crate::{native::target,service::Service};
const BOOT:Bootstrap=Bootstrap{cnode:d::DEVICE_CAPS_SELF_SLOT,bits:d::DEVICE_CAPS_SIZE_BITS,vm:d::DEVICE_CAPS_VM_SLOT,space:d::DEVICE_CAPS_ADDRESS_SPACE_SLOT};
struct DriverConfig {name:&'static str,device:u64,ram:u64,elf:&'static [u8]}
include!(concat!(env!("OUT_DIR"),"/drivers.rs"));
static PROBE:&[u8]=include_bytes!(concat!(env!("OUT_DIR"),"/probe.elf"));
struct Managed {job:Cap<s::SupervisedJobTag>,process:Cap<s::ProcessObjectTag>,device:Option<u64>,device_id:u64,ready:bool,finished:bool,probe:bool}
pub struct Drivers {records:Vec<Managed>,reported:bool}
fn job<'a>(service:&'a mut Service,cap:Cap<s::SupervisedJobTag>)->s::SupervisorJobClient<&'a mut horn_supervisor::client::Io>{
    s::SupervisorJobClient::new(&mut service.io().platform.transport,Cap::from_ref(cap.as_ref()))
}
impl Drivers {
    pub fn start(service:&mut Service)->Self {
        let mut this=Self{records:Vec::new(),reported:false};this.records.try_reserve(DRIVERS.len().max(1)).expect("driver records");
        if DRIVERS.is_empty(){this.spawn(service,"devmgr-observer",PROBE,8<<20,None);}else{
            for cfg in DRIVERS {this.spawn(service,cfg.name,cfg.elf,cfg.ram,Some(cfg.device));}
        }
        this
    }
    fn spawn(&mut self,service:&mut Service,name:&str,elf:&[u8],ram:u64,device_id:Option<u64>) {
        let claimed=device_id.map(|id|service.claim_driver(id).expect("configured device assignment"));
        let io=&mut service.io().platform.transport;
        let image=BOOT.upload(io,elf);
        let mut request=s::ProcessLaunch::default();request.name.len=name.len() as _;request.name.data[..name.len()].copy_from_slice(name.as_bytes());request.ram_limit=ram;
        let slots=if claimed.is_some(){s::ProcessBootstrap{vm:d::DEVICE_DRIVER_CAPS_VM_SLOT,address_space:d::DEVICE_DRIVER_CAPS_ADDRESS_SPACE_SLOT,lifecycle:d::DEVICE_DRIVER_CAPS_LIFECYCLE_SLOT,reply:d::DEVICE_DRIVER_CAPS_REPLY_SLOT}}
            else{s::ProcessBootstrap{vm:d::DEVICE_PROBE_CAPS_VM_SLOT,address_space:d::DEVICE_PROBE_CAPS_ADDRESS_SPACE_SLOT,lifecycle:d::DEVICE_PROBE_CAPS_LIFECYCLE_SLOT,reply:d::DEVICE_PROBE_CAPS_REPLY_SLOT}};
        let managed=s::SupervisorFactoryClient::new(&mut *io,target(d::DEVICE_CAPS_SUPERVISOR_SLOT)).prepare(&image,request,slots).unwrap();close_image(io,image);
        assert!(matches!(s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).cleanup_done(),Err(CallError::Remote(s::SupervisorError::Busy))));
        let process=s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).process().unwrap();
        s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).watch(&target(d::DEVICE_CAPS_EVENTS_SLOT)).unwrap();
        let ready=s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).signal(s::ServiceSignal::Ready).unwrap();
        if let Some((device,_))=claimed {
            BOOT.install(io,process,d::DEVICE_DRIVER_CAPS_DEVICE_SLOT,device,None,Rights::ALL);io.space.delete(device.as_ref()).unwrap();
            let wake=s::ProcessControlClient::new(&mut *io,Cap::from_ref(process.as_ref())).kernel_object(s::ProcessKernelObject::Notification).unwrap();
            BOOT.install(io,process,d::DEVICE_DRIVER_CAPS_WAKE_SLOT,wake,None,Rights::READ);
            BOOT.install(io,process,d::DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT,wake,Some(IRQ),Rights::WRITE);
            BOOT.install(io,process,d::DEVICE_DRIVER_CAPS_READY_SLOT,ready,None,Rights::WRITE);
            let stop=mint(io,sel4::cap::CNode::from_bits(d::DEVICE_CAPS_SELF_SLOT as _),Cap::<Notification>::from_ref(wake.as_ref()),STOP,Rights::WRITE);
            s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).stop_signal(&stop).unwrap();io.space.delete(stop.as_ref()).unwrap();io.space.delete(wake.as_ref()).unwrap();
            let stopped=s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).signal(s::ServiceSignal::Stopped).unwrap();
            BOOT.install(io,process,d::DEVICE_DRIVER_CAPS_STOPPED_SLOT,stopped,None,Rights::WRITE);io.space.delete(stopped.as_ref()).unwrap();
            let runtime=s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).client().unwrap();
            BOOT.install(io,process,d::DEVICE_DRIVER_CAPS_SUPERVISOR_SLOT,runtime,None,Rights(Rights::WRITE.0|Rights::GRANT_REPLY.0));io.space.delete(runtime.as_ref()).unwrap();
        }else{
            let observer=mint(io,sel4::cap::CNode::from_bits(d::DEVICE_CAPS_SELF_SLOT as _),target::<Endpoint>(d::DEVICE_CAPS_SERVICE_SLOT),horn_devmgr::OBSERVER_BADGE,Rights(Rights::WRITE.0|Rights::GRANT_REPLY.0));
            BOOT.install(io,process,d::DEVICE_PROBE_CAPS_OBSERVER_SLOT,observer,None,Rights(Rights::WRITE.0|Rights::GRANT_REPLY.0));io.space.delete(observer.as_ref()).unwrap();
            BOOT.install(io,process,d::DEVICE_PROBE_CAPS_READY_SLOT,ready,None,Rights::WRITE);
        }
        io.space.delete(ready.as_ref()).unwrap();
        s::SupervisorJobClient::new(&mut *io,Cap::from_ref(managed.as_ref())).start().unwrap();
        self.records.push(Managed{job:managed,process,device:claimed.map(|(_,badge)|badge),device_id:device_id.unwrap_or(0),ready:false,finished:false,probe:claimed.is_none()});
        sel4::debug_println!("DEVMGR_SERVICE_START name={} policy=devmgr",name);
    }
    pub fn faulted(&mut self,service:&mut Service,faults:crate::service::Faults){
        for record in &self.records {
            if record.finished || record.device.is_none(){continue;}
            let sid=(record.device_id-1) as usize;
            if faults.overflow || sid<256 && faults.requesters[sid/64]&(1u64<<(sid%64))!=0 {
                match job(service,record.job).request_stop(s::StopMode::Force) {
                    Ok(())|Err(CallError::Remote(s::SupervisorError::Busy))=>{},
                    error=>panic!("faulted driver stop: {:?}",error),
                }
            }
        }
    }
    pub fn changed(&mut self,service:&mut Service) {
        for record in &mut self.records {
            if record.finished{continue;}
            let state=job(service,record.job).status().unwrap();
            if state==s::ServiceStatus::Active && !record.ready {
                record.ready=true;
            }
            if state==s::ServiceStatus::AwaitingCleanup {
                let info=s::ProcessControlClient::new(&mut service.io().platform.transport,Cap::from_ref(record.process.as_ref())).info().unwrap();
                assert_eq!(info.state,s::ProcessState::Stopping);
                if record.probe {assert!(matches!(info.exit,Some(s::ProcessExit::Code(0))));}
                if let Some(badge)=record.device {
                    let orderly=job(service,record.job).cooperative().unwrap();
                    sel4::debug_println!("DEVMGR_DRIVER_REVOKE cpu_stopped=true orderly={}",orderly);
                    service.revoke_driver(badge).expect("device isolation/drain");
                    sel4::debug_println!("DEVMGR_DRIVER_REVOKE dma_drained=true irq_closed=true mmio_revoked=true");
                    assert!(service.claim_driver(record.device_id).is_err());
                    sel4::debug_println!("DEVMGR_ASSIGNMENT_PASS retired=true");
                }
                job(service,record.job).cleanup_done().unwrap();
                assert_eq!(job(service,record.job).status().unwrap(),s::ServiceStatus::Exited);
                service.io().space().delete(record.process.as_ref()).unwrap();service.io().space().delete(record.job.as_ref()).unwrap();record.finished=true;
            }
        }
        if !self.reported && self.records.iter().all(|r|if r.probe{r.finished}else{r.ready||r.finished}) {
            self.reported=true;
            sel4::debug_println!("DEVMGR_SERVICE_READY configured=true");
            if option_env!("DEVMGR_TEST_NO_READY")==Some("1"){sel4::debug_println!("DEVMGR_READY_WITHHELD test=true");}
            else {horn_runtime::notify_ready(d::DEVICE_CAPS_READY_SLOT);}
            if option_env!("DEVMGR_TEST_FAULT")==Some("1"){unsafe{core::arch::asm!("ud2",options(noreturn));}}
        }
    }
}
