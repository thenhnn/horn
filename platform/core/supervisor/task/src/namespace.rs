//! Scoped, read-only namespace grants. Publication belongs to a supervised owner.
use super::*;
use horn_registry::{Registry, protocol as r};
const DIRECTORY_BADGE: u64 = 4096;
const MAX_GRANTS: usize = 32;
struct Grant {
    scope: usize,
    owner: u64,
    anchor: Cap<Endpoint>,
    signal: Option<Cap<Notification>>,
    generation: u64,
    armed: bool,
}
pub struct Namespace {
    pub tree: Registry<Cap<Endpoint>>,
    grants: Vec<Option<Grant>>,
    hidden: Vec<usize>,
    root: sel4::cap::CNode,
    service: Cap<Endpoint>,
}
impl Namespace {
    pub fn new(io: &mut Io, service: Cap<Endpoint>, observer: Cap<Endpoint>) -> Self {
        let root = io.space.root;
        let mut tree = Registry::new(64);
        for definition in catalog::SERVICES {
            for scope in definition.publish {
                tree.directory(scope).unwrap();
            }
            for scope in definition.namespaces {
                tree.directory(scope.path).unwrap();
            }
        }
        let processes = copy(io, root, &observer);
        tree.declare("/services/procd", u64::MAX, processes)
            .unwrap();
        tree.ready(u64::MAX);
        Self {
            tree,
            grants: Vec::new(),
            hidden: Vec::new(),
            root,
            service,
        }
    }
    pub fn hide(&mut self, node: usize) {
        self.hidden.push(node);
    }
    pub fn is_hidden(&self, node: usize) -> bool {
        self.hidden.contains(&node)
    }
    pub fn recognizes(&self, badge: u64) -> bool {
        badge >= DIRECTORY_BADGE && (badge - DIRECTORY_BADGE) < self.grants.len() as u64
    }
    pub fn create(
        &mut self,
        io: &mut Io,
        scope: usize,
        owner: u64,
    ) -> Result<Cap<r::RegistryNodeTag>, r::RegistryError> {
        if !self.tree.node(scope).is_directory() {
            return Err(r::RegistryError::Invalid);
        }
        if self.grants.len() == MAX_GRANTS {
            return Err(r::RegistryError::Exhausted);
        }
        let anchor = mint(
            io,
            self.root,
            &self.service,
            DIRECTORY_BADGE + self.grants.len() as u64,
            Rights(Rights::WRITE.0 | Rights::GRANT.0 | Rights::GRANT_REPLY.0),
        );
        let exported = copy(io, self.root, &anchor);
        self.grants.push(Some(Grant {
            scope,
            owner,
            anchor,
            signal: None,
            generation: 0,
            armed: false,
        }));
        Ok(Cap::from_ref(exported.as_ref()))
    }
    /// Only a same-endpoint kernel-unwrapped grant is scope authority.
    /// A path/node/owner number alone never selects a namespace grant.
    pub fn binding(
        &self,
        scope: CapRef,
        path: &List<u8, 96>,
    ) -> Result<(u64, Cap<Endpoint>), r::RegistryError> {
        let badge = scope.unwrapped_badge().ok_or(r::RegistryError::Denied)?;
        let index = badge
            .checked_sub(DIRECTORY_BADGE)
            .ok_or(r::RegistryError::Denied)? as usize;
        let grant = self
            .grants
            .get(index)
            .and_then(Option::as_ref)
            .ok_or(r::RegistryError::Denied)?;
        let path = core::str::from_utf8(&path.data[..path.len as usize])
            .map_err(|_| r::RegistryError::Invalid)?;
        let node = self.tree.resolve(grant.scope, path)?;
        if self.is_hidden(node) {
            return Err(r::RegistryError::NotFound);
        }
        let entry = self.tree.node(node);
        if !entry.available {
            return Err(r::RegistryError::Unavailable);
        }
        let endpoint = *self.tree.value(node).ok_or(r::RegistryError::Invalid)?;
        Ok((entry.owner, endpoint))
    }
    pub fn changed(&mut self, io: &mut Io) {
        for grant in self.grants.iter_mut().flatten() {
            if grant.armed && grant.generation != self.tree.node(grant.scope).generation {
                grant.armed = false;
                io.space
                    .cap::<sel4::cap_type::Notification>(grant.signal.unwrap().as_ref())
                    .unwrap()
                    .signal();
            }
        }
    }
    pub fn remove(&mut self, io: &mut Io, owner: u64) {
        for endpoint in self.tree.remove(owner) {
            io.space.delete(endpoint.as_ref()).unwrap();
        }
        for slot in &mut self.grants {
            if slot.as_ref().is_some_and(|g| g.owner == owner) {
                let grant = slot.take().unwrap();
                self.root
                    .absolute_cptr(sel4::CPtr::from_bits(grant.anchor.as_ref().slot as _))
                    .revoke()
                    .unwrap();
                io.space.delete(grant.anchor.as_ref()).unwrap();
                if let Some(signal) = grant.signal {
                    io.space.delete(signal.as_ref()).unwrap();
                }
            }
        }
    }
}
pub struct DirectoryCaller<'a> {
    pub manager: &'a mut Manager,
    pub badge: u64,
}
impl DirectoryCaller<'_> {
    fn index(&self) -> Result<usize, r::RegistryError> {
        let index = self
            .badge
            .checked_sub(DIRECTORY_BADGE)
            .ok_or(r::RegistryError::Denied)? as usize;
        self.manager
            .namespace
            .grants
            .get(index)
            .and_then(Option::as_ref)
            .map(|_| index)
            .ok_or(r::RegistryError::Denied)
    }
    fn resolve(&self, path: &List<u8, 96>) -> Result<usize, r::RegistryError> {
        let scope = self.manager.namespace.grants[self.index()?]
            .as_ref()
            .unwrap()
            .scope;
        let path = core::str::from_utf8(&path.data[..path.len as usize])
            .map_err(|_| r::RegistryError::Invalid)?;
        let node = self.manager.namespace.tree.resolve(scope, path)?;
        if self.manager.namespace.is_hidden(node) {
            return Err(r::RegistryError::NotFound);
        }
        Ok(node)
    }
}
fn text<const N: usize>(s: &str) -> List<u8, N> {
    let mut result = List::default();
    assert!(s.len() <= N);
    result.len = s.len() as _;
    result.data[..s.len()].copy_from_slice(s.as_bytes());
    result
}
impl r::RegistryDirectory for DirectoryCaller<'_> {
    fn list(&mut self, after: u64, recursive: bool) -> Result<r::RegistryPage, r::RegistryError> {
        let scope = self.manager.namespace.grants[self.index()?]
            .as_ref()
            .unwrap()
            .scope;
        let tree = &self.manager.namespace.tree;
        let mut remaining = tree
            .entries(scope, after, recursive)
            .filter(|(id, _)| !self.manager.namespace.is_hidden(*id));
        let mut entries = List::default();
        let mut last = 0;
        for (id, node) in remaining.by_ref().take(4) {
            last = id as u64;
            entries.data[entries.len as usize] = r::RegistryEntry {
                name: text(&if recursive {
                    tree.path(scope, id)
                } else {
                    node.name.clone()
                }),
                kind: if node.is_directory() {
                    r::RegistryKind::Directory
                } else {
                    r::RegistryKind::Interface
                },
                available: node.available,
            };
            entries.len += 1;
        }
        Ok(r::RegistryPage {
            entries,
            next: if remaining.next().is_some() { last } else { 0 },
            generation: self.manager.namespace.tree.node(scope).generation,
        })
    }
    fn lookup(&mut self, path: List<u8, 96>) -> Result<Cap<Endpoint>, r::RegistryError> {
        let node = self.resolve(&path)?;
        let entry = self.manager.namespace.tree.node(node);
        if !entry.available {
            return Err(r::RegistryError::Unavailable);
        }
        let endpoint = *self
            .manager
            .namespace
            .tree
            .value(node)
            .ok_or(r::RegistryError::Invalid)?;
        Ok(copy(&mut self.manager.io, self.manager.cspace, &endpoint))
    }
    fn open(&mut self, path: List<u8, 96>) -> Result<Cap<r::RegistryNodeTag>, r::RegistryError> {
        let node = self.resolve(&path)?;
        let owner = self.manager.namespace.grants[self.index()?]
            .as_ref()
            .unwrap()
            .owner;
        self.manager
            .namespace
            .create(&mut self.manager.io, node, owner)
    }
    fn watch(
        &mut self,
        signal: Cap<Notification>,
        generation: u64,
    ) -> Result<u64, r::RegistryError> {
        let index = self.index()?;
        if signal.as_ref().unwrapped_badge().is_some() {
            return Err(r::RegistryError::Denied);
        }
        let scope = self.manager.namespace.grants[index].as_ref().unwrap().scope;
        let current = self.manager.namespace.tree.node(scope).generation;
        if generation > current {
            return Err(r::RegistryError::Invalid);
        }
        let retained = copy(&mut self.manager.io, self.manager.cspace, &signal);
        let grant = self.manager.namespace.grants[index].as_mut().unwrap();
        if let Some(old) = grant.signal.replace(retained) {
            self.manager.io.space.delete(old.as_ref()).unwrap();
        }
        grant.generation = generation;
        grant.armed = true;
        self.manager.namespace.changed(&mut self.manager.io);
        Ok(current)
    }
}
