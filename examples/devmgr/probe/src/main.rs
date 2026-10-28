#![no_std]
#![no_main]
use horn_ipc::*;
use horn_devmgr::protocol::*;
const SLOTS:TaskSlots=TaskSlots::new(DEVICE_PROBE_CAPS_FIRST_FREE_SLOT,DEVICE_PROBE_CAPS_SIZE_BITS);
horn_runtime::task!(main,vm:horn_runtime::VmBootstrap{cnode:DEVICE_PROBE_CAPS_SELF_SLOT,factory:DEVICE_PROBE_CAPS_VM_SLOT,address_space:DEVICE_PROBE_CAPS_ADDRESS_SPACE_SLOT,slots:SLOTS});
fn cap<T:CapType>(slot:u32)->Cap<T>{horn_runtime::bootstrap_cap(sel4::cap::CNode::from_bits(DEVICE_PROBE_CAPS_SELF_SLOT as _),slot)}
fn main()->! {
    let root=sel4::cap::CNode::from_bits(DEVICE_PROBE_CAPS_SELF_SLOT as _);
    let mut io=horn_runtime::transport(root,SLOTS.application()).unwrap();
    let forged=io.space.reserve_slot().unwrap();
    assert!(forged.mint(&root.absolute_cptr(sel4::CPtr::from_bits(DEVICE_PROBE_CAPS_OBSERVER_SLOT as _)),sel4::CapRights::all(),horn_devmgr::ASSIGNMENT_BADGE).is_err());
    io.space.delete(CapRef::new(root.bits() as _,forged.path().bits() as _,sel4::WORD_SIZE as _,0,CapMode::MOVE,Rights::ALL)).unwrap();
    assert!(matches!(DeviceAssignmentClient::new(&mut io,cap(DEVICE_PROBE_CAPS_OBSERVER_SLOT)).claim(33),Err(CallError::Remote(DeviceError::Denied))));
    sel4::debug_println!("DEVMGR_AUTH_PASS observer_rebadge=false");
    let mut client=DeviceObserverClient::new(&mut io,cap(DEVICE_PROBE_CAPS_OBSERVER_SLOT));
    let status=client.status().unwrap();assert_eq!(status.iommu,IommuState::NotInitialized);assert!(status.pci_devices>=5);
    let mut cursor=0;let mut count=0;let mut virtio=0;
    loop {
        let page=client.list(cursor).unwrap();
        for entry in &page.entries.data[..page.entries.len as usize] {
            assert!(entry.id>cursor);let info=client.info(entry.id).unwrap();assert_eq!(info.device,*entry);
            if entry.vendor_id==0x1af4{virtio+=1;assert!(info.msix||info.msi);}count+=1;
        }
        if page.next==0{break;}assert!(page.next>cursor);cursor=page.next;
    }
    assert_eq!(count,status.pci_devices);assert!(virtio>=2);
    assert!(matches!(client.info(0),Err(CallError::Remote(DeviceError::NotFound))));
    assert!(matches!(client.list(u64::MAX),Err(CallError::Remote(DeviceError::Invalid))));
    sel4::debug_println!("DEVMGR_RPC_PASS ecam=true pagination=true info=true msi=true readonly=true");
    horn_runtime::notify_ready(DEVICE_PROBE_CAPS_READY_SLOT);
    let _=horn_supervisor::protocol::ProcessLifecycleClient::new(&mut io,cap(DEVICE_PROBE_CAPS_LIFECYCLE_SLOT)).exit(0);
    panic!("exit returned");
}
