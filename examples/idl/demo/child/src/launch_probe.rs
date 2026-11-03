use alloc::vec::Vec;
use horn_ipc::*;
use horn_procd::protocol::*;
use horn_vm::protocol as vm;

fn request(ram_limit: u64) -> ProcessLaunch {
    let mut name = List::default();
    name.data[..7].copy_from_slice(b"spawned");
    name.len = 7;
    ProcessLaunch { name, ram_limit }
}
pub fn run(ctx: &horn_runtime::Context, mut t: &mut Sel4Transport<Sel4CapSpace>) {
    let factory = ctx.cap("process_factory").unwrap();
    let observer = ctx.cap("process_observer").unwrap();
    let memory = ctx.cap("horn.vm").unwrap();
    let elf = &ctx.cap("elf").unwrap();
    let initial = vm::VmFactoryReceiver::from_cap(memory)
        .info(&mut t)
        .unwrap()
        .charged_bytes;
    assert_eq!(
        ProcessObserverReceiver::from_cap(observer)
            .list(&mut t, 0)
            .unwrap()
            .entries
            .len,
        0
    );
    assert_eq!(
        ProcessFactoryReceiver::from_cap(Cap::from_ref(observer.as_ref())).launch(
            &mut t,
            elf,
            request(8 << 20)
        ),
        Err(CallError::Remote(ProcessError::Denied))
    );
    assert_eq!(
        ProcessControlReceiver::from_cap(Cap::from_ref(observer.as_ref())).info(&mut t,),
        Err(CallError::Remote(ProcessError::Denied))
    );
    assert_eq!(
        ProcessFactoryReceiver::from_cap(factory).open(&mut t, 1),
        Err(CallError::Remote(ProcessError::Denied))
    );
    // An object cap is not enough: FUTURE_WRITE is not an immutable source.
    let bad = vm::VmFactoryReceiver::from_cap(memory)
        .allocate(
            &mut t,
            4096,
            vm::VmBacking::Resident,
            vm::VmProtection::READ,
        )
        .unwrap();
    for seal in [
        None,
        Some(vm::VmSeals::FUTURE_WRITE),
        Some(vm::VmSeals::WRITE),
    ] {
        if let Some(seal) = seal {
            bad.interface::<vm::VmMemoryReceiver>()
                .add_seals(&mut t, seal)
                .unwrap();
        }
        let expected = if seal == Some(vm::VmSeals::WRITE) {
            ProcessError::InvalidElf
        } else {
            ProcessError::Denied
        };
        assert_eq!(
            ProcessFactoryReceiver::from_cap(factory).launch(
                &mut t,
                &bad.borrow().cast(),
                request(8 << 20)
            ),
            Err(CallError::Remote(expected))
        );
    }
    bad.interface::<vm::VmMemoryReceiver>()
        .close(&mut t)
        .unwrap();
    drop(bad);
    assert_eq!(
        ProcessFactoryReceiver::from_cap(factory).launch(&mut t, elf, request(4096)),
        Err(CallError::Remote(ProcessError::QuotaExceeded))
    );
    assert_eq!(
        vm::VmFactoryReceiver::from_cap(memory)
            .info(&mut t,)
            .unwrap()
            .charged_bytes,
        initial
    );
    let mut children = Vec::new();
    for _ in 0..5 {
        let child = ProcessFactoryReceiver::from_cap(factory)
            .launch(&mut t, elf, request(8 << 20))
            .unwrap();
        let info = child
            .interface::<ProcessControlReceiver>()
            .info(&mut t)
            .unwrap();
        assert_eq!(info.state, ProcessState::Created);
        assert!(info.ram.charged_bytes > 0);
        assert_eq!(info.ram.limit_bytes, 8 << 20);
        // Even several loaded children must not spend the launcher's RAM quota.
        assert_eq!(
            vm::VmFactoryReceiver::from_cap(memory)
                .info(&mut t,)
                .unwrap()
                .charged_bytes,
            initial
        );
        children.push(child);
    }
    assert_eq!(
        ProcessFactoryReceiver::from_cap(factory).launch(&mut t, elf, request(8 << 20)),
        Err(CallError::Remote(ProcessError::Exhausted))
    );
    let page = ProcessObserverReceiver::from_cap(observer)
        .list(&mut t, 0)
        .unwrap();
    assert_eq!(page.entries.len, 4);
    assert_ne!(page.next, 0);
    let tail = ProcessObserverReceiver::from_cap(observer)
        .list(&mut t, page.next)
        .unwrap();
    assert_eq!(tail.entries.len, 1);
    assert_eq!(tail.next, 0);
    let extra = ProcessFactoryReceiver::from_cap(factory)
        .open(&mut t, page.entries.data[0].id)
        .unwrap();
    assert_eq!(
        extra
            .interface::<ProcessControlReceiver>()
            .info(&mut t,)
            .unwrap()
            .id,
        page.entries.data[0].id
    );
    drop(extra);
    for child in children.drain(..4) {
        child
            .interface::<ProcessControlReceiver>()
            .terminate(&mut t)
            .unwrap();
        let info = child
            .interface::<ProcessControlReceiver>()
            .info(&mut t)
            .unwrap();
        assert_eq!(info.state, ProcessState::Exited);
        assert_eq!(info.exit, Some(ProcessExit::Killed));
        assert_eq!(info.ram.charged_bytes, 0);
        child
            .interface::<ProcessControlReceiver>()
            .close(&mut t)
            .unwrap();
        drop(child);
    }
    // Keep a child live to verify parent-exit subtree cleanup, not cooperation.
    let _orphan = ProcessFactoryReceiver::from_cap(factory)
        .launch(&mut t, elf, request(8 << 20))
        .unwrap();
    // Source ownership is independent: the loaded image must still run.
    vm::VmMemoryReceiver::from_cap(Cap::from_ref(elf.as_ref()))
        .close(&mut t)
        .unwrap();
    let child = children.pop().unwrap();
    let charge = child
        .interface::<ProcessControlReceiver>()
        .info(&mut t)
        .unwrap()
        .ram
        .charged_bytes;
    assert_eq!(
        child
            .interface::<ProcessControlReceiver>()
            .set_limit(&mut t, charge - 1),
        Err(CallError::Remote(ProcessError::QuotaExceeded))
    );
    assert_eq!(
        child
            .interface::<ProcessControlReceiver>()
            .set_limit(&mut t, 16 << 20),
        Err(CallError::Remote(ProcessError::Denied))
    );
    child
        .interface::<ProcessControlReceiver>()
        .set_limit(&mut t, 8 << 20)
        .unwrap();
    assert_eq!(
        child.interface::<ProcessControlReceiver>().start(&mut t,),
        Err(CallError::Remote(ProcessError::State)),
    );
    let boot = horn_supervisor::client::Bootstrap {
        cnode: t.space.root.bits() as _,
        bits: ctx.cspace_bits(),
        vm: ctx.cap::<Endpoint>("horn.vm").unwrap().as_ref().slot,
        space: ctx
            .cap::<Endpoint>("horn.address-space")
            .unwrap()
            .as_ref()
            .slot,
    };
    let mut entries = [
        libhorn::startup::Entry {
            name: "horn.vm",
            selector: 1,
        },
        libhorn::startup::Entry {
            name: "horn.address-space",
            selector: 2,
        },
        libhorn::startup::Entry {
            name: "horn.lifecycle",
            selector: 3,
        },
        libhorn::startup::Entry {
            name: "horn.reply",
            selector: 4,
        },
    ];
    boot.context(&mut t, &child.borrow().cast(), &mut entries);
    child
        .interface::<ProcessControlReceiver>()
        .start(&mut t)
        .unwrap();
    let mut exited = false;
    for _ in 0..10000 {
        let info = child
            .interface::<ProcessControlReceiver>()
            .info(&mut t)
            .unwrap();
        if info.state == ProcessState::Exited {
            assert_eq!(info.exit, Some(ProcessExit::Code(7)));
            assert_eq!(info.ram.charged_bytes, 0);
            exited = true;
            break;
        }
    }
    assert!(exited);
    assert_eq!(
        child.interface::<ProcessControlReceiver>().start(&mut t,),
        Err(CallError::Remote(ProcessError::State))
    );
    child
        .interface::<ProcessControlReceiver>()
        .close(&mut t)
        .unwrap();
    drop(child);
    assert_eq!(
        ProcessObserverReceiver::from_cap(observer)
            .list(&mut t, 0)
            .unwrap()
            .entries
            .len,
        1
    );
    sel4::debug_println!(
        "LAUNCH_RPC_PASS sealed=true quota=true pagination=true control=true cleanup=true"
    );
}
