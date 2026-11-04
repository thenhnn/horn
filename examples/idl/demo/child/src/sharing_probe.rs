//! Cross-process VM delegation and mapping-lifetime regression probe.
use horn_ipc::*;
use horn_supervisor::client::{Bootstrap, Io};
use horn_supervisor::protocol::ProcessObjectTag;
use horn_vm::protocol::*;

fn updates(values: &[VmUpdate]) -> Parcel<List<VmUpdate, 16>> {
    let mut list = List::default();
    list.len = values.len() as u32;
    list.data[..values.len()].copy_from_slice(values);
    Parcel::inline(list)
}
fn map(
    io: &mut Io,
    factory: Cap<VmServiceTag>,
    space: Cap<VmAddressSpaceObjectTag>,
    memory: Cap<VmMemoryObjectTag>,
    protection: VmProtection,
) -> (OwnedCap<VmRegionObjectTag>, usize) {
    let region = space
        .reserve(
            &mut *io,
            VmRegionRequest {
                address: 0,
                length: 4096,
                alignment: 4096,
                placement: VmPlacement::Anywhere,
                ceiling: VmProtection(3),
            },
        )
        .unwrap();
    let charge = factory.info(&mut *io).unwrap().charged_bytes;
    region
        .apply(
            &mut *io,
            Some(memory),
            updates(&[
                VmUpdate::Prepare(VmRange {
                    offset: 0,
                    length: 4096,
                }),
                VmUpdate::Map(VmMap {
                    offset: 0,
                    length: 4096,
                    memory_offset: 0,
                    protection,
                }),
            ]),
        )
        .unwrap();
    assert_eq!(factory.info(&mut *io).unwrap().charged_bytes, charge);
    let address = region.info(&mut *io).unwrap().address as usize;
    (region, address)
}

pub struct Probe {
    memory: OwnedCap<VmMemoryObjectTag>,
    region: OwnedCap<VmRegionObjectTag>,
    control: OwnedCap<VmMemoryObjectTag>,
    control_region: OwnedCap<VmRegionObjectTag>,
    status: usize,
    branch: OwnedCap<VmMemoryObjectTag>,
    sibling: OwnedCap<VmMemoryObjectTag>,
    factory: Cap<VmServiceTag>,
    initial: u64,
}
impl Probe {
    pub fn install(
        ctx: &horn_runtime::Context,
        io: &mut Io,
        boot: Bootstrap,
        child: &impl CapView<ProcessObjectTag>,
    ) -> Self {
        let factory = ctx.object::<VmServiceTag>("horn.vm").unwrap();
        let space = ctx
            .object::<VmAddressSpaceObjectTag>("horn.address-space")
            .unwrap();
        let initial = factory.info(&mut *io).unwrap().charged_bytes;
        let memory = factory
            .allocate(&mut *io, 4096, VmBacking::Resident, VmProtection(3))
            .unwrap();
        let (region, address) = map(
            io,
            factory,
            space,
            Cap::from_ref(memory.cap_ref()),
            VmProtection(3),
        );
        unsafe {
            core::ptr::write_volatile(address as *mut u32, 0xa5);
        }
        let branch = memory
            .delegate(
                &mut *io,
                VmMemoryRights(
                    VmMemoryRights::READ.0
                        | VmMemoryRights::DELEGATE.0
                        | VmMemoryRights::REVOKE.0
                        | VmMemoryRights::CLOSE.0,
                ),
            )
            .unwrap();
        assert_eq!(
            branch.delegate(&mut *io, VmMemoryRights(3)),
            Err(CallError::Remote(VmError::Denied))
        );
        assert_eq!(
            branch.delegate(&mut *io, VmMemoryRights(257)),
            Err(CallError::Remote(VmError::Denied))
        );
        let leaf = branch
            .delegate(
                &mut *io,
                VmMemoryRights(VmMemoryRights::READ.0 | VmMemoryRights::CLOSE.0),
            )
            .unwrap();
        let sibling = memory
            .delegate(&mut *io, VmMemoryRights(3 | VmMemoryRights::CLOSE.0))
            .unwrap();
        let control = factory
            .allocate(&mut *io, 4096, VmBacking::Resident, VmProtection(3))
            .unwrap();
        let (control_region, status) = map(
            io,
            factory,
            space,
            Cap::from_ref(control.cap_ref()),
            VmProtection(3),
        );
        let control_handle = control.delegate(&mut *io, VmMemoryRights(3)).unwrap();
        boot.install(io, child, 20, &leaf, None, Rights::ALL);
        boot.install(io, child, 21, &control_handle, None, Rights::ALL);
        boot.install(io, child, 22, &sibling, None, Rights::ALL);
        // Local cap deletion must not close remotely installed aliases.
        drop(leaf);
        drop(control_handle);
        assert_eq!(
            factory.info(&mut *io).unwrap().charged_bytes,
            initial + 8192
        );
        Self {
            memory,
            region,
            control,
            control_region,
            status,
            branch,
            sibling,
            factory,
            initial,
        }
    }
    fn wait(&self, io: &mut Io, value: u32) {
        for _ in 0..100000 {
            if unsafe { core::ptr::read_volatile(self.status as *const u32) } == value {
                return;
            }
            self.factory.info(&mut *io).unwrap();
        }
        panic!("sharing peer did not reach phase {}", value);
    }
    pub fn revoke_branch(&self, io: &mut Io) {
        self.wait(io, 1);
        self.region
            .apply(
                &mut *io,
                None,
                updates(&[VmUpdate::Unmap(VmRange {
                    offset: 0,
                    length: 4096,
                })]),
            )
            .unwrap();
        // Only the other process retains a writable mapping now.
        assert_eq!(
            self.memory.make_immutable(&mut *io),
            Err(CallError::Remote(VmError::Busy))
        );
        self.branch.revoke(&mut *io).unwrap();
        assert_eq!(
            self.memory.make_immutable(&mut *io),
            Err(CallError::Remote(VmError::Busy))
        );
        unsafe {
            core::ptr::write_volatile(self.status as *mut u32, 2);
        }
        self.wait(io, 3);
    }
    pub fn cleanup(self, io: &mut Io) {
        self.sibling.close(&mut *io).unwrap();
        self.memory.make_immutable(&mut *io).unwrap();
        self.memory.revoke(&mut *io).unwrap();
        self.control.revoke(&mut *io).unwrap();
        self.region.destroy(&mut *io).unwrap();
        self.control_region.destroy(&mut *io).unwrap();
        assert_eq!(
            self.factory.info(&mut *io).unwrap().charged_bytes,
            self.initial
        );
        sel4::debug_println!(
            "VM_CROSS_SPACE_PASS shared=true attenuation=true close=true subtree_revoke=true sibling=true cleanup=true"
        );
    }
}

