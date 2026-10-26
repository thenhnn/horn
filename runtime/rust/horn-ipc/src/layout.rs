//! Flat CNode packing and unpacking.
//!
//! A message either transfers up to three capabilities directly or one flat
//! CNode containing all of them.  Nested CNodes and multi-root trees are not
//! supported.

use crate::{
    CapLayout, WireCaps, CapMode, CapNode, CapRef, CapSink, CapSource, Rights, RpcError,
};

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

pub struct CapPacker<'a, S: CapSpace> {
    space: &'a mut S,
    layout: &'static CapLayout,
    node: Option<&'static CapNode>,
    index: usize,
    cnode: Option<CapRef>,
    caps: WireCaps,
}

impl<'a, S: CapSpace> CapPacker<'a, S> {
    pub fn new(space: &'a mut S, layout: &'static CapLayout) -> Self {
        Self {
            space,
            layout,
            node: layout.cnode,
            index: 0,
            cnode: None,
            caps: WireCaps::default(),
        }
    }

    fn ensure_cnode(&mut self) -> Result<CapRef, RpcError> {
        if let Some(cnode) = self.cnode {
            return Ok(cnode);
        }
        let node = self.node.ok_or(RpcError::TooManyCaps)?;
        let cnode = self.space.alloc_cnode(None, node.size_bits)?;
        self.cnode = Some(cnode);
        Ok(cnode)
    }

    pub fn finish(mut self) -> Result<WireCaps, RpcError> {
        match self.node {
            None => {
                if self.caps.len() != self.layout.direct.len() {
                    return Err(RpcError::MissingCap);
                }
            }
            Some(node) => {
                if self.index != node.slots.len() {
                    return Err(RpcError::MissingCap);
                }
                let cnode = self.cnode.ok_or(RpcError::MissingCap)?;
                self.caps.push(cnode.with_mode(CapMode::MOVE))?;
            }
        }
        Ok(self.caps)
    }
}

impl<S: CapSpace> CapSink for CapPacker<'_, S> {
    fn push(&mut self, cap: CapRef) -> Result<(), RpcError> {
        if self.node.is_none() {
            if self.index >= self.layout.direct.len() {
                return Err(RpcError::TooManyCaps);
            }
            self.index += 1;
            return self.caps.push(cap);
        }
        let node = self.node.unwrap();
        let slot = node
            .slots
            .get(self.index)
            .ok_or(RpcError::TooManyCaps)?;
        self.index += 1;
        let cnode = self.ensure_cnode()?;
        let dest = SlotRef {
            cnode,
            slot: slot.slot,
        };
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
    caps: WireCaps,
    index: usize,
    cnode: Option<CapRef>,
    localize: bool,
}

impl<'a, S: CapSpace> CapUnpacker<'a, S> {
    pub fn new(space: &'a mut S, layout: &'static CapLayout, caps: WireCaps) -> Self {
        Self {
            space,
            layout,
            caps,
            index: 0,
            cnode: None,
            localize: false,
        }
    }

    /// Unpack caps and copy/move each one into the caller's root CNode.
    /// This is the ergonomic control-plane path; use `new` for zero-copy
    /// CNode access.
    pub fn new_localizing(space: &'a mut S, layout: &'static CapLayout, caps: WireCaps) -> Self {
        Self {
            localize: true,
            ..Self::new(space, layout, caps)
        }
    }

    fn maybe_localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        if self.localize {
            self.space.localize(cap)
        } else {
            Ok(cap)
        }
    }
}

impl<S: CapSpace> CapSource for CapUnpacker<'_, S> {
    fn pop(&mut self) -> Result<CapRef, RpcError> {
        let node = match self.layout.cnode {
            None => {
                let slot = self
                    .layout
                    .direct
                    .get(self.index)
                    .ok_or(RpcError::MissingCap)?;
                self.index += 1;
                let mut cap = self.caps.pop()?;
                cap.mode = slot.mode;
                cap.rights = slot.rights;
                return self.maybe_localize(cap);
            }
            Some(node) => node,
        };
        let slot = node
            .slots
            .get(self.index)
            .ok_or(RpcError::MissingCap)?;
        self.index += 1;
        let cnode = match self.cnode {
            Some(cnode) => cnode,
            None => {
                let cnode = self.caps.pop()?;
                self.cnode = Some(cnode);
                cnode
            }
        };
        let mut cap = self.space.cap_at(SlotRef {
            cnode,
            slot: slot.slot,
        })?;
        cap.mode = slot.mode;
        cap.rights = slot.rights;
        self.maybe_localize(cap)
    }
}
