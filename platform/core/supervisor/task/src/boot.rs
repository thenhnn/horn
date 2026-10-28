//! Initial core-service bootstrap manifest. No driver catalogue or device policy.
//! Kept separate from the generic supervision loop; root boots only supervisor.
use super::*;
use horn_devmgr::protocol as d;
static CORE:&[u8]=include_bytes!(concat!(env!("OUT_DIR"),"/devmgr.elf"));
pub fn start(io:&mut Io) {
    let image=BOOT.upload(io,CORE);
    let mut request=ProcessLaunch::default();request.name.len=6;request.name.data[..6].copy_from_slice(b"devmgr");request.ram_limit=64<<20;
    let process=ProcessFactoryClient::new(&mut *io,cap(SUPERVISOR_CAPS_FACTORY_SLOT)).prepare(&image,request,ProcessBootstrap {
        vm:d::DEVICE_CAPS_VM_SLOT,address_space:d::DEVICE_CAPS_ADDRESS_SPACE_SLOT,lifecycle:d::DEVICE_CAPS_LIFECYCLE_SLOT,reply:d::DEVICE_CAPS_REPLY_SLOT}).unwrap();
    horn_supervisor::client::close_image(io,image);
    let service=ProcessControlClient::new(&mut *io,Cap::from_ref(process.as_ref())).kernel_object(ProcessKernelObject::Endpoint).unwrap();
    let note=ProcessControlClient::new(&mut *io,Cap::from_ref(process.as_ref())).kernel_object(ProcessKernelObject::Notification).unwrap();
    BOOT.install(io,process,d::DEVICE_CAPS_SERVICE_SLOT,service,None,Rights::ALL);
    BOOT.install(io,process,d::DEVICE_CAPS_PLATFORM_SLOT,cap::<Endpoint>(SUPERVISOR_CAPS_PLATFORM_SLOT),None,Rights::ALL);
    BOOT.install(io,process,d::DEVICE_CAPS_FAULT_SLOT,note,Some(horn_devmgr::FAULT_NOTIFICATION_BADGE),Rights::ALL);
    BOOT.install(io,process,d::DEVICE_CAPS_EVENTS_SLOT,note,Some(OWNER_CHANGED),Rights::WRITE);
    BOOT.install(io,process,d::DEVICE_CAPS_READY_SLOT,cap::<Notification>(SUPERVISOR_CAPS_EVENTS_SLOT),Some(CORE_READY),Rights::WRITE);
    let factory=mint(io,root(),cap::<Endpoint>(SUPERVISOR_CAPS_SERVICE_SLOT),FACTORY_BADGE,Rights::ALL);
    BOOT.install(io,process,d::DEVICE_CAPS_SUPERVISOR_SLOT,factory,None,Rights::ALL);
    io.space.delete(factory.as_ref()).unwrap();
    let mut control=ProcessControlClient::new(&mut *io,Cap::from_ref(process.as_ref()));
    control.bind_notification(&Cap::from_ref(note.as_ref())).unwrap();
    control.fault_handler(&cap(SUPERVISOR_CAPS_SELF_SLOT),SUPERVISOR_CAPS_SIZE_BITS as _,SUPERVISOR_CAPS_CRITICAL_FAULT_SLOT).unwrap();
    control.exit_signal(&cap(SUPERVISOR_CAPS_CRITICAL_EXIT_SLOT)).unwrap();
    if option_env!("SUPERVISOR_STALL")!=Some("1"){control.start().unwrap();}
    io.space.delete(service.as_ref()).unwrap();io.space.delete(note.as_ref()).unwrap();
    // The critical fault/exit path goes directly to root, independent of any
    // supervisor RPC wait. No ordinary job-control grant is exported for it.
    io.space.delete(process.as_ref()).unwrap();
}
