use alloc::vec::Vec;
use horn_ipc::*;
use horn_procd::protocol::*;
use horn_vm::protocol as vm;
use vm::{VmFactoryClient as _, VmMemoryClient as _};

fn request(ram_limit: u64) -> ProcessLaunch {
    let mut name = List::default();
    name.data[..7].copy_from_slice(b"spawned");
    name.len = 7;
    ProcessLaunch { name, ram_limit }
}
pub fn run(ctx: &horn_runtime::Context, mut t: &mut Sel4Transport<Sel4CapSpace>) {
    let factory = ctx.object::<ProcessHostTag>("process_factory").unwrap();
    let observer = ctx.object::<ProcessHostTag>("process_observer").unwrap();
    let memory = ctx.object::<vm::VmServiceTag>("horn.vm").unwrap();
    let elf = &ctx.cap("elf").unwrap();
    let initial = vm::VmFactory::from_ref(memory.as_ref())
        .info(&mut t)
        .unwrap()
        .charged_bytes;
    assert_eq!(
        ProcessObserver::from_ref(observer.as_ref())
            .list(&mut t, 0)
            .unwrap()
            .entries
            .len,
        0
    );
    assert_eq!(
        ProcessFactory::from_ref(observer.as_ref()).launch(&mut t, elf, request(8 << 20)),
        Err(CallError::Remote(ProcessError::Denied))
    );
    assert_eq!(
        ProcessControl::from_ref(observer.as_ref()).info(&mut t,),
        Err(CallError::Remote(ProcessError::Denied))
    );
    assert_eq!(
        ProcessFactory::from_ref(factory.as_ref()).open(&mut t, 1),
        Err(CallError::Remote(ProcessError::Denied))
    );
    // An object cap alone is not an immutable source.
    let bad = vm::VmFactory::from_ref(memory.as_ref())
        .allocate(
            &mut t,
            4096,
            vm::VmBacking::Resident,
            vm::VmProtection::READ,
        )
        .unwrap();
    for immutable in [false, true] {
        if immutable {
            bad.make_immutable(&mut t).unwrap();
        }
        let expected = if immutable {
            ProcessError::InvalidElf
        } else {
            ProcessError::Denied
        };
        assert_eq!(
            ProcessFactory::from_ref(factory.as_ref()).launch(
                &mut t,
                &bad.borrow().cast(),
                request(8 << 20)
            ),
            Err(CallError::Remote(expected))
        );
    }
    bad.close(&mut t).unwrap();
    drop(bad);
    assert_eq!(
        ProcessFactory::from_ref(factory.as_ref()).launch(&mut t, elf, request(4096)),
        Err(CallError::Remote(ProcessError::QuotaExceeded))
    );
    assert_eq!(
        vm::VmFactory::from_ref(memory.as_ref())
            .info(&mut t,)
            .unwrap()
            .charged_bytes,
        initial
    );
    let mut children = Vec::new();
    for _ in 0..5 {
        let child = ProcessFactory::from_ref(factory.as_ref())
            .launch(&mut t, elf, request(8 << 20))
            .unwrap();
        let info = child.info(&mut t).unwrap();
        assert_eq!(info.state, ProcessState::Created);
        assert!(info.ram.charged_bytes > 0);
        assert_eq!(info.ram.limit_bytes, 8 << 20);
        // Even several loaded children must not spend the launcher's RAM quota.
        assert_eq!(
            vm::VmFactory::from_ref(memory.as_ref())
                .info(&mut t,)
                .unwrap()
                .charged_bytes,
            initial
        );
        children.push(child);
    }
    assert_eq!(
        ProcessFactory::from_ref(factory.as_ref()).launch(&mut t, elf, request(8 << 20)),
        Err(CallError::Remote(ProcessError::Exhausted))
    );
    let page = ProcessObserver::from_ref(observer.as_ref())
        .list(&mut t, 0)
        .unwrap();
    assert_eq!(page.entries.len, 4);
    assert_ne!(page.next, 0);
    let tail = ProcessObserver::from_ref(observer.as_ref())
        .list(&mut t, page.next)
        .unwrap();
    assert_eq!(tail.entries.len, 1);
    assert_eq!(tail.next, 0);
    let extra = ProcessFactory::from_ref(factory.as_ref())
        .open(&mut t, page.entries.data[0].id)
        .unwrap();
    assert_eq!(extra.info(&mut t,).unwrap().id, page.entries.data[0].id);
    drop(extra);
    for child in children.drain(..4) {
        child.terminate(&mut t).unwrap();
        let info = child.info(&mut t).unwrap();
        assert_eq!(info.state, ProcessState::Exited);
        assert_eq!(info.exit, Some(ProcessExit::Killed));
        assert_eq!(info.ram.charged_bytes, 0);
        child.close(&mut t).unwrap();
        drop(child);
    }
    // Keep a child live to verify parent-exit subtree cleanup, not cooperation.
    let _orphan = ProcessFactory::from_ref(factory.as_ref())
        .launch(&mut t, elf, request(8 << 20))
        .unwrap();
    // Source ownership is independent: the loaded image must still run.
    vm::VmMemory::from_ref(elf.as_ref()).close(&mut t).unwrap();
    let child = children.pop().unwrap();
    let charge = child.info(&mut t).unwrap().ram.charged_bytes;
    assert_eq!(
        child.set_limit(&mut t, charge - 1),
        Err(CallError::Remote(ProcessError::QuotaExceeded))
    );
    assert_eq!(
        child.set_limit(&mut t, 16 << 20),
        Err(CallError::Remote(ProcessError::Denied))
    );
    child.set_limit(&mut t, 8 << 20).unwrap();
    assert_eq!(
        child.start(&mut t,),
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
    let sharing = crate::sharing_probe::Probe::install(ctx, &mut t, boot, &child.borrow().cast());
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
        libhorn::startup::Entry {
            name: "sharing.leaf",
            selector: 20,
        },
        libhorn::startup::Entry {
            name: "sharing.control",
            selector: 21,
        },
        libhorn::startup::Entry {
            name: "sharing.sibling",
            selector: 22,
        },
    ];
    boot.context(&mut t, &child.borrow().cast(), &mut entries);
    child.start(&mut t).unwrap();
    sharing.revoke_branch(&mut t);
    let mut exited = false;
    for _ in 0..10000 {
        let info = child.info(&mut t).unwrap();
        if info.state == ProcessState::Exited {
            assert_eq!(info.exit, Some(ProcessExit::Code(7)));
            assert_eq!(info.ram.charged_bytes, 0);
            exited = true;
            break;
        }
    }
    assert!(exited);
    sharing.cleanup(&mut t);
    assert_eq!(
        child.start(&mut t,),
        Err(CallError::Remote(ProcessError::State))
    );
    child.close(&mut t).unwrap();
    drop(child);
    assert_eq!(
        ProcessObserver::from_ref(observer.as_ref())
            .list(&mut t, 0)
            .unwrap()
            .entries
            .len,
        1
    );
    sel4::debug_println!(
        "LAUNCH_RPC_PASS immutable=true quota=true pagination=true control=true cleanup=true"
    );
}
