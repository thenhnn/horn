//! Privileged bootstrap mechanisms. All image/slot/grant policy is catalog data.
use super::*;
use catalog::{BootSource, core};
pub fn start(manager: &mut Manager) {
    let definition = core();
    let io = &mut manager.io;
    let image = BOOT.upload(io, definition.elf);
    let mut launch = ProcessLaunch::default();
    launch.name.len = definition.name.len() as _;
    launch.name.data[..definition.name.len()].copy_from_slice(definition.name.as_bytes());
    launch.ram_limit = definition.ram;
    let process = ProcessFactoryClient::new(&mut *io, cap(SUPERVISOR_CAPS_FACTORY_SLOT))
        .prepare(&image, launch, definition.bootstrap.clone())
        .unwrap();
    horn_supervisor::client::close_image(io, image);
    manager.objects(process, definition, 0);
    let io = &mut manager.io;
    let ready = mint(
        io,
        root(),
        cap::<Notification>(SUPERVISOR_CAPS_EVENTS_SLOT),
        CORE_READY,
        Rights::WRITE,
    );
    BOOT.install(io, process, definition.ready, ready, None, Rights::WRITE);
    io.space.delete(ready.as_ref()).unwrap();
    for grant in definition.grants {
        let source = match grant.source {
            BootSource::Platform => {
                copy(io, root(), cap::<Endpoint>(SUPERVISOR_CAPS_PLATFORM_SLOT))
            }
            BootSource::SupervisorFactory => mint(
                io,
                root(),
                cap::<Endpoint>(SUPERVISOR_CAPS_SERVICE_SLOT),
                FACTORY_BADGE,
                Rights::ALL,
            ),
        };
        BOOT.install(io, process, grant.slot, source, None, Rights(grant.rights));
        io.space.delete(source.as_ref()).unwrap();
    }
    let mut control = ProcessControlClient::new(&mut *io, Cap::from_ref(process.as_ref()));
    control
        .fault_handler(
            &cap(SUPERVISOR_CAPS_SELF_SLOT),
            SUPERVISOR_CAPS_SIZE_BITS as _,
            SUPERVISOR_CAPS_CRITICAL_FAULT_SLOT,
        )
        .unwrap();
    control
        .exit_signal(&cap(SUPERVISOR_CAPS_CRITICAL_EXIT_SLOT))
        .unwrap();
    if !(cfg!(feature = "boot-tests") && option_env!("SUPERVISOR_STALL") == Some("1")) {
        control.start().unwrap();
    }
    io.space.delete(process.as_ref()).unwrap();
}
