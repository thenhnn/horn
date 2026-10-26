use std::collections::HashMap;

use horn_ipc::layout::{CapPacker, CapSpace, CapUnpacker, SlotRef};
use horn_ipc::{
    CapEntry, CapLayout, CapMode, CapNode, CapRef, CapSink, CapSlot, CapSource, Rights, RpcError,
};

#[derive(Default)]
struct MockSpace {
    next: u64,
    slots: HashMap<(u64, u32), CapRef>,
    cnodes: HashMap<u64, u8>,
}

impl MockSpace {
    fn cap(slot: u64) -> CapRef {
        CapRef {
            slot,
            mode: CapMode::COPY,
            rights: Rights::ALL,
        }
    }
}

impl CapSpace for MockSpace {
    fn alloc_cnode(&mut self, parent: Option<SlotRef>, size_bits: u8) -> Result<CapRef, RpcError> {
        self.next += 1;
        let id = self.next;
        self.cnodes.insert(id, size_bits);
        let cap = Self::cap(id);
        if let Some(parent) = parent {
            self.slots.insert((parent.cnode.slot, parent.slot), cap);
        }
        Ok(cap)
    }
    fn free_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError> {
        self.cnodes.remove(&cnode.slot);
        Ok(())
    }
    fn copy_into(&mut self, dest: SlotRef, src: CapRef, _rights: Rights) -> Result<(), RpcError> {
        self.slots.insert((dest.cnode.slot, dest.slot), src);
        Ok(())
    }
    fn move_into(&mut self, dest: SlotRef, src: CapRef) -> Result<(), RpcError> {
        self.slots.insert((dest.cnode.slot, dest.slot), src);
        Ok(())
    }
    fn cap_at(&mut self, slot: SlotRef) -> Result<CapRef, RpcError> {
        self.slots
            .get(&(slot.cnode.slot, slot.slot))
            .copied()
            .ok_or(RpcError::MissingCap)
    }
    fn delete(&mut self, cap: CapRef) -> Result<(), RpcError> {
        self.slots.retain(|_, value| value.slot != cap.slot);
        Ok(())
    }
    fn localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        Ok(cap)
    }
    fn install_self_cnode(&mut self, _cnode: CapRef) -> Result<(), RpcError> {
        Ok(())
    }
    fn cptr(&self, cap: CapRef) -> Result<u64, RpcError> {
        Ok(cap.slot)
    }
}

static LEAF_NODE: CapNode = CapNode {
    size_bits: 2,
    entries: &[
        CapEntry::Leaf(CapSlot {
            path: "a",
            slot: 0,
            mode: CapMode::COPY,
            rights: Rights::ALL,
        }),
        CapEntry::Leaf(CapSlot {
            path: "b",
            slot: 1,
            mode: CapMode::MOVE,
            rights: Rights::ALL,
        }),
        CapEntry::Leaf(CapSlot {
            path: "c",
            slot: 2,
            mode: CapMode::BORROW,
            rights: Rights::ALL,
        }),
        CapEntry::Leaf(CapSlot {
            path: "d",
            slot: 3,
            mode: CapMode::COPY,
            rights: Rights::ALL,
        }),
    ],
};

static TREE_LAYOUT: CapLayout = CapLayout {
    direct: &[],
    roots: &[&LEAF_NODE],
};

static DIRECT_LAYOUT: CapLayout = CapLayout {
    direct: &[
        CapSlot {
            path: "x",
            slot: 0,
            mode: CapMode::COPY,
            rights: Rights::ALL,
        },
        CapSlot {
            path: "y",
            slot: 1,
            mode: CapMode::MOVE,
            rights: Rights::ALL,
        },
    ],
    roots: &[],
};

#[test]
fn packs_and_unpacks_a_cnode_tree() {
    let mut space = MockSpace::default();
    let mut packer = CapPacker::new(&mut space, &TREE_LAYOUT);
    for slot in 10..14 {
        packer.push(MockSpace::cap(slot)).unwrap();
    }
    let roots = packer.finish().unwrap();
    assert_eq!(roots.len(), 1);

    let mut unpacker = CapUnpacker::new(&mut space, &TREE_LAYOUT, roots);
    let mut modes = Vec::new();
    let mut slots = Vec::new();
    for _ in 0..4 {
        let cap = unpacker.pop().unwrap();
        modes.push(cap.mode);
        slots.push(cap.slot);
    }
    assert_eq!(slots, vec![10, 11, 12, 13]);
    assert_eq!(
        modes,
        vec![CapMode::COPY, CapMode::MOVE, CapMode::BORROW, CapMode::COPY]
    );
}

#[test]
fn packs_direct_caps_without_a_cnode() {
    let mut space = MockSpace::default();
    let mut packer = CapPacker::new(&mut space, &DIRECT_LAYOUT);
    packer.push(MockSpace::cap(1)).unwrap();
    packer.push(MockSpace::cap(2)).unwrap();
    let roots = packer.finish().unwrap();
    assert_eq!(roots.len(), 2);

    let mut unpacker = CapUnpacker::new(&mut space, &DIRECT_LAYOUT, roots);
    assert_eq!(unpacker.pop().unwrap().slot, 1);
    assert_eq!(unpacker.pop().unwrap().slot, 2);
    assert!(unpacker.pop().is_err());
}
