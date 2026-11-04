use alloc::vec::Vec;
use core::{
    alloc::{Allocator, Layout},
    ptr::NonNull,
};
use horn_ipc::*;
use horn_mimalloc::Mimalloc;
use horn_vm::protocol::*;

fn batch(values: &[VmUpdate]) -> Parcel<List<VmUpdate, 16>> {
    let mut list = List::default();
    list.len = values.len() as u32;
    list.data[..values.len()].copy_from_slice(values);
    Parcel::inline(list)
}
fn reserve(
    transport: &mut Sel4Transport<Sel4CapSpace>,
    space: Cap<VmAddressSpaceTag>,
    size: u64,
) -> OwnedCap<VmRegionObjectTag> {
    VmAddressSpace::from_ref(space.as_ref())
        .reserve(
            transport,
            VmRegionRequest {
                address: 0,
                length: size,
                alignment: 4096,
                placement: VmPlacement::Anywhere,
                ceiling: VmProtection(VmProtection::READ.0 | VmProtection::WRITE.0),
            },
        )
        .unwrap()
}
fn destroy(transport: &mut Sel4Transport<Sel4CapSpace>, region: OwnedCap<VmRegionObjectTag>) {
    region.destroy(&mut *transport).unwrap();
    drop(region);
}

pub fn run(ctx: &mut horn_runtime::Context, mut transport: &mut Sel4Transport<Sel4CapSpace>) {
    let root = transport.space.root;
    let factory = ctx.cap::<VmFactoryTag>("horn.vm").unwrap();
    let address_space = ctx.cap::<VmAddressSpaceTag>("horn.address-space").unwrap();
    let initial_charge = VmFactory::from_ref(factory.as_ref())
        .info(&mut transport)
        .unwrap()
        .charged_bytes;
    // The first synchronous call also orders this marker after root's marker.
    ::sel4::debug_println!("VM_CLIENT_CPU id={}", unsafe {
        core::arch::x86_64::__cpuid(1).ebx >> 24
    });
    // A whole Untyped is charged once, independent of the objects retyped inside it.
    let tcb_bits = ::sel4::ObjectBlueprint::Tcb.physical_size_bits() as u32;
    let lease = VmFactory::from_ref(factory.as_ref())
        .delegate(&mut transport, tcb_bits)
        .unwrap();
    let grant = lease.capability(&mut transport).unwrap();
    assert_eq!(grant.size_bits, tcb_bits);
    let untyped = grant.capability;
    let kernel_untyped = transport
        .space
        .cap::<::sel4::cap_type::Untyped>(untyped.cap_ref())
        .unwrap();
    let slot = transport.space.reserve_slot().unwrap();
    kernel_untyped
        .untyped_retype(
            &::sel4::ObjectBlueprint::Tcb,
            &root.absolute_cptr_for_self(),
            slot.path().bits() as usize,
            1,
        )
        .unwrap();
    assert_eq!(
        VmFactory::from_ref(factory.as_ref())
            .info(&mut transport,)
            .unwrap()
            .charged_bytes,
        initial_charge + (1u64 << tcb_bits)
    );
    lease.close(&mut transport).unwrap();
    let derived = transport
        .space
        .register(::sel4::cap::Tcb::from_bits(slot.path().bits()))
        .unwrap();
    transport.space.delete(derived).unwrap();
    drop(untyped);
    drop(lease);
    assert_eq!(
        VmFactory::from_ref(factory.as_ref())
            .info(&mut transport,)
            .unwrap()
            .charged_bytes,
        initial_charge
    );
    let rw = VmProtection(VmProtection::READ.0 | VmProtection::WRITE.0);
    let executable = VmFactory::from_ref(factory.as_ref())
        .allocate(
            &mut transport,
            4096,
            VmBacking::Resident,
            VmProtection(VmProtection::READ.0 | VmProtection::EXECUTE.0),
        )
        .unwrap();
    executable.close(&mut transport).unwrap();
    let range = VmRange {
        offset: 0,
        length: 8192,
    };
    assert_eq!(
        VmFactory::from_ref(factory.as_ref()).allocate(&mut transport, 0, VmBacking::Resident, rw),
        Err(CallError::Remote(VmError::Invalid))
    );
    assert_eq!(
        VmFactory::from_ref(factory.as_ref()).allocate(
            &mut transport,
            4096,
            VmBacking::DemandZero,
            rw
        ),
        Err(CallError::Remote(VmError::Unsupported))
    );
    assert_eq!(
        VmFactory::from_ref(factory.as_ref()).create_address_space(&mut transport,),
        Err(CallError::Remote(VmError::Unsupported))
    );
    assert_eq!(
        VmFactory::from_ref(address_space.as_ref()).info(&mut transport,),
        Err(CallError::Remote(VmError::Denied))
    );
    let memory = VmFactory::from_ref(factory.as_ref())
        .allocate(&mut transport, 8192, VmBacking::Resident, rw)
        .unwrap();
    let first = reserve(&mut transport, address_space, 8192);
    let alias = reserve(&mut transport, address_space, 8192);
    let first_info = first.info(&mut transport).unwrap();
    let alias_info = alias.info(&mut transport).unwrap();
    let mapping = VmMap {
        offset: 0,
        length: 8192,
        memory_offset: 0,
        protection: rw,
    };
    let update = VmUpdate::Map(mapping);
    for region in [&first, &alias] {
        region
            .apply(
                &mut transport,
                Some(Cap::from_ref(memory.cap_ref())),
                batch(&[VmUpdate::Prepare(range), update]),
            )
            .unwrap();
    }
    unsafe {
        let bytes = core::slice::from_raw_parts_mut(first_info.address as *mut u8, 8192);
        assert!(bytes.iter().all(|byte| *byte == 0));
        bytes.fill(0xa5);
        let alias = core::slice::from_raw_parts(alias_info.address as *const u8, 8192);
        assert!(alias.iter().all(|byte| *byte == 0xa5));
    }
    assert_eq!(
        memory.make_immutable(&mut transport),
        Err(CallError::Remote(VmError::Busy))
    );
    assert!(!memory.info(&mut transport).unwrap().immutable);
    // Copies alias one handle and observe the same backing policy.
    let copied_slot = transport.space.reserve_slot().unwrap();
    let endpoint = transport
        .space
        .cap::<::sel4::cap_type::Endpoint>(memory.cap_ref())
        .unwrap();
    copied_slot
        .copy(
            &root.absolute_cptr(endpoint.cptr()),
            ::sel4::CapRights::all(),
        )
        .unwrap();
    let copied = transport
        .space
        .register(::sel4::cap::Endpoint::from_bits(copied_slot.path().bits()))
        .unwrap();
    assert!(
        !VmMemory::from_ref(copied)
            .info(&mut transport)
            .unwrap()
            .immutable
    );
    let fake = ctx.cap::<VmMemoryObjectTag>("wake").unwrap();
    assert_eq!(
        alias.apply(&mut transport, Some(fake), batch(&[])),
        Err(CallError::Remote(VmBatchError {
            completed: 0,
            error: VmError::Denied
        }))
    );
    // Protect(READ) retains the ability to restore writes, so freeze stays Busy.
    for region in [&first, &alias] {
        region
            .apply(
                &mut transport,
                None,
                batch(&[VmUpdate::Protect(VmProtect {
                    offset: 0,
                    length: 8192,
                    protection: VmProtection::READ,
                })]),
            )
            .unwrap();
    }
    assert_eq!(
        memory.make_immutable(&mut transport),
        Err(CallError::Remote(VmError::Busy))
    );
    destroy(&mut transport, first);
    assert_eq!(
        VmMemory::from_ref(copied).make_immutable(&mut transport),
        Err(CallError::Remote(VmError::Busy))
    );
    destroy(&mut transport, alias);
    VmMemory::from_ref(copied)
        .make_immutable(&mut transport)
        .unwrap();
    memory.make_immutable(&mut transport).unwrap(); // Idempotent, no unfreeze API.
    assert!(memory.info(&mut transport).unwrap().immutable);
    let first = reserve(&mut transport, address_space, 8192);
    assert_eq!(
        first.apply(
            &mut transport,
            Some(Cap::from_ref(memory.cap_ref())),
            batch(&[VmUpdate::Prepare(range), update])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 1,
            error: VmError::Denied
        }))
    );
    first
        .apply(
            &mut transport,
            Some(Cap::from_ref(memory.cap_ref())),
            batch(&[VmUpdate::Map(VmMap {
                protection: VmProtection::READ,
                ..mapping
            })]),
        )
        .unwrap();
    assert_eq!(
        first.apply(
            &mut transport,
            None,
            batch(&[VmUpdate::Protect(VmProtect {
                offset: 0,
                length: 8192,
                protection: rw
            })])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 0,
            error: VmError::Denied
        }))
    );
    memory.close(&mut transport).unwrap();
    drop(memory);
    transport.space.delete(copied).unwrap();
    let address = first.info(&mut transport).unwrap().address;
    unsafe {
        assert_eq!(core::ptr::read_volatile(address as *const u8), 0xa5);
    }
    destroy(&mut transport, first);
    assert_eq!(
        VmFactory::from_ref(factory.as_ref())
            .info(&mut transport)
            .unwrap()
            .charged_bytes,
        initial_charge
    );
    ::sel4::debug_println!(
        "VM_IMMUTABLE_PASS aliases=true maywrite=true immutable=true cleanup=true"
    );
    let revoked = VmFactory::from_ref(factory.as_ref())
        .allocate(&mut transport, 4096, VmBacking::Resident, rw)
        .unwrap();
    let region = reserve(&mut transport, address_space, 4096);
    region
        .apply(
            &mut transport,
            Some(Cap::from_ref(revoked.cap_ref())),
            batch(&[
                VmUpdate::Prepare(VmRange {
                    offset: 0,
                    length: 4096,
                }),
                VmUpdate::Map(VmMap {
                    offset: 0,
                    length: 4096,
                    memory_offset: 0,
                    protection: rw,
                }),
            ]),
        )
        .unwrap();
    revoked.revoke(&mut transport).unwrap();
    drop(revoked);
    destroy(&mut transport, region);
    assert_eq!(
        VmFactory::from_ref(factory.as_ref())
            .info(&mut transport,)
            .unwrap()
            .charged_bytes,
        initial_charge
    );
    ::sel4::debug_println!(
        "VM_RPC_PASS mapping=true sharing=true lifetime=true protection=true errors=true"
    );

    // Leave one lease live: process exit, not client cooperation, must reclaim it.
    let _unclosed = VmFactory::from_ref(factory.as_ref())
        .delegate(&mut transport, 12)
        .unwrap();
    let mut bootstrap = Vec::with_capacity(4);
    bootstrap.extend_from_slice(&[11u8, 22, 33, 44]);
    assert!(horn_runtime::is_control(bootstrap.as_ptr()));
    let bootstrap_box = alloc::boxed::Box::new(0x1234_5678u64);
    assert!(horn_runtime::is_control(
        (&*bootstrap_box as *const u64).cast()
    ));
    let mut heap = ctx.heap().unwrap();
    // Recycled physical backing must not disclose the previous dirty contents.
    let pointer = heap.allocate(8192, 4096).unwrap();
    unsafe {
        assert!(
            core::slice::from_raw_parts(pointer.as_ptr(), 8192)
                .iter()
                .all(|byte| *byte == 0)
        );
    }
    heap.free(pointer.as_ptr() as usize, 8192).unwrap();
    unsafe {
        horn_mimalloc::install(heap).unwrap();
    }
    // An ordinary Vec reallocates from bootstrap storage into mimalloc. Its
    // old control allocation must be freed by provenance, not the new mode.
    bootstrap.reserve_exact(8192);
    assert_eq!(&bootstrap[..], &[11, 22, 33, 44]);
    assert!(!horn_runtime::is_control(bootstrap.as_ptr()));
    assert_eq!(*bootstrap_box, 0x1234_5678);
    drop(bootstrap_box); // Still owned by the control heap after installation.
    drop(bootstrap);
    horn_mimalloc::collect();
    let baseline = horn_mimalloc::info().unwrap();
    let peak;
    {
        let mut values = Vec::with_capacity_in(128 * 1024, Mimalloc);
        for value in 0..128 * 1024 {
            values.push(value as u64 ^ 0x1234_5678);
        }
        for (index, value) in values.iter().enumerate() {
            assert_eq!(*value, index as u64 ^ 0x1234_5678);
        }
        let layout = Layout::from_size_align(8192, 65536).unwrap();
        let aligned = Mimalloc.allocate(layout).unwrap();
        assert_eq!(aligned.as_ptr() as *mut u8 as usize % 65536, 0);
        unsafe {
            Mimalloc.deallocate(NonNull::new(aligned.as_ptr() as *mut u8).unwrap(), layout);
        }
        let mut impossible = Vec::<u8, _>::new_in(Mimalloc);
        assert!(
            impossible
                .try_reserve_exact(baseline.limit_bytes as usize * 2)
                .is_err()
        );
        peak = horn_mimalloc::info().unwrap().charged_bytes;
        assert!(peak > baseline.charged_bytes);
    }
    horn_mimalloc::collect();
    let collected = horn_mimalloc::info().unwrap();
    assert!(collected.charged_bytes < peak);
    assert_eq!(collected.quarantined_bytes, 0);
    // Allocator metadata may remain resident; collection must leave it usable.
    let mut small = Vec::new_in(Mimalloc);
    small.resize(64, 42u8);
    assert!(small.iter().all(|byte| *byte == 42));
    ::sel4::debug_println!(
        "MIMALLOC_VM_PASS allocation=true alignment=true exhaustion=true pages_returned=true"
    );
    let before_default = horn_mimalloc::info().unwrap();
    let default_peak;
    {
        let mut values = alloc::vec![0u64; 256 * 1024];
        assert!(values.iter().all(|v| *v == 0));
        assert!(!horn_runtime::is_control(values.as_ptr().cast()));
        for (index, value) in values.iter_mut().enumerate() {
            *value = index as u64;
        }
        values.reserve_exact(256 * 1024);
        assert!(values.iter().enumerate().all(|(i, v)| *v == i as u64));
        let boxed = alloc::boxed::Box::new([42u8; 8192]);
        assert!(!horn_runtime::is_control(boxed.as_ptr()));
        assert!(boxed.iter().all(|v| *v == 42));
        let layout = Layout::from_size_align(8192, 65536).unwrap();
        let aligned = unsafe { alloc::alloc::alloc_zeroed(layout) };
        assert!(!aligned.is_null());
        assert_eq!(aligned as usize % 65536, 0);
        assert!(!horn_runtime::is_control(aligned));
        unsafe {
            assert!(
                core::slice::from_raw_parts(aligned, layout.size())
                    .iter()
                    .all(|v| *v == 0)
            );
            alloc::alloc::dealloc(aligned, layout);
        }
        let mut impossible = alloc::vec![7u8; 128];
        let original = impossible.as_ptr();
        assert!(
            impossible
                .try_reserve_exact(before_default.limit_bytes as usize * 2)
                .is_err()
        );
        assert_eq!(impossible.as_ptr(), original);
        assert_eq!(impossible.len(), 128);
        assert!(impossible.iter().all(|value| *value == 7));
        default_peak = horn_mimalloc::info().unwrap().charged_bytes;
        assert!(default_peak > before_default.charged_bytes);
    }
    horn_mimalloc::collect();
    assert!(horn_mimalloc::info().unwrap().charged_bytes < default_peak);
    // Control storage remains reusable; there is no post-install dlmalloc leak
    // from moving or freeing the bootstrap objects above.
    ::sel4::debug_println!(
        "MIMALLOC_GLOBAL_PASS vec=true box=true realloc=true zeroed=true alignment=true bootstrap_free=true exhaustion=true pages_returned=true"
    );
}
