#![no_std]
#![no_main]
use horn_devmgr::protocol::*;
use horn_ipc::*;
use horn_supervisor::protocol::ProcessLifecycleClient as _;
horn_runtime::task!(main);
fn main(mut ctx: horn_runtime::Context) -> ! {
    let mut io = ctx.transport().unwrap();
    let root = io.space.root;
    let forged = io.space.reserve_slot().unwrap();
    assert!(
        forged
            .mint(
                &root.absolute_cptr(sel4::CPtr::from_bits(
                    ctx.cap::<Endpoint>("devmgr.observer")
                        .unwrap()
                        .as_ref()
                        .slot as _
                )),
                sel4::CapRights::all(),
                horn_devmgr::ASSIGNMENT_BADGE
            )
            .is_err()
    );
    io.space
        .delete(CapRef::new(
            root.bits() as _,
            forged.path().bits() as _,
            sel4::WORD_SIZE as _,
            0,
            CapMode::MOVE,
            Rights::ALL,
        ))
        .unwrap();
    assert!(matches!(
        ctx.interface::<DeviceAssignment>("devmgr.observer")
            .unwrap()
            .claim(&mut io, 33),
        Err(CallError::Remote(DeviceError::Denied))
    ));
    sel4::debug_println!("DEVMGR_AUTH_PASS observer_rebadge=false");
    let client = ctx.interface::<DeviceObserver>("devmgr.observer").unwrap();
    let status = client.status(&mut io).unwrap();
    assert_eq!(status.iommu, IommuState::NotInitialized);
    assert!(status.pci_devices >= 5);
    let mut cursor = 0;
    let mut count = 0;
    let mut virtio = 0;
    loop {
        let page = client.list(&mut io, cursor).unwrap();
        for entry in &page.entries.data[..page.entries.len as usize] {
            assert!(entry.id > cursor);
            let info = client.info(&mut io, entry.id).unwrap();
            assert_eq!(info.device, *entry);
            if entry.vendor_id == 0x1af4 {
                virtio += 1;
                assert!(info.msix || info.msi);
            }
            count += 1;
        }
        if page.next == 0 {
            break;
        }
        assert!(page.next > cursor);
        cursor = page.next;
    }
    assert_eq!(count, status.pci_devices);
    assert!(virtio >= 2);
    assert!(matches!(
        client.info(&mut io, 0),
        Err(CallError::Remote(DeviceError::NotFound))
    ));
    assert!(matches!(
        client.list(&mut io, u64::MAX),
        Err(CallError::Remote(DeviceError::Invalid))
    ));
    sel4::debug_println!(
        "DEVMGR_RPC_PASS ecam=true pagination=true info=true msi=true readonly=true"
    );
    ctx.notify_ready().unwrap();
    let _ = ctx
        .interface::<horn_supervisor::protocol::ProcessLifecycle>("horn.lifecycle")
        .unwrap()
        .exit(&mut io, 0);
    panic!("exit returned");
}
