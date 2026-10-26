//! A small seL4 CSpace implementation of [`CapSpace`].
//!
//! This is intentionally conservative:
//! - CNodes are retyped from one Untyped into a caller-provided free slot
//!   range of the root CNode.
//! - Capability handles are indices into a fixed table of `AbsoluteCPtr`s.
//! - Slots and handles are bump-allocated; there is no reclamation yet.
//!
//! That is enough for bootstrap and control-plane work, and the layout code
//! can be swapped to a real allocator later.

use sel4::cap_type::{CNode, Untyped};
use sel4::{AbsoluteCPtr, CPtr, Cap, CapRights, ObjectBlueprint, Word};

use crate::layout::{CapSpace, SlotRef};
use crate::{CapRef, CapMode, Rights, RpcError};

pub const MAX_CAP_HANDLES: usize = 256;
const ROOT_HANDLE: u64 = u64::MAX;

pub struct Sel4CapSpace {
    pub root: sel4::cap::CNode,
    pub untyped: Option<sel4::cap::Untyped>,
    next_slot: Word,
    end_slot: Word,
    next_handle: usize,
    handles: [Option<AbsoluteCPtr>; MAX_CAP_HANDLES],
    cnode_sizes: [u8; MAX_CAP_HANDLES],
    root_handle: CapRef,
}

impl Sel4CapSpace {
    pub fn new(
        root: sel4::cap::CNode,
        untyped: sel4::cap::Untyped,
        first_slot: Word,
        end_slot: Word,
    ) -> Self {
        Self {
            root,
            untyped: Some(untyped),
            next_slot: first_slot,
            end_slot,
            next_handle: 0,
            handles: [None; MAX_CAP_HANDLES],
            cnode_sizes: [0; MAX_CAP_HANDLES],
            root_handle: CapRef {
                slot: ROOT_HANDLE,
                mode: CapMode::COPY,
                rights: Rights::ALL,
            },
        }
    }

    /// A CSpace with no Untyped: it can receive/localize caps into existing
    /// root slots, but cannot allocate new CNodes.
    pub fn new_local(root: sel4::cap::CNode, first_slot: Word, end_slot: Word) -> Self {
        Self {
            root,
            untyped: None,
            next_slot: first_slot,
            end_slot,
            next_handle: 0,
            handles: [None; MAX_CAP_HANDLES],
            cnode_sizes: [0; MAX_CAP_HANDLES],
            root_handle: CapRef {
                slot: ROOT_HANDLE,
                mode: CapMode::COPY,
                rights: Rights::ALL,
            },
        }
    }

    pub fn register<T: sel4::CapType>(&mut self, cap: Cap<T>) -> Result<CapRef, RpcError> {
        let abs = self
            .root
            .absolute_cptr(CPtr::from_bits(cap.cptr().bits() as Word));
        self.insert(abs)
    }

    pub fn reserve_slot(&mut self) -> Result<AbsoluteCPtr, RpcError> {
        let slot = self.alloc_root_slot()?;
        Ok(self.root.absolute_cptr(CPtr::from_bits(slot)))
    }

    pub fn cap<T: sel4::CapType>(&self, cap: CapRef) -> Result<Cap<T>, RpcError> {
        let slot = self.cptr(cap)?;
        Ok(Cap::<T>::from_bits(slot as Word))
    }

    fn insert(&mut self, abs: AbsoluteCPtr) -> Result<CapRef, RpcError> {
        while self.next_handle < MAX_CAP_HANDLES {
            let index = self.next_handle;
            self.next_handle += 1;
            if self.handles[index].is_none() {
                self.handles[index] = Some(abs);
                return Ok(CapRef {
                    slot: index as u64,
                    mode: CapMode::COPY,
                    rights: Rights::ALL,
                });
            }
        }
        Err(RpcError::TooManyCaps)
    }

    fn get(&self, cap: CapRef) -> Result<AbsoluteCPtr, RpcError> {
        if cap.slot == ROOT_HANDLE {
            return Ok(self.root.absolute_cptr_for_self());
        }
        self.handles
            .get(cap.slot as usize)
            .and_then(|slot| *slot)
            .ok_or(RpcError::MissingCap)
    }

    fn cnode_cap(&self, cap: CapRef) -> Result<sel4::cap::CNode, RpcError> {
        if cap.slot == ROOT_HANDLE {
            return Ok(self.root);
        }
        let abs = self.get(cap)?;
        Ok(Cap::<CNode>::from_bits(abs.path().bits() as Word))
    }

    fn slot_abs(&self, cnode: CapRef, slot: u32) -> Result<AbsoluteCPtr, RpcError> {
        let dest_cnode = self.cnode_cap(cnode)?;
        let size_bits = if cnode.slot == ROOT_HANDLE {
            0
        } else {
            self.cnode_sizes.get(cnode.slot as usize).copied().unwrap_or(0) as usize
        };
        if size_bits == 0 {
            Ok(dest_cnode.absolute_cptr(CPtr::from_bits(slot as Word)))
        } else {
            Ok(dest_cnode.absolute_cptr_from_bits_with_depth(slot as Word, size_bits))
        }
    }

