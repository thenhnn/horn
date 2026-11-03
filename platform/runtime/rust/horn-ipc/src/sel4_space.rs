//! A small seL4 CSpace implementation of [`CapSpace`].
//!
//! `CapRef` directly names a CNode slot:
//! `(cnode, slot, depth)`.  There is no handle table.  CNodes are retyped from
//! one Untyped into a caller-provided root-CNode slot range.
//!
//! The current implementation supports flat layouts: root CNodes and one
//! level of child CNodes.  Deeper trees are rejected rather than guessed at.

extern crate alloc;
use crate::owned::LocalCapSpace;
use crate::root_slots::{LocalSlots, Locked, RootSlots};
use crate::{CapType, OwnedCap};
use alloc::sync::Arc;

struct LocalOwner {
    slots: Locked<LocalSlots>,
}
impl LocalCapSpace for LocalOwner {
    fn delete_owned(&self, cap: CapRef) -> Result<(), RpcError> {
        let root = sel4::cap::CNode::from_bits(cap.cnode as Word);
        root.absolute_cptr_from_bits_with_depth(cap.slot as Word, cap.depth as usize)
            .delete()
            .map_err(|_| RpcError::Transport)?;
        self.slots
            .with(|slots| slots.release_taken(cap.slot as usize));
        Ok(())
    }
}
use sel4::cap_type::CNode;
use sel4::{AbsoluteCPtr, CPtr, Cap, CapRights, ObjectBlueprint, Word};

use crate::layout::{CapSpace, SlotRef};
use crate::{CapMode, CapRef, Rights, RpcError};

pub struct Sel4CapSpace {
    pub root: sel4::cap::CNode,
    pub untyped: Option<sel4::cap::Untyped>,
    local: Arc<LocalOwner>,
    root_ref: CapRef,
}

impl Sel4CapSpace {
    pub fn new(
        root: sel4::cap::CNode,
        untyped: sel4::cap::Untyped,
        first_slot: Word,
        end_slot: Word,
    ) -> Self {
        Self::with_untyped(root, Some(untyped), first_slot, end_slot)
    }

    /// A CSpace with no Untyped: it can receive/localize caps into existing
    /// root slots, but cannot allocate new CNodes.
    pub fn new_local(root: sel4::cap::CNode, first_slot: Word, end_slot: Word) -> Self {
        Self::with_untyped(root, None, first_slot, end_slot)
    }

    fn with_untyped(
        root: sel4::cap::CNode,
        untyped: Option<sel4::cap::Untyped>,
        first_slot: Word,
        end_slot: Word,
    ) -> Self {
        Self::with_pool(
            root,
            untyped,
            RootSlots::new(first_slot as usize..end_slot as usize),
        )
    }

    pub fn new_shared_local(root: sel4::cap::CNode, slots: RootSlots) -> Self {
        Self::with_pool(root, None, slots)
    }

    fn with_pool(
        root: sel4::cap::CNode,
        untyped: Option<sel4::cap::Untyped>,
        slots: RootSlots,
    ) -> Self {
        Self {
            root,
            untyped,
            local: Arc::new(LocalOwner {
                slots: Locked::new(LocalSlots::new(slots)),
            }),
            root_ref: CapRef::new(
                root.cptr().bits() as u64,
                0,
                0,
                0,
                CapMode::COPY,
                Rights::ALL,
            ),
        }
    }

    fn root_cptr(&self) -> u64 {
        self.root.cptr().bits() as u64
    }

    fn is_root_ref(&self, cap: CapRef) -> bool {
        cap.depth == 0 && cap.cnode == self.root_cptr()
    }

    fn alloc_root_slot(&mut self) -> Result<Word, RpcError> {
        self.local
            .slots
            .with(|slots| slots.allocate())
            .map(|slot| slot as Word)
            .ok_or(RpcError::TooManyCaps)
    }

    fn cnode_cptr_of(&self, cnode: CapRef) -> Result<u64, RpcError> {
        if self.is_root_ref(cnode) {
            return Ok(self.root_cptr());
        }
        if cnode.cnode == self.root_cptr() {
            return Ok(cnode.slot as u64);
        }
        Err(RpcError::Transport)
    }