pub fn receive(ctx: &horn_runtime::Context, io: &mut Io) {
    let space = ctx
        .object::<VmAddressSpaceObjectTag>("horn.address-space")
        .unwrap();
    let leaf = ctx.object::<VmMemoryObjectTag>("sharing.leaf").unwrap();
    let control = ctx.object::<VmMemoryObjectTag>("sharing.control").unwrap();
    let sibling = ctx.object::<VmMemoryObjectTag>("sharing.sibling").unwrap();
    let factory = ctx.object::<VmServiceTag>("horn.vm").unwrap();
    assert_eq!(
        leaf.rights(&mut *io).unwrap(),
        VmMemoryRights(VmMemoryRights::READ.0 | VmMemoryRights::CLOSE.0)
    );
    assert_eq!(
        control.close(&mut *io),
        Err(CallError::Remote(VmError::Denied))
    );
    assert_eq!(
        leaf.delegate(&mut *io, VmMemoryRights::READ),
        Err(CallError::Remote(VmError::Denied))
    );
    assert_eq!(
        leaf.make_immutable(&mut *io),
        Err(CallError::Remote(VmError::Denied))
    );
    assert_eq!(
        leaf.revoke(&mut *io),
        Err(CallError::Remote(VmError::Denied))
    );
    let denied_region = space
        .reserve(
            &mut *io,
            VmRegionRequest {
                address: 0,
                length: 4096,
                alignment: 4096,
                placement: VmPlacement::Anywhere,
                ceiling: VmProtection(3),
            },
        )
        .unwrap();
    assert_eq!(
        denied_region.apply(
            &mut *io,
            Some(leaf),
            updates(&[
                VmUpdate::Prepare(VmRange {
                    offset: 0,
                    length: 4096
                }),
                VmUpdate::Map(VmMap {
                    offset: 0,
                    length: 4096,
                    memory_offset: 0,
                    protection: VmProtection(3)
                }),
            ])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 1,
            error: VmError::Denied
        }))
    );
    denied_region.destroy(&mut *io).unwrap();
    drop(denied_region);
    let (region, address) = map(io, factory, space, leaf, VmProtection::READ);
    let (sibling_region, sibling_address) = map(io, factory, space, sibling, VmProtection(3));
    assert_eq!(
        unsafe { core::ptr::read_volatile(address as *const u32) },
        0xa5
    );
    assert_eq!(
        region.apply(
            &mut *io,
            None,
            updates(&[VmUpdate::Protect(VmProtect {
                offset: 0,
                length: 4096,
                protection: VmProtection(3)
            })])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 0,
            error: VmError::Denied
        }))
    );
    let (control_region, status) = map(io, factory, space, control, VmProtection(3));
    // Close the leaf before revoke: the page must retain its provenance.
    leaf.close(&mut *io).unwrap();
    assert_eq!(
        unsafe { core::ptr::read_volatile(address as *const u32) },
        0xa5
    );
    unsafe {
        core::ptr::write_volatile(status as *mut u32, 1);
    }
    let mut revoked = false;
    for _ in 0..100000 {
        if unsafe { core::ptr::read_volatile(status as *const u32) } == 2 {
            revoked = true;
            break;
        }
        factory.info(&mut *io).unwrap();
    }
    assert!(revoked);
    assert_eq!(
        region.apply(
            &mut *io,
            None,
            updates(&[VmUpdate::Protect(VmProtect {
                offset: 0,
                length: 4096,
                protection: VmProtection::READ
            })])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 0,
            error: VmError::Invalid
        }))
    );
    assert_eq!(
        unsafe { core::ptr::read_volatile(sibling_address as *const u32) },
        0xa5
    );
    region.destroy(&mut *io).unwrap();
    // Remaining foreign mappings are intentionally left for process-exit cleanup.
    drop(sibling_region);
    drop(control_region);
    unsafe {
        core::ptr::write_volatile(status as *mut u32, 3);
    }
}
