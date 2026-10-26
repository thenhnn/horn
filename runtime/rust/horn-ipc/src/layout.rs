//! CNode layout packing and unpacking.

use crate::{
    CapEntry, CapLayout, CapList, CapMode, CapNode, CapRef, CapSink, CapSource, Rights, RpcError,
};

pub const MAX_LAYOUT_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotRef {
    pub cnode: CapRef,
    pub slot: u32,
}

pub trait CapSpace {
    fn alloc_cnode(&mut self, parent: Option<SlotRef>, size_bits: u8) -> Result<CapRef, RpcError>;
    fn free_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError>;
    fn copy_into(&mut self, dest: SlotRef, src: CapRef, rights: Rights) -> Result<(), RpcError>;
    fn move_into(&mut self, dest: SlotRef, src: CapRef) -> Result<(), RpcError>;
    fn cap_at(&mut self, slot: SlotRef) -> Result<CapRef, RpcError>;
    fn delete(&mut self, cap: CapRef) -> Result<(), RpcError>;
    fn localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError>;
    fn install_self_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError>;
    fn cptr(&self, cap: CapRef) -> Result<u64, RpcError>;
}

#[derive(Clone, Copy)]
struct Cursor {
    cnode: CapRef,
    node: &'static CapNode,
    entry: usize,
    attach_slot: u32,
}

pub struct CapPacker<'a, S: CapSpace> {
    space: &'a mut S,
    layout: &'static CapLayout,
    direct: bool,
    root_index: usize,
    roots: CapList,
    stack: [Option<Cursor>; MAX_LAYOUT_DEPTH],
    depth: usize,
}

impl<'a, S: CapSpace> CapPacker<'a, S> {
    pub fn new(space: &'a mut S, layout: &'static CapLayout) -> Self {
        Self {
            space,
            layout,
            direct: layout.roots.is_empty(),
            root_index: 0,
            roots: CapList::default(),
            stack: [None; MAX_LAYOUT_DEPTH],
            depth: 0,
        }
    }

    fn next_leaf(&mut self) -> Result<Option<SlotRef>, RpcError> {
        loop {
            if self.depth == 0 {
                if self.root_index >= self.layout.roots.len() {
                    return Ok(None);
                }
                let node = self.layout.roots[self.root_index];
                self.root_index += 1;
                let cnode = self.space.alloc_cnode(None, node.size_bits)?;
                self.stack[0] = Some(Cursor {
                    cnode,
                    node,
                    entry: 0,
                    attach_slot: 0,
                });
                self.depth = 1;
            }

            let (entry, cnode, done) = {
                let top = self.stack[self.depth - 1].as_mut().unwrap();
                if top.entry >= top.node.entries.len() {
                    (None, top.cnode, true)
                } else {
                    let entry = top.node.entries[top.entry];
                    top.entry += 1;
                    (Some(entry), top.cnode, false)
                }
            };

            if done {
                let finished = self.stack[self.depth - 1].take().unwrap();
                self.depth -= 1;
                if self.depth == 0 {
                    self.roots.push(CapRef {
                        slot: finished.cnode.slot,
                        mode: CapMode::MOVE,
                        rights: Rights::ALL,
                    })?;
                }
                continue;
            }

            match entry.unwrap() {
                CapEntry::Leaf(leaf) => {
                    return Ok(Some(SlotRef {
                        cnode,
                        slot: leaf.slot,
                    }));
                }
                CapEntry::Node { slot, layout } => {
                    if self.depth >= MAX_LAYOUT_DEPTH {
                        return Err(RpcError::InvalidValue);
                    }
                    let child = self.space.alloc_cnode(
                        Some(SlotRef { cnode, slot }),
                        layout.size_bits,
                    )?;
                    self.stack[self.depth] = Some(Cursor {
                        cnode: child,
                        node: layout,
                        entry: 0,
                        attach_slot: slot,
                    });
                    self.depth += 1;
                }
            }
        }
    }

    pub fn finish(mut self) -> Result<CapList, RpcError> {
        if !self.direct {
            while self.next_leaf()?.is_some() {}
        }
        Ok(self.roots)
    }
}

