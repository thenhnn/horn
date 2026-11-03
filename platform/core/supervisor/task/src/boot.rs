//! Privileged bootstrap mechanisms. Image/slot/grant policy is catalog data.
use super::*;
use catalog::{BootSource, core};
pub fn start(manager: &mut Manager) {
    let definition = core();
    let boot = manager.boot;
    let root = manager.cspace;
    let io = &mut manager.io;
    let image = boot.upload(io, definition.elf);
    let mut launch = ProcessLaunch::default();
    launch.name.len = definition.name.len() as _;
    launch.name.data[..definition.name.len()].copy_from_slice(definition.name.as_bytes());
    launch.ram_limit = definition.ram;
    let process = manager
        .ctx
        .interface::<ProcessFactory>("supervisor.factory")
        .unwrap()
        .prepare(&mut *io, &image, launch, definition.bootstrap.clone())
        .unwrap();
    horn_supervisor::client::close_image(io, image);
    manager.objects(Cap::from_ref(process.cap_ref()), definition, 0);
    for grant in definition.namespaces {
        assert_eq!(grant.min_ready, 0, "boot namespace cannot wait for READY");
        let node = manager
            .namespace
            .tree
            .resolve(0, grant.path.strip_prefix('/').unwrap())
            .unwrap();
        let directory = manager.namespace.create(&mut manager.io, node, 0).unwrap();
        boot.install(
            &mut manager.io,
            &process,
            grant.slot,
            &directory,
            None,
            Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
        );
        manager.io.space.delete(directory.as_ref()).unwrap();
    }
    let io = &mut manager.io;
    let ready = mint(
        io,
        root,
        &manager
            .ctx
            .cap::<Notification>("supervisor.events")
            .unwrap(),
        CORE_READY,
        Rights::WRITE,
    );
    boot.install(io, &process, definition.ready, &ready, None, Rights::WRITE);
    io.space.delete(ready.as_ref()).unwrap();
    for grant in definition.grants {
        let source = match grant.source {
            BootSource::Platform => copy(
                io,
                root,
                &manager.ctx.cap::<Endpoint>("supervisor.platform").unwrap(),
            ),
            BootSource::SupervisorFactory => mint(
                io,
                root,
                &manager.ctx.cap::<Endpoint>("supervisor.service").unwrap(),
                FACTORY_BADGE,
                Rights::ALL,
            ),
        };
        boot.install(
            io,
            &process,
            grant.slot,
            &source,
            None,
            Rights(grant.rights),
        );
        io.space.delete(source.as_ref()).unwrap();
    }
    let mut entries: Vec<_> = definition
        .context
        .iter()
        .map(|b| libhorn::startup::Entry {
            name: b.name,
            selector: b.slot as u64,
        })
        .collect();
    boot.context(io, &process, &mut entries);
    let control = &process;
    control
        .fault_handler(
            io,
            &boot.cap(boot.cnode),
            boot.bits as _,
            manager
                .ctx
                .cap::<Endpoint>("supervisor.critical_fault")
                .unwrap()
                .as_ref()
                .slot,
        )
        .unwrap();
    control
        .exit_signal(io, &manager.ctx.cap("supervisor.critical_exit").unwrap())
        .unwrap();
    if !(cfg!(feature = "boot-tests") && option_env!("SUPERVISOR_STALL") == Some("1")) {
        control.start(io).unwrap();
    }
    drop(process);
}