    fn cnode_cap_for_slot(&self, cnode: CapRef) -> Result<sel4::cap::CNode, RpcError> {
        if self.is_root_ref(cnode) {
            return Ok(self.root);
        }
        let cptr = self.cnode_cptr_of(cnode)?;
        Ok(Cap::<CNode>::from_bits(cptr as Word))
    }

    fn slot_abs(&self, cnode: CapRef, slot: u32) -> Result<AbsoluteCPtr, RpcError> {
        if self.is_root_ref(cnode) {
            return Ok(self.root.absolute_cptr(CPtr::from_bits(slot as Word)));
        }
        let cnode_cap = self.cnode_cap_for_slot(cnode)?;
        Ok(cnode_cap.absolute_cptr_from_bits_with_depth(slot as Word, cnode.cnode_bits as usize))
    }

    fn cap_abs(&self, cap: CapRef) -> Result<AbsoluteCPtr, RpcError> {
        if cap.unwrapped_badge().is_some() {
            return Err(RpcError::NotLocal);
        }
        let cnode_cap = Cap::<CNode>::from_bits(cap.cnode as Word);
        Ok(cnode_cap.absolute_cptr_from_bits_with_depth(cap.slot as Word, cap.depth as usize))
    }

    /// Adopt a live, unborrowed root-local allocation. Registered initial grants,
    /// receive slots outside this ledger, and already-adopted slots are rejected.
    ///
    /// # Safety
    /// The caller must exclusively own this live capability and abandon all raw
    /// aliases, including any future transfer/deletion through the raw API. The
    /// process root CNode and thread invocation context must remain valid through
    /// Drop (startup roots are process-lifetime grants). A raw Cap is Copy and
    /// cannot prove freshness or exclusivity (like a raw fd).
    pub unsafe fn own<T: CapType>(&mut self, cap: crate::Cap<T>) -> Result<OwnedCap<T>, RpcError> {
        let raw = cap.as_ref();
        if raw.unwrapped_badge().is_some()
            || raw.cnode != self.root_cptr()
            || raw.depth != sel4::WORD_SIZE as u8
            || raw.cnode_bits != 0
            || raw.mode == CapMode::BORROW
            || !self.local.slots.with(|slots| slots.take(raw.slot as usize))
        {
            return Err(RpcError::NotLocal);
        }
        Ok(OwnedCap::new(cap, self.local.clone()))
    }

    pub fn register<T: sel4::CapType>(&mut self, cap: Cap<T>) -> Result<CapRef, RpcError> {
        Ok(CapRef::new(
            self.root_cptr(),
            cap.cptr().bits() as u32,
            sel4::WORD_SIZE as u8,
            0,
            CapMode::COPY,
            Rights::ALL,
        ))
    }

    pub fn reserve_slot(&mut self) -> Result<AbsoluteCPtr, RpcError> {
        let slot = self.alloc_root_slot()?;
        Ok(self.root.absolute_cptr(CPtr::from_bits(slot)))
    }

