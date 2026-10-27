//! Flat CNode packing and unpacking.
//!
//! A message either transfers up to three capabilities directly or one flat
//! CNode containing all of them.  Nested CNodes and multi-root trees are not
//! supported.

use crate::{CapLayout, CapMode, CapNode, CapRef, CapSink, CapSource, Rights, RpcError, WireCaps};

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

impl<S: CapSpace + ?Sized> CapSpace for &mut S {
    fn alloc_cnode(&mut self, parent: Option<SlotRef>, bits: u8) -> Result<CapRef, RpcError> {
        (**self).alloc_cnode(parent, bits)
    }
    fn free_cnode(&mut self, cap: CapRef) -> Result<(), RpcError> {
        (**self).free_cnode(cap)
    }
    fn copy_into(&mut self, dest: SlotRef, src: CapRef, rights: Rights) -> Result<(), RpcError> {
        (**self).copy_into(dest, src, rights)
    }
    fn move_into(&mut self, dest: SlotRef, src: CapRef) -> Result<(), RpcError> {
        (**self).move_into(dest, src)
    }
    fn cap_at(&mut self, slot: SlotRef) -> Result<CapRef, RpcError> {
        (**self).cap_at(slot)
    }
    fn delete(&mut self, cap: CapRef) -> Result<(), RpcError> {
        (**self).delete(cap)
    }
    fn localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        (**self).localize(cap)
    }
    fn install_self_cnode(&mut self, cap: CapRef) -> Result<(), RpcError> {
        (**self).install_self_cnode(cap)
    }
    fn cptr(&self, cap: CapRef) -> Result<u64, RpcError> {
        (**self).cptr(cap)
    }
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
                if !self.layout.variable && self.caps.len() != self.layout.direct.len() {
                    return Err(RpcError::MissingCap);
                }
            }
            Some(node) => {
                if !self.layout.variable && self.index != node.slots.len() {
                    return Err(RpcError::MissingCap);
                }
                if self.layout.variable && self.index == 0 {
                    return Ok(self.caps);
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
        if cap.unwrapped_badge().is_some() {
            return Err(RpcError::NotLocal);
        }
        if self.node.is_none() {
            if self.index >= self.layout.direct.len() {
                return Err(RpcError::TooManyCaps);
            }
            self.index += 1;
            return self.caps.push(cap);
        }
        let node = self.node.unwrap();
        let slot = node.slots.get(self.index).ok_or(RpcError::TooManyCaps)?;
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

    pub fn finish(&self) -> Result<(), RpcError> {
        let expected = if self.layout.cnode.is_some() {
            usize::from(self.index != 0)
        } else {
            self.index
        };
        if self.caps.len() != expected {
            return Err(RpcError::InvalidValue);
        }
        Ok(())
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
        let slots = match self.layout.cnode {
            Some(node) => node.slots,
            None => self.layout.direct,
        };
        let slot = slots.get(self.index).ok_or(RpcError::MissingCap)?;
        self.pop_typed(slot.mode, slot.rights)
    }

    fn pop_typed(&mut self, mode: CapMode, rights: Rights) -> Result<CapRef, RpcError> {
        let node = match self.layout.cnode {
            None => {
                self.layout
                    .direct
                    .get(self.index)
                    .ok_or(RpcError::MissingCap)?;
                self.index += 1;
                let mut cap = self.caps.pop()?;
                if cap.unwrapped_badge().is_some() {
                    if mode != CapMode::BORROW {
                        return Err(RpcError::InvalidValue);
                    }
                    cap.rights = rights;
                    return Ok(cap);
                }
                cap.mode = mode;
                cap.rights = rights;
                return self.maybe_localize(cap);
            }
            Some(node) => node,
        };
        let slot = node.slots.get(self.index).ok_or(RpcError::MissingCap)?;
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
        cap.mode = mode;
        cap.rights = rights;
        self.maybe_localize(cap)
    }
}
