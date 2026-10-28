use crate::protocol::{CHILD_CAPS_ADDRESS_SPACE_SLOT, CHILD_CAPS_VM_SLOT};
use alloc::vec::Vec;
use core::{
    alloc::{Allocator, Layout},
    ptr::NonNull,
};
use horn_ipc::*;
use horn_mimalloc::{Mimalloc, VmHeap};
use horn_vm::protocol::*;

fn target<T: CapType>(root: ::sel4::cap::CNode, slot: u32) -> Cap<T> {
    Cap::new(
        root.bits() as u64,
        slot,
        ::sel4::WORD_SIZE as u8,
        0,
        CapMode::COPY,
        Rights::ALL,
    )
}
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
) -> Cap<VmRegionObjectTag> {
    VmAddressSpaceClient::new(transport, space)
        .reserve(VmRegionRequest {
            address: 0,
            length: size,
            alignment: 4096,
            placement: VmPlacement::Anywhere,
            ceiling: VmProtection(VmProtection::READ.0 | VmProtection::WRITE.0),
        })
        .unwrap()
}
fn destroy(transport: &mut Sel4Transport<Sel4CapSpace>, region: Cap<VmRegionObjectTag>) {
    VmRegionClient::new(&mut *transport, Cap::from_ref(region.as_ref()))
        .destroy()
        .unwrap();
    transport.space.delete(region.as_ref()).unwrap();
}

