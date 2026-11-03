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
    let initial = vm::VmFactoryClient::new(&mut t, memory)
        .info()
        .unwrap()
        .charged_bytes;
    assert_eq!(
        ProcessObserverClient::new(&mut t, observer)
            .list(0)
            .unwrap()
            .entries
            .len,
        0
    );
    assert_eq!(
        ProcessFactoryClient::new(&mut t, Cap::from_ref(observer.as_ref()))
            .launch(elf, request(8 << 20)),
        Err(CallError::Remote(ProcessError::Denied))
    );
    assert_eq!(
        ProcessControlClient::new(&mut t, Cap::from_ref(observer.as_ref())).info(),
        Err(CallError::Remote(ProcessError::Denied))
    );
    assert_eq!(
        ProcessFactoryClient::new(&mut t, factory).open(1),
        Err(CallError::Remote(ProcessError::Denied))
    );
    // An object cap is not enough: FUTURE_WRITE is not an immutable source.
    let bad = vm::VmFactoryClient::new(&mut t, memory)
        .allocate(4096, vm::VmBacking::Resident, vm::VmProtection::READ)
        .unwrap();
    for seal in [
        None,
        Some(vm::VmSeals::FUTURE_WRITE),
        Some(vm::VmSeals::WRITE),
    ] {
        if let Some(seal) = seal {
            vm::VmMemoryClient::new(&mut t, Cap::from_ref(bad.as_ref()))
                .add_seals(seal)
                .unwrap();
        }
        let expected = if seal == Some(vm::VmSeals::WRITE) {
            ProcessError::InvalidElf
        } else {
            ProcessError::Denied
        };
        assert_eq!(
            ProcessFactoryClient::new(&mut t, factory)
                .launch(&Cap::from_ref(bad.as_ref()), request(8 << 20)),
            Err(CallError::Remote(expected))
        );
    }
    vm::VmMemoryClient::new(&mut t, Cap::from_ref(bad.as_ref()))
        .close()
        .unwrap();
    t.space.delete(bad.as_ref()).unwrap();
    assert_eq!(
        ProcessFactoryClient::new(&mut t, factory).launch(elf, request(4096)),
        Err(CallError::Remote(ProcessError::QuotaExceeded))
    );
    assert_eq!(
        vm::VmFactoryClient::new(&mut t, memory)
            .info()
            .unwrap()
            .charged_bytes,
        initial
    );
    let mut children = Vec::new();
    for _ in 0..5 {
        let child = ProcessFactoryClient::new(&mut t, factory)
            .launch(elf, request(8 << 20))
            .unwrap();
        let info = ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
            .info()
            .unwrap();
        assert_eq!(info.state, ProcessState::Created);
        assert!(info.ram.charged_bytes > 0);
        assert_eq!(info.ram.limit_bytes, 8 << 20);
        // Even several loaded children must not spend the launcher's RAM quota.
        assert_eq!(
            vm::VmFactoryClient::new(&mut t, memory)
                .info()
                .unwrap()
                .charged_bytes,
            initial
        );
        children.push(child);
    }
    assert_eq!(
        ProcessFactoryClient::new(&mut t, factory).launch(elf, request(8 << 20)),
        Err(CallError::Remote(ProcessError::Exhausted))
    );
    let page = ProcessObserverClient::new(&mut t, observer)
        .list(0)
        .unwrap();
    assert_eq!(page.entries.len, 4);
    assert_ne!(page.next, 0);
    let tail = ProcessObserverClient::new(&mut t, observer)
        .list(page.next)
        .unwrap();
    assert_eq!(tail.entries.len, 1);
    assert_eq!(tail.next, 0);
    let extra = ProcessFactoryClient::new(&mut t, factory)
        .open(page.entries.data[0].id)
        .unwrap();
    assert_eq!(
        ProcessControlClient::new(&mut t, Cap::from_ref(extra.as_ref()))
            .info()
            .unwrap()
            .id,
        page.entries.data[0].id
    );
    t.space.delete(extra.as_ref()).unwrap();
    for child in &children[..4] {
        ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
            .terminate()
            .unwrap();
        let info = ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
            .info()
            .unwrap();
        assert_eq!(info.state, ProcessState::Exited);
        assert_eq!(info.exit, Some(ProcessExit::Killed));
        assert_eq!(info.ram.charged_bytes, 0);
        ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
            .close()
            .unwrap();
        t.space.delete(child.as_ref()).unwrap();
    }
    // Keep a child live to verify parent-exit subtree cleanup, not cooperation.
    let _orphan = ProcessFactoryClient::new(&mut t, factory)
        .launch(elf, request(8 << 20))
        .unwrap();
    // Source ownership is independent: the loaded image must still run.
    vm::VmMemoryClient::new(&mut t, Cap::from_ref(elf.as_ref()))
        .close()
        .unwrap();
    let child = children[4];
    let charge = ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
        .info()
        .unwrap()
        .ram
        .charged_bytes;
    assert_eq!(
        ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref())).set_limit(charge - 1),
        Err(CallError::Remote(ProcessError::QuotaExceeded))
    );
    assert_eq!(
        ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref())).set_limit(16 << 20),
        Err(CallError::Remote(ProcessError::Denied))
    );
    ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
        .set_limit(8 << 20)
        .unwrap();
    assert_eq!(
        ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref())).start(),
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
    boot.context(&mut t, Cap::from_ref(child.as_ref()), &mut entries);
    ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
        .start()
        .unwrap();
    let mut exited = false;
    for _ in 0..10000 {
        let info = ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
            .info()
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
        ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref())).start(),
        Err(CallError::Remote(ProcessError::State))
    );
    ProcessControlClient::new(&mut t, Cap::from_ref(child.as_ref()))
        .close()
        .unwrap();
    t.space.delete(child.as_ref()).unwrap();
    assert_eq!(
        ProcessObserverClient::new(&mut t, observer)
            .list(0)
            .unwrap()
            .entries
            .len,
        1
    );
    sel4::debug_println!(
        "LAUNCH_RPC_PASS sealed=true quota=true pagination=true control=true cleanup=true"
    );
}