    pub fn cap<T: sel4::CapType>(&self, cap: CapRef) -> Result<Cap<T>, RpcError> {
        let cptr = self.cptr(cap)?;
        Ok(Cap::<T>::from_bits(cptr as Word))
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
        let (dst, dst_offset, cnode, slot, depth) = match parent {
            None => {
                let slot = self.alloc_root_slot()?;
                (
                    self.root.absolute_cptr_for_self(),
                    slot as usize,
                    self.root_cptr(),
                    slot as u32,
                    sel4::WORD_SIZE as u8,
                )
            }
            Some(parent) => {
                let parent_cnode = self.cnode_cap_for_slot(parent.cnode)?;
                let parent_cptr = self.cnode_cptr_of(parent.cnode)?;
                let depth = if self.is_root_ref(parent.cnode) {
                    sel4::WORD_SIZE as u8
                } else {
                    parent.cnode.cnode_bits
                };
                (
                    parent_cnode.absolute_cptr_for_self(),
                    parent.slot as usize,
                    parent_cptr,
                    parent.slot,
                    depth,
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
        Ok(CapRef::new(
            cnode,
            slot,
            depth,
            size_bits,
            CapMode::COPY,
            Rights::ALL,
        ))
    }

    fn free_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError> {
        self.delete(cnode)
    }

    fn copy_into(&mut self, dest: SlotRef, src: CapRef, rights: Rights) -> Result<(), RpcError> {
        let dest_abs = self.slot_abs(dest.cnode, dest.slot)?;
        let src_abs = self.cap_abs(src)?;
        dest_abs
            .copy(&src_abs, cap_rights(rights))
            .map_err(|_| RpcError::Transport)
    }

    fn move_into(&mut self, dest: SlotRef, src: CapRef) -> Result<(), RpcError> {
        let dest_abs = self.slot_abs(dest.cnode, dest.slot)?;
        let src_abs = self.cap_abs(src)?;
        dest_abs.move_(&src_abs).map_err(|_| RpcError::Transport)
    }

    fn cap_at(&mut self, slot: SlotRef) -> Result<CapRef, RpcError> {
        if self.is_root_ref(slot.cnode) {
            Ok(CapRef::new(
                self.root_cptr(),
                slot.slot,
                sel4::WORD_SIZE as u8,
                0,
                CapMode::COPY,
                Rights::ALL,
            ))
        } else {
            let cnode_cptr = self.cnode_cptr_of(slot.cnode)?;
            Ok(CapRef::new(
                cnode_cptr,
                slot.slot,
                slot.cnode.cnode_bits,
                0,
                CapMode::COPY,
                Rights::ALL,
            ))
        }
    }

    fn delete(&mut self, cap: CapRef) -> Result<(), RpcError> {
        let abs = self.cap_abs(cap)?;
        abs.delete().map_err(|_| RpcError::Transport)?;
        let slot = cap.slot as Word;
        if cap.cnode == self.root_cptr() {
            self.local
                .slots
                .with(|slots| slots.release_if_owned(slot as usize));
        }
        Ok(())
    }

    fn localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        // Even root-local receive caps need their own slot: the receive slot
        // must be empty for the next reply, while returned handles stay live.
        let slot = self.alloc_root_slot()?;
        let dest = SlotRef {
            cnode: self.root_ref,
            slot: slot as u32,
        };
        let local = CapRef::new(
            self.root_cptr(),
            slot as u32,
            sel4::WORD_SIZE as u8,
            0,
            cap.mode,
            cap.rights,
        );
        let result = if cap.mode == CapMode::MOVE {
            self.move_into(dest, cap)
        } else {
            self.copy_into(dest, cap, cap.rights)
        };
        if let Err(error) = result {
            // Recycle only after successful deletion; retain ownership if
            // cleanup fails so an uncertain slot cannot be handed out again.
            self.delete(local)?;
            return Err(error);
        }
        Ok(local)
    }

    fn install_self_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError> {
        // Copy the CNode cap to a temporary root slot, then move that copy
        // into slot 0 of the new CNode.  A direct copy into itself is rejected
        // by the kernel.
        let temp_slot = self.alloc_root_slot()?;
        let temp = CapRef::new(
            self.root_cptr(),
            temp_slot as u32,
            sel4::WORD_SIZE as u8,
            0,
            CapMode::COPY,
            Rights::ALL,
        );
        // Retyped CNode caps have no guard. Mint a full-width self view,
        // while preserving the original cap's short lookup form.
        self.slot_abs(self.root_ref, temp_slot as u32)?
            .mint(
                &self.cap_abs(cnode)?,
                CapRights::all(),
                sel4::CNodeCapData::skip_high_bits(cnode.cnode_bits as usize).into_word(),
            )
            .map_err(|_| RpcError::Transport)?;
        self.move_into(SlotRef { cnode, slot: 0 }, temp)?;
        self.delete(temp)
    }

    fn cptr(&self, cap: CapRef) -> Result<u64, RpcError> {
        if cap.unwrapped_badge().is_none() && cap.cnode == self.root_cptr() {
            return Ok(cap.slot as u64);
        }
        Err(RpcError::NotLocal)
    }
}
