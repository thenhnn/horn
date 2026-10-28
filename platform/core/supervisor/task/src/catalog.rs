//! Data-only launch contracts. Slots come from IDL descriptors, not program kinds.
use super::*;
pub struct ObjectSlot {
    pub slot: u32,
    pub badge: Option<u64>,
    pub rights: u8,
}
pub struct Export {
    pub path: &'static str,
    pub badge: u64,
    pub rights: u8,
}
pub struct Object {
    pub kind: ProcessKernelObject,
    pub slots: &'static [ObjectSlot],
    pub bind: bool,
    pub exports: &'static [Export],
}
pub struct Requirement {
    pub path: &'static str,
    pub first: bool,
    pub slot: u32,
}
pub struct NamespaceGrant {
    pub path: &'static str,
    pub slot: u32,
    pub min_ready: u32,
}
pub enum BootSource {
    Platform,
    SupervisorFactory,
}
pub struct BootGrant {
    pub slot: u32,
    pub source: BootSource,
    pub rights: u8,
}
pub struct ServiceDefinition {
    pub name: &'static str,
    pub elf: &'static [u8],
    pub ram: u64,
    pub boot: bool,
    pub bootstrap: ProcessBootstrap,
    pub ready: u32,
    pub objects: &'static [Object],
    pub requires: &'static [Requirement],
    pub namespaces: &'static [NamespaceGrant],
    pub grants: &'static [BootGrant],
    pub after: &'static [&'static str],
    pub publish: &'static [&'static str],
}
include!(concat!(env!("OUT_DIR"), "/services.rs"));
pub fn core() -> &'static ServiceDefinition {
    SERVICES
        .iter()
        .find(|s| s.boot)
        .expect("boot service definition")
}
impl Manager {
    pub fn objects(
        &mut self,
        process: Cap<ProcessObjectTag>,
        definition: &ServiceDefinition,
        owner: u64,
    ) {
        for object in definition.objects {
            let capability =
                ProcessControlClient::new(&mut self.io, Cap::from_ref(process.as_ref()))
                    .kernel_object(object.kind.clone())
                    .unwrap();
            for binding in object.slots {
                BOOT.install(
                    &mut self.io,
                    process,
                    binding.slot,
                    capability,
                    binding.badge,
                    Rights(binding.rights),
                );
            }
            if object.bind {
                ProcessControlClient::new(&mut self.io, Cap::from_ref(process.as_ref()))
                    .bind_notification(&Cap::from_ref(capability.as_ref()))
                    .unwrap();
            }
            for export in object.exports {
                let endpoint = mint(
                    &mut self.io,
                    root(),
                    Cap::<Endpoint>::from_ref(capability.as_ref()),
                    export.badge,
                    Rights(export.rights),
                );
                self.namespace
                    .tree
                    .declare(export.path, owner, endpoint)
                    .expect("configured export");
            }
            self.io.space.delete(capability.as_ref()).unwrap();
        }
    }
    fn requirements(&self, definition: &ServiceDefinition) -> Option<Vec<usize>> {
        let mut bindings = Vec::new();
        for dependency in definition.after {
            let node = self
                .namespace
                .tree
                .resolve(0, dependency.strip_prefix('/').unwrap())
                .ok()?;
            if !self.namespace.tree.node(node).available {
                return None;
            }
        }
        for required in definition.requires {
            let node = self
                .namespace
                .tree
                .resolve(0, required.path.strip_prefix('/').unwrap())
                .ok()?;
            let node = if required.first {
                self.namespace.tree.first_available(node)?
            } else {
                node
            };
            let export = self.namespace.tree.node(node);
            if !export.available {
                return None;
            }
            bindings.push(node);
        }
        for grant in definition.namespaces {
            let node = self
                .namespace
                .tree
                .resolve(0, grant.path.strip_prefix('/').unwrap())
                .ok()?;
            if grant.min_ready != 0 && self.namespace.tree.first_available(node).is_none() {
                return None;
            }
        }
        Some(bindings)
    }
    pub fn activate(&mut self) {
        for (index, definition) in SERVICES.iter().enumerate() {
            if definition.boot || self.launched[index] {
                continue;
            }
            let Some(bindings) = self.requirements(definition) else {
                if !self.wait_logged[index] {
                    self.wait_logged[index] = true;
                    sel4::debug_println!(
                        "SERVICE_WAIT name={} dependencies=unavailable",
                        definition.name
                    );
                }
                continue;
            };
            self.launched[index] = true;
            let image = BOOT.upload(&mut self.io, definition.elf);
            let mut launch = ProcessLaunch::default();
            launch.name.len = definition.name.len() as _;
            launch.name.data[..definition.name.len()].copy_from_slice(definition.name.as_bytes());
            launch.ram_limit = definition.ram;
            let owner = Caller {
                manager: self,
                badge: FACTORY_BADGE,
            }
            .prepare(image, launch, definition.bootstrap.clone())
            .unwrap();
            horn_supervisor::client::close_image(&mut self.io, image);
            let i = self.jobs.len() - 1;
            self.jobs[i].internal = true;
            let process = self.jobs[i].process;
            self.objects(process, definition, (i + 1) as u64);
            let ready = mint(
                &mut self.io,
                root(),
                cap::<Notification>(SUPERVISOR_CAPS_EVENTS_SLOT),
                ready_bit(i),
                Rights::WRITE,
            );
            BOOT.install(
                &mut self.io,
                process,
                definition.ready,
                ready,
                None,
                Rights::WRITE,
            );
            self.io.space.delete(ready.as_ref()).unwrap();
            for (required, node) in definition.requires.iter().zip(bindings) {
                let entry = self.namespace.tree.node(node);
                self.jobs[i].dependencies.push(entry.owner);
                let endpoint = *self.namespace.tree.value(node).unwrap();
                BOOT.install(
                    &mut self.io,
                    process,
                    required.slot,
                    endpoint,
                    None,
                    Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
                );
            }
            for grant in definition.namespaces {
                let node = self
                    .namespace
                    .tree
                    .resolve(0, grant.path.strip_prefix('/').unwrap())
                    .unwrap();
                let directory = self
                    .namespace
                    .create(&mut self.io, node, (i + 1) as u64)
                    .unwrap();
                BOOT.install(
                    &mut self.io,
                    process,
                    grant.slot,
                    directory,
                    None,
                    Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
                );
                self.io.space.delete(directory.as_ref()).unwrap();
            }
            self.jobs[i].lifecycle.start().unwrap();
            self.jobs[i].deadline = now().wrapping_add(self.ticks * 30_000_000);
            self.process(i).start().unwrap();
            self.io.space.delete(owner.as_ref()).unwrap();
            sel4::debug_println!(
                "SERVICE_START name={} dependencies=resolved",
                definition.name
            );
        }
    }
}
