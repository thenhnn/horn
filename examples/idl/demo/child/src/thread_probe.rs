//! Generic empty containers, cross-VSpace RAM, and independent native TCBs.
use horn_ipc::*;
use horn_procd::protocol::{
    self as p, ExecutionContextClient as _, ProcessControlClient as _, ProcessFactoryClient as _,
    ProcessThreadsClient as _, ThreadControlClient as _, ThreadObserverClient as _,
};
use horn_vm::protocol::{
    self as v, VmAddressSpaceClient as _, VmFactoryClient as _, VmMemoryClient as _,
    VmRegionClient as _,
};
type Io = Sel4Transport<Sel4CapSpace>;
fn batch(values: &[v::VmUpdate]) -> Parcel<List<v::VmUpdate, 16>> {
    let mut list = List::default();
    list.len = values.len() as u32;
    list.data[..values.len()].copy_from_slice(values);
    Parcel::inline(list)
}
fn map(
    io: &mut Io,
    space: Cap<v::VmAddressSpaceObjectTag>,
    memory: Cap<v::VmMemoryObjectTag>,
    protection: v::VmProtection,
) -> (OwnedCap<v::VmRegionObjectTag>, usize) {
    let region = space
        .reserve(
            &mut *io,
            v::VmRegionRequest {
                address: 0,
                length: 4096,
                alignment: 4096,
                placement: v::VmPlacement::Anywhere,
                ceiling: v::VmProtection(7),
            },
        )
        .unwrap();
    region
        .apply(
            &mut *io,
            Some(memory),
            batch(&[
                v::VmUpdate::Prepare(v::VmRange {
                    offset: 0,
                    length: 4096,
                }),
                v::VmUpdate::Map(v::VmMap {
                    offset: 0,
                    length: 4096,
                    memory_offset: 0,
                    protection,
                }),
            ]),
        )
        .unwrap();
    let address = region.info(&mut *io).unwrap().address as usize;
    (region, address)
}
fn registers(ip: usize, data: usize, fs: usize, mxcsr: usize) -> p::ExecutionRegisters {
    p::ExecutionRegisters {
        ip: ip as _,
        sp: (data + 4096) as _,
        flags: 0x202,
        ax: 0,
        bx: 0,
        cx: 0,
        dx: 0,
        si: mxcsr as _,
        di: data as _,
        bp: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        fs: fs as _,
        gs: 0,
    }
}
pub fn run(ctx: &horn_runtime::Context, io: &mut Io) {
    let factory = ctx
        .object::<p::ProcessFactoryObjectTag>("process_factory")
        .unwrap();
    let vm = ctx.object::<v::VmServiceTag>("horn.vm").unwrap();
    let local = ctx
        .object::<v::VmAddressSpaceObjectTag>("horn.address-space")
        .unwrap();
    let initial = vm.info(&mut *io).unwrap().charged_bytes;
    let mut name = List::default();
    name.len = 7;
    name.data[..7].copy_from_slice(b"threads");
    let child = factory
        .spawn_empty(
            &mut *io,
            p::ProcessEmpty {
                launch: p::ProcessLaunch {
                    name,
                    ram_limit: 8 << 20,
                },
                address: 0x1000_0000,
                length: 2 << 20,
            },
        )
        .unwrap();
    assert_eq!(child.list_threads(&mut *io, None).unwrap().entries.len, 0);
    assert_eq!(
        child.start(&mut *io),
        Err(CallError::Remote(p::ProcessError::State))
    );
    let view = child.observe(&mut *io).unwrap();
    assert_eq!(
        p::ProcessThreads::from_ref(view.cap_ref()).execution_context(&mut *io),
        Err(CallError::Remote(p::ProcessError::Denied))
    );
    let space = child.address_space(&mut *io).unwrap();
    let code = vm
        .allocate(&mut *io, 4096, v::VmBacking::Resident, v::VmProtection(7))
        .unwrap();
    let data = vm
        .allocate(&mut *io, 4096, v::VmBacking::Resident, v::VmProtection(3))
        .unwrap();
    let (writer, code_address) = map(io, local, Cap::from_ref(code.cap_ref()), v::VmProtection(3));
    let (data_writer, data_address) =
        map(io, local, Cap::from_ref(data.cap_ref()), v::VmProtection(3));
    // ldmxcsr [rsi]; mov rax,fs:[0]; add [rdi],rax; ud2.
    let native: &[u8] = &[
        0x0f, 0xae, 0x16, 0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x48, 0x01, 0x07, 0x0f, 0x0b,
    ];
    // Same work, then a foreign syscall with Linux-style RAX=123.
    let trapped: &[u8] = &[
        0x0f, 0xae, 0x16, 0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x48, 0x01, 0x07, 0xb8, 123, 0,
        0, 0, 0x0f, 0x05, 0x0f, 0x0b,
    ];
    unsafe {
        core::ptr::copy_nonoverlapping(native.as_ptr(), code_address as *mut u8, native.len());
        core::ptr::copy_nonoverlapping(
            trapped.as_ptr(),
            (code_address + 64) as *mut u8,
            trapped.len(),
        );
        core::ptr::write_volatile((data_address + 64) as *mut u64, 42);
        core::ptr::write_volatile((data_address + 72) as *mut u64, 99);
        core::ptr::write_volatile((data_address + 80) as *mut u32, 0x1f80);
        core::ptr::write_volatile((data_address + 84) as *mut u32, 0x3f80);
    }
    // Deliberately retain the RW alias: RX does not imply immutability.
    let (code_region, executable) = map(
        io,
        Cap::from_ref(space.cap_ref()),
        Cap::from_ref(code.cap_ref()),
        v::VmProtection(5),
    );
    let (data_region, shared) = map(
        io,
        Cap::from_ref(space.cap_ref()),
        Cap::from_ref(data.cap_ref()),
        v::VmProtection(3),
    );
    // A maximum RWX ceiling allows RW -> RX changes, never active RWX.
    code_region
        .apply(
            &mut *io,
            None,
            batch(&[
                v::VmUpdate::Protect(v::VmProtect {
                    offset: 0,
                    length: 4096,
                    protection: v::VmProtection(3),
                }),
                v::VmUpdate::Protect(v::VmProtect {
                    offset: 0,
                    length: 4096,
                    protection: v::VmProtection(5),
                }),
            ]),
        )
        .unwrap();
    assert_eq!(
        code_region.apply(
            &mut *io,
            None,
            batch(&[v::VmUpdate::Protect(v::VmProtect {
                offset: 0,
                length: 4096,
                protection: v::VmProtection(7)
            }),])
        ),
        Err(CallError::Remote(v::VmBatchError {
            completed: 0,
            error: v::VmError::Denied
        }))
    );
    let wake = child
        .kernel_object(&mut *io, p::ProcessKernelObject::Notification)
        .unwrap();
    let notification = Cap::<Notification>::from_ref(wake.cap_ref());
    let context = child.execution_context(&mut *io).unwrap();
    let a = child
        .create_thread(
            &mut *io,
            p::ThreadCreate {
                abi: p::ThreadAbi::Native,
                allocate_ipc_buffer: false,
                registers: registers(executable, shared, shared + 64, shared + 80),
            },
        )
        .unwrap();
    let b = child
        .create_thread(
            &mut *io,
            p::ThreadCreate {
                abi: p::ThreadAbi::TrappedSyscalls,
                allocate_ipc_buffer: false,
                registers: registers(executable + 64, shared, shared + 72, shared + 84),
            },
        )
        .unwrap();
    assert_eq!(a.ipc_buffer(&mut *io).unwrap(), None);
    assert_eq!(b.ipc_buffer(&mut *io).unwrap(), None);
    let observer = a.observe(&mut *io).unwrap();
    assert_eq!(
        p::ThreadControl::from_ref(observer.cap_ref()).registers(&mut *io),
        Err(CallError::Remote(p::ProcessError::Denied))
    );
    assert_eq!(
        context.run(
            &mut *io,
            &Cap::<p::ThreadObjectTag>::from_ref(observer.cap_ref())
        ),
        Err(CallError::Remote(p::ProcessError::Denied))
    );
    let aid = a.info(&mut *io).unwrap().id;
    let bid = b.info(&mut *io).unwrap().id;
    assert_ne!(aid, bid);
    assert_eq!(child.list_threads(&mut *io, None).unwrap().entries.len, 2);
    let alias = child.open_thread(&mut *io, aid).unwrap();
    assert_eq!(alias.info(&mut *io).unwrap().id, aid);
    a.watch(&mut *io, &notification).unwrap();
    b.watch(&mut *io, &notification).unwrap();
    for (thread, expected) in [
        (&a, p::ExecutionReason::Exception),
        (&b, p::ExecutionReason::Syscall),
    ] {
        context.run(&mut *io, thread).unwrap();
        sel4::cap::Notification::from_bits(wake.cap_ref().slot as _).wait();
        let stopped = thread.stop(&mut *io).unwrap();
        assert_eq!(stopped.reason, expected);
        if expected == p::ExecutionReason::Syscall {
            assert_eq!(stopped.registers.ax, 123);
        }
    }
    assert_eq!(
        unsafe { core::ptr::read_volatile(data_address as *const u64) },
        141
    );
    assert_eq!(
        observer.thread_info(&mut *io).unwrap().state,
        p::ThreadState::Stopped
    );
    for (thread, mxcsr) in [(&a, 0x1f80u32), (&b, 0x3f80u32)] {
        let bytes = thread.fp_read(&mut *io, 24, 4).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes.data[..4].try_into().unwrap()),
            mxcsr
        );
    }
    let fp_size = a.fp_info(&mut *io).unwrap().bytes as usize;
    let mut image = alloc::vec::Vec::with_capacity(fp_size);
    for offset in (0..fp_size).step_by(256) {
        let bytes = a
            .fp_read(&mut *io, offset as u32, (fp_size - offset).min(256) as u32)
            .unwrap();
        image.extend_from_slice(&bytes.data[..bytes.len as usize]);
    }
    // MXCSR reserved bits are normalized, not rejected. A reserved XSAVE
    // header word, however, must be rejected before touching the TCB.
    assert!(fp_size >= 576);
    image[520..528].copy_from_slice(&1u64.to_le_bytes());
    for offset in (0..fp_size).step_by(256) {
        let length = (fp_size - offset).min(256);
        let mut bytes = List::default();
        bytes.len = length as u32;
        bytes.data[..length].copy_from_slice(&image[offset..offset + length]);
        let commit = offset + length == fp_size;
        let result = a.fp_write(&mut *io, offset as u32, bytes, commit);
        if commit {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
    }
    let bytes = a.fp_read(&mut *io, 24, 4).unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes.data[..4].try_into().unwrap()),
        0x1f80
    );
    // Mutate through RW while the other alias stays RX, then execute the change.
    unsafe {
        core::ptr::write_volatile((code_address + 13) as *mut u8, 0x29);
    }
    let buffer = a.allocate_ipc_buffer(&mut *io).unwrap();
    assert_eq!(a.ipc_buffer(&mut *io).unwrap(), Some(buffer));
    assert_eq!(a.allocate_ipc_buffer(&mut *io).unwrap(), buffer);
    a.set_registers(
        &mut *io,
        registers(executable, shared, shared + 64, shared + 80),
    )
    .unwrap();
    context.run(&mut *io, &a).unwrap();
    sel4::cap::Notification::from_bits(wake.cap_ref().slot as _).wait();
    assert_eq!(
        a.stop(&mut *io).unwrap().reason,
        p::ExecutionReason::Exception
    );
    assert_eq!(
        unsafe { core::ptr::read_volatile(data_address as *const u64) },
        99
    );
    let c = child
        .create_thread(
            &mut *io,
            p::ThreadCreate {
                abi: p::ThreadAbi::TrappedSyscalls,
                allocate_ipc_buffer: true,
                registers: registers(executable + 64, shared, shared + 72, shared + 84),
            },
        )
        .unwrap();
    assert!(c.ipc_buffer(&mut *io).unwrap().is_some());
    c.destroy(&mut *io).unwrap();
    a.destroy(&mut *io).unwrap();
    b.destroy(&mut *io).unwrap();
    assert_eq!(child.list_threads(&mut *io, None).unwrap().entries.len, 0);
    code_region.destroy(&mut *io).unwrap();
    data_region.destroy(&mut *io).unwrap();
    child.terminate(&mut *io).unwrap();
    assert_eq!(child.info(&mut *io).unwrap().ram.charged_bytes, 0);
    child.close(&mut *io).unwrap();
    writer.destroy(&mut *io).unwrap();
    data_writer.destroy(&mut *io).unwrap();
    code.close(&mut *io).unwrap();
    data.close(&mut *io).unwrap();
    assert_eq!(vm.info(&mut *io).unwrap().charged_bytes, initial);
    sel4::debug_println!(
        "generic threads: native/trapped TCBs, shared RAM, observer denial and FP isolation passed"
    );
}