pub fn run(root: ::sel4::cap::CNode, recv: ::sel4::AbsoluteCPtr, space: Sel4CapSpace) {
    let mut transport = Sel4Transport::new(root, recv, space);
    let factory = target::<VmFactoryTag>(root, CHILD_CAPS_VM_SLOT);
    let address_space = target::<VmAddressSpaceTag>(root, CHILD_CAPS_ADDRESS_SPACE_SLOT);
    let initial_charge = VmFactoryClient::new(&mut transport, factory)
        .info()
        .unwrap()
        .charged_bytes;
    // The first synchronous call also orders this marker after root's marker.
    ::sel4::debug_println!("VM_CLIENT_CPU id={}", unsafe {
        core::arch::x86_64::__cpuid(1).ebx >> 24
    });
    // A whole Untyped is charged once, independent of the objects retyped inside it.
    let lease = VmFactoryClient::new(&mut transport, factory)
        .delegate(13)
        .unwrap();
    let grant = VmUntypedClient::new(&mut transport, Cap::from_ref(lease.as_ref()))
        .capability()
        .unwrap();
    assert_eq!(grant.size_bits, 13);
    let untyped = grant.capability;
    let kernel_untyped = transport
        .space
        .cap::<::sel4::cap_type::Untyped>(untyped.as_ref())
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
        VmFactoryClient::new(&mut transport, factory)
            .info()
            .unwrap()
            .charged_bytes,
        initial_charge + 8192
    );
    VmUntypedClient::new(&mut transport, Cap::from_ref(lease.as_ref()))
        .close()
        .unwrap();
    let derived = transport
        .space
        .register(::sel4::cap::Tcb::from_bits(slot.path().bits()))
        .unwrap();
    transport.space.delete(derived).unwrap();
    transport.space.delete(untyped.as_ref()).unwrap();
    transport.space.delete(lease.as_ref()).unwrap();
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory)
            .info()
            .unwrap()
            .charged_bytes,
        initial_charge
    );
    let rw = VmProtection(VmProtection::READ.0 | VmProtection::WRITE.0);
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory).allocate(
            4096,
            VmBacking::Resident,
            VmProtection(VmProtection::READ.0 | VmProtection::EXECUTE.0)
        ),
        Err(CallError::Remote(VmError::Denied))
    );
    let range = VmRange {
        offset: 0,
        length: 8192,
    };
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory).allocate(0, VmBacking::Resident, rw),
        Err(CallError::Remote(VmError::Invalid))
    );
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory).allocate(4096, VmBacking::DemandZero, rw),
        Err(CallError::Remote(VmError::Unsupported))
    );
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory).create_address_space(),
        Err(CallError::Remote(VmError::Unsupported))
    );
    assert_eq!(
        VmFactoryClient::new(&mut transport, Cap::from_ref(address_space.as_ref())).info(),
        Err(CallError::Remote(VmError::Denied))
    );
    let memory = VmFactoryClient::new(&mut transport, factory)
        .allocate(8192, VmBacking::Resident, rw)
        .unwrap();
    let first = reserve(&mut transport, address_space, 8192);
    let alias = reserve(&mut transport, address_space, 8192);
    let first_info = VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref()))
        .info()
        .unwrap();
    let alias_info = VmRegionClient::new(&mut transport, Cap::from_ref(alias.as_ref()))
        .info()
        .unwrap();
    let mapping = VmMap {
        offset: 0,
        length: 8192,
        memory_offset: 0,
        protection: rw,
    };
    let update = VmUpdate::Map(mapping);
    for region in [first, alias] {
        VmRegionClient::new(&mut transport, Cap::from_ref(region.as_ref()))
            .apply(Some(memory), batch(&[VmUpdate::Prepare(range), update]))
            .unwrap();
    }
    unsafe {
        let bytes = core::slice::from_raw_parts_mut(first_info.address as *mut u8, 8192);
        assert!(bytes.iter().all(|byte| *byte == 0));
        bytes.fill(0xa5);
        let alias = core::slice::from_raw_parts(alias_info.address as *const u8, 8192);
        assert!(alias.iter().all(|byte| *byte == 0xa5));
    }
    let write_and_lock = VmSeals(VmSeals::WRITE.0 | VmSeals::SEAL.0);
    assert_eq!(
        VmMemoryClient::new(&mut transport, Cap::from_ref(memory.as_ref()))
            .add_seals(write_and_lock),
        Err(CallError::Remote(VmError::Busy))
    );
    assert_eq!(
        VmMemoryClient::new(&mut transport, Cap::from_ref(memory.as_ref()))
            .info()
            .unwrap()
            .seals,
        VmSeals(0)
    );
    // A kernel-cap copy must see and change the same backing-wide seals.
    let copied_slot = transport.space.reserve_slot().unwrap();
    let endpoint = transport
        .space
        .cap::<::sel4::cap_type::Endpoint>(memory.as_ref())
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
    VmMemoryClient::new(&mut transport, Cap::from_ref(copied))
        .add_seals(VmSeals::FUTURE_WRITE)
        .unwrap();
    unsafe {
        core::ptr::write_volatile(alias_info.address as *mut u8, 0xa5);
    }
    // A non-service capability must not be interpreted as a memory identity.
    let fake = target::<VmMemoryObjectTag>(root, crate::protocol::CHILD_CAPS_WAKE_SLOT);
    assert_eq!(
        VmRegionClient::new(&mut transport, Cap::from_ref(alias.as_ref()))
            .apply(Some(fake), batch(&[])),
        Err(CallError::Remote(VmBatchError {
            completed: 0,
            error: VmError::Denied
        }))
    );
    assert_eq!(
        VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref())).apply(
            None,
            batch(&[
                VmUpdate::Prepare(range),
                VmUpdate::Unmap(VmRange {
                    offset: u64::MAX - 4095,
                    length: 8192
                }),
            ])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 1,
            error: VmError::Invalid
        }))
    );
    destroy(&mut transport, first);
    // New mappings after FUTURE_WRITE cannot acquire write authority, even
    // with an RW region ceiling. Failed Map leaves the completed Prepare.
    let first = reserve(&mut transport, address_space, 8192);
    let readonly = VmMap {
        protection: VmProtection::READ,
        ..mapping
    };
    assert_eq!(
        VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref()))
            .apply(Some(memory), batch(&[VmUpdate::Prepare(range), update])),
        Err(CallError::Remote(VmBatchError {
            completed: 1,
            error: VmError::Denied
        }))
    );
    VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref()))
        .apply(Some(memory), batch(&[VmUpdate::Map(readonly)]))
        .unwrap();
    assert_eq!(
        VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref())).apply(
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
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory)
            .info()
            .unwrap()
            .charged_bytes,
        initial_charge + 8192
    );
    // Old mappings keep MAYWRITE across protect(READ), just like memfd.
    for protection in [VmProtection::READ, rw, VmProtection::READ] {
        VmRegionClient::new(&mut transport, Cap::from_ref(alias.as_ref()))
            .apply(
                None,
                batch(&[VmUpdate::Protect(VmProtect {
                    offset: 0,
                    length: 8192,
                    protection,
                })]),
            )
            .unwrap();
    }
    assert_eq!(
        VmMemoryClient::new(&mut transport, Cap::from_ref(memory.as_ref()))
            .add_seals(write_and_lock),
        Err(CallError::Remote(VmError::Busy))
    );
    assert_eq!(
        VmMemoryClient::new(&mut transport, Cap::from_ref(memory.as_ref()))
            .info()
            .unwrap()
            .seals,
        VmSeals::FUTURE_WRITE
    );
    unsafe {
        assert_eq!(
            core::ptr::read_volatile(alias_info.address as *const u8),
            0xa5
        );
    }
    assert_eq!(
        VmRegionClient::new(&mut transport, Cap::from_ref(alias.as_ref())).apply(
            None,
            batch(&[VmUpdate::Protect(VmProtect {
                offset: 0,
                length: 8192,
                protection: VmProtection(7)
            }),])
        ),
        Err(CallError::Remote(VmBatchError {
            completed: 0,
            error: VmError::Denied
        }))
    );
    destroy(&mut transport, alias);
    VmMemoryClient::new(&mut transport, Cap::from_ref(memory.as_ref()))
        .add_seals(VmSeals(
            write_and_lock.0 | VmSeals::GROW.0 | VmSeals::SHRINK.0,
        ))
        .unwrap();
    let sealed = VmMemoryClient::new(&mut transport, Cap::from_ref(copied))
        .info()
        .unwrap();
    assert_eq!(sealed.seals.0, 31);
    assert_eq!(sealed.size, 8192);
    assert_eq!(
        VmMemoryClient::new(&mut transport, Cap::from_ref(copied)).add_seals(VmSeals(0)),
        Err(CallError::Remote(VmError::Denied))
    );
    assert_eq!(
        VmMemoryClient::new(&mut transport, Cap::from_ref(copied)).add_seals(VmSeals(32)),
        Err(CallError::Remote(VmError::Invalid))
    );
    assert_eq!(
        VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref())).apply(
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
    VmMemoryClient::new(&mut transport, Cap::from_ref(memory.as_ref()))
        .close()
        .unwrap();
    transport.space.delete(memory.as_ref()).unwrap();
    transport.space.delete(copied).unwrap();
    let address = VmRegionClient::new(&mut transport, Cap::from_ref(first.as_ref()))
        .info()
        .unwrap()
        .address;
    unsafe {
        assert_eq!(core::ptr::read_volatile(address as *const u8), 0xa5);
    }
    destroy(&mut transport, first);
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory)
            .info()
            .unwrap()
            .charged_bytes,
        initial_charge
    );
    ::sel4::debug_println!(
        "VM_SEALS_PASS aliases=true future_write=true immutable=true cleanup=true"
    );
    let revoked = VmFactoryClient::new(&mut transport, factory)
        .allocate(4096, VmBacking::Resident, rw)
        .unwrap();
    let region = reserve(&mut transport, address_space, 4096);
    VmRegionClient::new(&mut transport, Cap::from_ref(region.as_ref()))
        .apply(
            Some(revoked),
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
    VmMemoryClient::new(&mut transport, Cap::from_ref(revoked.as_ref()))
        .revoke()
        .unwrap();
    transport.space.delete(revoked.as_ref()).unwrap();
    destroy(&mut transport, region);
    assert_eq!(
        VmFactoryClient::new(&mut transport, factory)
            .info()
            .unwrap()
            .charged_bytes,
        initial_charge
    );
    ::sel4::debug_println!(
        "VM_RPC_PASS mapping=true sharing=true lifetime=true protection=true errors=true"
    );

    // Leave one lease live: process exit, not client cooperation, must reclaim it.
    let _unclosed = VmFactoryClient::new(&mut transport, factory)
        .delegate(12)
        .unwrap();
    let mut bootstrap = Vec::with_capacity(4);
    bootstrap.extend_from_slice(&[11u8, 22, 33, 44]);
    assert!(crate::runtime::is_control(bootstrap.as_ptr()));
    let bootstrap_box = alloc::boxed::Box::new(0x1234_5678u64);
    assert!(crate::runtime::is_control(
        (&*bootstrap_box as *const u64).cast()
    ));
    let mut heap = VmHeap::new(transport, factory.as_ref(), address_space.as_ref()).unwrap();
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
    assert!(!crate::runtime::is_control(bootstrap.as_ptr()));
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
        assert!(!crate::runtime::is_control(values.as_ptr().cast()));
        for (index, value) in values.iter_mut().enumerate() {
            *value = index as u64;
        }
        values.reserve_exact(256 * 1024);
        assert!(values.iter().enumerate().all(|(i, v)| *v == i as u64));
        let boxed = alloc::boxed::Box::new([42u8; 8192]);
        assert!(!crate::runtime::is_control(boxed.as_ptr()));
        assert!(boxed.iter().all(|v| *v == 42));
        let layout = Layout::from_size_align(8192, 65536).unwrap();
        let aligned = unsafe { alloc::alloc::alloc_zeroed(layout) };
        assert!(!aligned.is_null());
        assert_eq!(aligned as usize % 65536, 0);
        assert!(!crate::runtime::is_control(aligned));
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