    fn alloc_root_slot(&mut self) -> Result<Word, RpcError> {
        if self.next_slot >= self.end_slot {
            return Err(RpcError::TooManyCaps);
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        Ok(slot)
    }
}

fn cap_rights(rights: Rights) -> CapRights {
    CapRights::new(
        rights.0 & Rights::GRANT_REPLY.0 != 0,
        rights.0 & Rights::GRANT.0 != 0,
        rights.0 & Rights::READ.0 != 0,
        rights.0 & Rights::WRITE.0 != 0,
    )
}

impl CapSpace for Sel4CapSpace {
    fn alloc_cnode(&mut self, parent: Option<SlotRef>, size_bits: u8) -> Result<CapRef, RpcError> {
        let (dst, dst_offset, abs) = match parent {
            None => {
                let slot = self.alloc_root_slot()?;
                (
                    self.root.absolute_cptr_for_self(),
                    slot as usize,
                    self.root.absolute_cptr(CPtr::from_bits(slot)),
                )
            }
            Some(parent) => {
                let cnode = self.cnode_cap(parent.cnode)?;
                (
                    cnode.absolute_cptr_for_self(),
                    parent.slot as usize,
                    cnode.absolute_cptr(CPtr::from_bits(parent.slot as Word)),
                )
            }
        };
        self.untyped
            .ok_or(RpcError::Transport)?
            .untyped_retype(
                &ObjectBlueprint::CNode {
                    size_bits: size_bits as usize,
                },
                &dst,
                dst_offset,
                1,
            )
            .map_err(|_| RpcError::Transport)?;
        let cap = self.insert(abs)?;
        if cap.slot != ROOT_HANDLE {
            self.cnode_sizes[cap.slot as usize] = size_bits;
        }
        Ok(cap)
    }

    fn free_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError> {
        if cnode.slot == ROOT_HANDLE {
            return Err(RpcError::InvalidValue);
        }
        let abs = self.get(cnode)?;
        abs.delete().map_err(|_| RpcError::Transport)?;
        if let Some(slot) = self.handles.get_mut(cnode.slot as usize) {
            *slot = None;
        }
        Ok(())
    }

    fn copy_into(&mut self, dest: SlotRef, src: CapRef, rights: Rights) -> Result<(), RpcError> {
        let dest_abs = self.slot_abs(dest.cnode, dest.slot)?;
        let src_abs = self.get(src)?;
        dest_abs
            .copy(&src_abs, cap_rights(rights))
            .map_err(|_| RpcError::Transport)
    }

    fn move_into(&mut self, dest: SlotRef, src: CapRef) -> Result<(), RpcError> {
        let dest_abs = self.slot_abs(dest.cnode, dest.slot)?;
        let src_abs = self.get(src)?;
        dest_abs
            .move_(&src_abs)
            .map_err(|_| RpcError::Transport)?;
        if let Some(slot) = self.handles.get_mut(src.slot as usize) {
            *slot = None;
        }
        Ok(())
    }

    fn cap_at(&mut self, slot: SlotRef) -> Result<CapRef, RpcError> {
        let abs = self.slot_abs(slot.cnode, slot.slot)?;
        self.insert(abs)
    }

    fn delete(&mut self, cap: CapRef) -> Result<(), RpcError> {
        if cap.slot == ROOT_HANDLE {
            return Err(RpcError::InvalidValue);
        }
        let abs = self.get(cap)?;
        abs.delete().map_err(|_| RpcError::Transport)?;
        if let Some(slot) = self.handles.get_mut(cap.slot as usize) {
            *slot = None;
        }
        Ok(())
    }

    fn localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        if cap.slot == ROOT_HANDLE {
            return Ok(cap);
        }
        let abs = self.get(cap)?;
        if abs.root().cptr().bits() == self.root.cptr().bits() {
            return Ok(cap);
        }
        let slot = self.alloc_root_slot()?;
        if slot > u32::MAX as Word {
            return Err(RpcError::TooManyCaps);
        }
        let dest = SlotRef {
            cnode: self.root_handle,
            slot: slot as u32,
        };
        if cap.mode == CapMode::MOVE {
            self.move_into(dest, cap)?;
        } else {
            self.copy_into(dest, cap, cap.rights)?;
        }
        let mut localized = self.insert(self.root.absolute_cptr(CPtr::from_bits(slot)))?;
        localized.mode = cap.mode;
        localized.rights = cap.rights;
        Ok(localized)
    }

    fn install_self_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError> {
        // Copy the CNode cap to a temporary slot in the root CNode, then move
        // that copy into slot 0 of the new CNode.  A direct CNode_Copy into
        // itself is rejected by the kernel.
        let temp_slot = self.alloc_root_slot()?;
        if temp_slot > u32::MAX as Word {
            return Err(RpcError::TooManyCaps);
        }
        let temp_abs = self.root.absolute_cptr(CPtr::from_bits(temp_slot));
        let temp_cap = self.insert(temp_abs)?;
        self.copy_into(
            SlotRef {
                cnode: self.root_handle,
                slot: temp_slot as u32,
            },
            cnode,
            Rights::ALL,
        )?;
        self.move_into(SlotRef { cnode, slot: 0 }, temp_cap)
    }

    fn cptr(&self, cap: CapRef) -> Result<u64, RpcError> {
        if cap.slot == ROOT_HANDLE {
            return Ok(self.root.cptr().bits() as u64);
        }
        let abs = self.get(cap)?;
        if abs.root().cptr().bits() != self.root.cptr().bits() {
            return Err(RpcError::NotLocal);
        }
        Ok(abs.path().bits() as u64)
    }
}