impl<S: CapSpace> CapSink for CapPacker<'_, S> {
    fn push(&mut self, cap: CapRef) -> Result<(), RpcError> {
        if self.direct {
            return self.roots.push(cap);
        }
        let dest = self.next_leaf()?.ok_or(RpcError::TooManyCaps)?;
        if cap.mode == CapMode::MOVE {
            self.space.move_into(dest, cap)
        } else {
            self.space.copy_into(dest, cap, cap.rights)
        }
    }
}

pub struct CapUnpacker<'a, S: CapSpace> {
    space: &'a mut S,
    layout: &'static CapLayout,
    direct: bool,
    roots: CapList,
    direct_index: usize,
    root_index: usize,
    stack: [Option<Cursor>; MAX_LAYOUT_DEPTH],
    depth: usize,
    localize: bool,
}

impl<'a, S: CapSpace> CapUnpacker<'a, S> {
    pub fn new(space: &'a mut S, layout: &'static CapLayout, roots: CapList) -> Self {
        Self {
            space,
            layout,
            direct: layout.roots.is_empty(),
            roots,
            direct_index: 0,
            root_index: 0,
            stack: [None; MAX_LAYOUT_DEPTH],
            depth: 0,
            localize: false,
        }
    }

    /// Unpack caps and copy/move each one into the caller's root CNode.
    /// This is the ergonomic control-plane path; use `new` for zero-copy
    /// bundle access.
    pub fn new_localizing(space: &'a mut S, layout: &'static CapLayout, roots: CapList) -> Self {
        Self {
            localize: true,
            ..Self::new(space, layout, roots)
        }
    }

    fn maybe_localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        if self.localize {
            self.space.localize(cap)
        } else {
            Ok(cap)
        }
    }

    fn next_leaf(&mut self) -> Result<Option<CapRef>, RpcError> {
        loop {
            if self.depth == 0 {
                let root = match self.roots.pop() {
                    Ok(root) => root,
                    Err(RpcError::MissingCap) => return Ok(None),
                    Err(error) => return Err(error),
                };
                if self.root_index >= self.layout.roots.len() {
                    return Ok(None);
                }
                let node = self.layout.roots[self.root_index];
                self.root_index += 1;
                self.stack[0] = Some(Cursor {
                    cnode: root,
                    node,
                    entry: 0,
                    attach_slot: 0,
                });
                self.depth = 1;
            }

            let (entry, cnode, done) = {
                let top = self.stack[self.depth - 1].as_mut().unwrap();
                if top.entry >= top.node.entries.len() {
                    (None, top.cnode, true)
                } else {
                    let entry = top.node.entries[top.entry];
                    top.entry += 1;
                    (Some(entry), top.cnode, false)
                }
            };

            if done {
                self.stack[self.depth - 1] = None;
                self.depth -= 1;
                continue;
            }

            match entry.unwrap() {
                CapEntry::Leaf(leaf) => {
                    let mut cap = self.space.cap_at(SlotRef {
                        cnode,
                        slot: leaf.slot,
                    })?;
                    cap.mode = leaf.mode;
                    cap.rights = leaf.rights;
                    return self.maybe_localize(cap).map(Some);
                }
                CapEntry::Node { slot, layout } => {
                    if self.depth >= MAX_LAYOUT_DEPTH {
                        return Err(RpcError::InvalidValue);
                    }
                    let child = self.space.cap_at(SlotRef { cnode, slot })?;
                    self.stack[self.depth] = Some(Cursor {
                        cnode: child,
                        node: layout,
                        entry: 0,
                        attach_slot: slot,
                    });
                    self.depth += 1;
                }
            }
        }
    }
}

impl<S: CapSpace> CapSource for CapUnpacker<'_, S> {
    fn pop(&mut self) -> Result<CapRef, RpcError> {
        if self.direct {
            let slot = self
                .layout
                .direct
                .get(self.direct_index)
                .ok_or(RpcError::MissingCap)?;
            let mut cap = self.roots.pop()?;
            cap.mode = slot.mode;
            cap.rights = slot.rights;
            self.direct_index += 1;
            return self.maybe_localize(cap);
        }
        self.next_leaf()?.ok_or(RpcError::MissingCap)
    }
}
