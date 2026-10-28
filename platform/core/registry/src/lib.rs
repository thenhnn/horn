//! Bounded service namespace policy. Node IDs are internal selectors, never authority.
//! The server binds a directory scope to a kernel-badged capability. Only trusted
//! launch code declares exports; READY controls availability, CPU stop removes it.
#![no_std]
extern crate alloc;
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}
use protocol::RegistryError as Error;
pub type NodeId = usize;
pub const ROOT: NodeId = 0;
pub struct Node<T> {
    pub name: String,
    pub owner: u64,
    pub available: bool,
    pub generation: u64,
    parent: NodeId,
    value: Option<T>,
    directory: bool,
}
impl<T> Node<T> {
    pub fn is_directory(&self) -> bool {
        self.directory
    }
}
pub struct Registry<T> {
    nodes: Vec<Node<T>>,
    limit: usize,
}
fn component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b'/' && c != b'\\')
}
fn relative(path: &str) -> Result<(), Error> {
    if path.len() > 96 || (!path.is_empty() && !path.split('/').all(component)) {
        return Err(Error::Invalid);
    }
    Ok(())
}
impl<T> Registry<T> {
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0);
        let mut result = Self {
            nodes: Vec::new(),
            limit,
        };
        result.nodes.push(Node {
            name: String::new(),
            owner: 0,
            available: true,
            generation: 1,
            parent: ROOT,
            value: None,
            directory: true,
        });
        result
    }
    pub fn node(&self, id: NodeId) -> &Node<T> {
        &self.nodes[id]
    }
    /// Trusted launch code only; wire callers must resolve within a scoped directory.
    pub fn value(&self, id: NodeId) -> Option<&T> {
        self.nodes[id].value.as_ref()
    }
    pub fn first_available(&self, scope: NodeId) -> Option<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .find(|(id, n)| n.available && n.value.is_some() && self.descends(*id, scope))
            .map(|(id, _)| id)
    }
    fn add(
        &mut self,
        parent: NodeId,
        name: &str,
        owner: u64,
        value: Option<T>,
    ) -> Result<NodeId, Error> {
        if self.nodes.len() == self.limit {
            return Err(Error::Exhausted);
        }
        if self
            .nodes
            .iter()
            .any(|n| n.parent == parent && n.name == name)
        {
            return Err(Error::Invalid);
        }
        let id = self.nodes.len();
        let directory = value.is_none();
        self.nodes.push(Node {
            name: name.to_string(),
            owner,
            available: directory,
            generation: 1,
            parent,
            value,
            directory,
        });
        self.changed(id);
        Ok(id)
    }
    fn changed(&mut self, mut node: NodeId) {
        loop {
            self.nodes[node].generation = self.nodes[node]
                .generation
                .checked_add(1)
                .expect("namespace generation exhausted");
            if node == ROOT {
                break;
            }
            node = self.nodes[node].parent;
        }
    }
    /// Bootstrap/configuration only. Client paths are always relative to their scope.
    pub fn directory(&mut self, absolute: &str) -> Result<NodeId, Error> {
        let path = absolute.strip_prefix('/').ok_or(Error::Invalid)?;
        relative(path)?;
        let mut parent = ROOT;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            parent = match self
                .nodes
                .iter()
                .position(|n| n.parent == parent && n.name == part)
            {
                Some(id) if self.nodes[id].directory => id,
                Some(_) => return Err(Error::Invalid),
                None => self.add(parent, part, 0, None)?,
            };
        }
        Ok(parent)
    }
    pub fn declare(&mut self, path: &str, owner: u64, value: T) -> Result<NodeId, Error> {
        if path.len() > 96 {
            return Err(Error::Invalid);
        }
        let (parent, name) = path.rsplit_once('/').ok_or(Error::Invalid)?;
        if !component(name) {
            return Err(Error::Invalid);
        }
        let parent = self.directory(if parent.is_empty() { "/" } else { parent })?;
        self.add(parent, name, owner, Some(value))
    }
    pub fn resolve(&self, scope: NodeId, path: &str) -> Result<NodeId, Error> {
        relative(path)?;
        if !self.nodes.get(scope).is_some_and(|n| n.directory) {
            return Err(Error::Denied);
        }
        let mut parent = scope;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            parent = self
                .nodes
                .iter()
                .position(|n| n.parent == parent && n.name == part)
                .ok_or(Error::NotFound)?;
        }
        Ok(parent)
    }
    pub fn lookup(&self, scope: NodeId, path: &str) -> Result<&T, Error> {
        let n = &self.nodes[self.resolve(scope, path)?];
        if n.directory {
            return Err(Error::Invalid);
        }
        if !n.available {
            return Err(Error::Unavailable);
        }
        n.value.as_ref().ok_or(Error::Unavailable)
    }
    pub fn children(&self, scope: NodeId, after: u64) -> impl Iterator<Item = (NodeId, &Node<T>)> {
        self.nodes
            .iter()
            .enumerate()
            .filter(move |(i, n)| *i != ROOT && n.parent == scope && (*i as u64) > after)
    }
    pub fn path(&self, scope: NodeId, mut node: NodeId) -> String {
        let mut parts = Vec::new();
        while node != scope {
            parts.push(self.nodes[node].name.as_str());
            node = self.nodes[node].parent;
        }
        parts.reverse();
        parts.join("/")
    }
    pub fn entries(
        &self,
        scope: NodeId,
        after: u64,
        recursive: bool,
    ) -> impl Iterator<Item = (NodeId, &Node<T>)> {
        self.nodes.iter().enumerate().filter(move |(id, n)| {
            (*id as u64) > after
                && *id != scope
                && if recursive {
                    !n.directory && self.descends(*id, scope)
                } else {
                    n.parent == scope
                }
        })
    }
    fn descends(&self, mut node: NodeId, scope: NodeId) -> bool {
        loop {
            if node == scope {
                return true;
            }
            if node == ROOT {
                return false;
            }
            node = self.nodes[node].parent;
        }
    }
    pub fn ready(&mut self, owner: u64) {
        for id in 1..self.nodes.len() {
            if self.nodes[id].owner == owner
                && self.nodes[id].value.is_some()
                && !self.nodes[id].available
            {
                self.nodes[id].available = true;
                self.changed(id);
            }
        }
    }
    /// Caller releases retained capability slots only after withdrawing visibility.
    /// Existing client copies are NOT revoked by removing a name.
    pub fn remove(&mut self, owner: u64) -> Vec<T> {
        let mut removed = Vec::new();
        for id in 1..self.nodes.len() {
            if self.nodes[id].owner == owner {
                if let Some(value) = self.nodes[id].value.take() {
                    self.nodes[id].available = false;
                    removed.push(value);
                    self.changed(id);
                }
            }
        }
        removed
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_and_death_are_not_capability_revocation() {
        let mut r = Registry::new(8);
        let d = r.directory("/consoles").unwrap();
        r.declare("/consoles/a", 1, 42).unwrap();
        assert_eq!(r.lookup(d, "a"), Err(Error::Unavailable));
        r.ready(1);
        let copy = *r.lookup(d, "a").unwrap();
        let generation = r.node(d).generation;
        assert_eq!(r.remove(1), alloc::vec![42]);
        assert!(r.node(d).generation > generation);
        assert_eq!(copy, 42);
        assert_eq!(r.lookup(d, "a"), Err(Error::Unavailable));
        r.ready(1);
        assert_eq!(r.lookup(d, "a"), Err(Error::Unavailable));
    }
    #[test]
    fn scope_cannot_escape_and_changes_are_local() {
        let mut r = Registry::new(12);
        let d = r.directory("/consoles").unwrap();
        r.declare("/services/devmgr", 1, 100).unwrap();
        let g = r.node(d).generation;
        r.ready(1);
        assert_eq!(r.node(d).generation, g);
        for p in [
            "../services/devmgr",
            "/services/devmgr",
            "a/../b",
            "a//b",
            ".",
            "a/",
        ] {
            assert!(r.resolve(d, p).is_err(), "{p}");
        }
        assert!(r.lookup(d, "devmgr").is_err());
    }
    #[test]
    fn capacity_tombstones_and_cursors_are_bounded() {
        let mut r = Registry::new(4);
        let d = r.directory("/c").unwrap();
        let a = r.declare("/c/a", 1, 1).unwrap();
        let b = r.declare("/c/b", 2, 2).unwrap();
        assert!(r.declare("/c/c", 3, 3).is_err());
        r.ready(2);
        assert_eq!(r.first_available(d), Some(b));
        assert_eq!(r.children(d, a as u64).count(), 1);
        r.remove(1);
        assert!(r.declare("/c/a", 1, 1).is_err());
    }
}
