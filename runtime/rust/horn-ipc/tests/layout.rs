use std::collections::HashMap;

use horn_ipc::layout::{CapPacker, CapSpace, CapUnpacker, SlotRef};
use horn_ipc::{
    CapLayout, CapMode, CapNode, CapRef, CapSink, CapSlot, CapSource, Rights, RpcError,
};

#[derive(Default)]
struct MockSpace {
    next: u64,
    slots: HashMap<(u64, u32), CapRef>,
    cnodes: HashMap<u64, u8>,
}

impl MockSpace {
    fn cap(slot: u64) -> CapRef {
        CapRef::new(0, slot as u32, 0, 0, CapMode::COPY, Rights::ALL)
    }
}

impl CapSpace for MockSpace {
    fn alloc_cnode(&mut self, parent: Option<SlotRef>, size_bits: u8) -> Result<CapRef, RpcError> {
        self.next += 1;
        let id = self.next;
        self.cnodes.insert(id, size_bits);
        let cap = CapRef::new(0, id as u32, 0, size_bits, CapMode::COPY, Rights::ALL);
        if let Some(parent) = parent {
            self.slots.insert((parent.cnode.slot.into(), parent.slot), cap);
        }
        Ok(cap)
    }
    fn free_cnode(&mut self, cnode: CapRef) -> Result<(), RpcError> {
        self.cnodes.remove(&(cnode.slot as u64));
        Ok(())
    }
    fn copy_into(&mut self, dest: SlotRef, src: CapRef, _rights: Rights) -> Result<(), RpcError> {
        self.slots.insert((dest.cnode.slot.into(), dest.slot), src);
        Ok(())
    }
    fn move_into(&mut self, dest: SlotRef, src: CapRef) -> Result<(), RpcError> {
        self.slots.insert((dest.cnode.slot.into(), dest.slot), src);
        Ok(())
    }
    fn cap_at(&mut self, slot: SlotRef) -> Result<CapRef, RpcError> {
        self.slots
            .get(&(slot.cnode.slot as u64, slot.slot))
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
        Ok(cap.slot as u64)
    }
}

static CNODE: CapNode = CapNode {
    size_bits: 2,
    slots: &[
        CapSlot {
            path: "a",
            slot: 0,
            mode: CapMode::COPY,
            rights: Rights::ALL,
        },
        CapSlot {
            path: "b",
            slot: 1,
            mode: CapMode::MOVE,
            rights: Rights::ALL,
        },
        CapSlot {
            path: "c",
            slot: 2,
            mode: CapMode::BORROW,
            rights: Rights::ALL,
        },
        CapSlot {
            path: "d",
            slot: 3,
            mode: CapMode::COPY,
            rights: Rights::ALL,
        },
    ],
};

static CNODE_LAYOUT: CapLayout = CapLayout {
    direct: &[],
    cnode: Some(&CNODE),
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
    cnode: None,
};

#[test]
fn packs_and_unpacks_a_cnode() {
    let mut space = MockSpace::default();
    let mut packer = CapPacker::new(&mut space, &CNODE_LAYOUT);
    for slot in 10..14 {
        packer.push(MockSpace::cap(slot)).unwrap();
    }
    let caps = packer.finish().unwrap();
    assert_eq!(caps.len(), 1);

    let mut unpacker = CapUnpacker::new(&mut space, &CNODE_LAYOUT, caps);
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
    let caps = packer.finish().unwrap();
    assert_eq!(caps.len(), 2);

    let mut unpacker = CapUnpacker::new(&mut space, &DIRECT_LAYOUT, caps);
    assert_eq!(unpacker.pop().unwrap().slot, 1);
    assert_eq!(unpacker.pop().unwrap().slot, 2);
    assert!(unpacker.pop().is_err());
}
