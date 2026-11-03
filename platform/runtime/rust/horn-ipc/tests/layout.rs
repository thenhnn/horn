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
    localized: Vec<CapMode>,
    deleted: Vec<CapRef>,
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
            self.slots
                .insert((parent.cnode.slot.into(), parent.slot), cap);
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
        self.deleted.push(cap);
        self.slots.retain(|_, value| value.slot != cap.slot);
        Ok(())
    }
    fn localize(&mut self, cap: CapRef) -> Result<CapRef, RpcError> {
        self.localized.push(cap.mode);
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
    variable: false,
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
    variable: false,
};

static VARIABLE_CNODE: CapLayout = CapLayout {
    variable: true,
    ..CNODE_LAYOUT
};

#[test]
fn variable_layout_only_transfers_active_caps() {
    let mut space = MockSpace::default();
    let caps = CapPacker::new(&mut space, &VARIABLE_CNODE)
        .finish()
        .unwrap();
    assert_eq!(caps.len(), 0);
    assert!(space.cnodes.is_empty());
    let mut packer = CapPacker::new(&mut space, &VARIABLE_CNODE);
    packer.push(MockSpace::cap(42)).unwrap();
    let caps = packer.finish().unwrap();
    assert_eq!(caps.len(), 1);
    let mut unpacker = CapUnpacker::new_localizing(&mut space, &VARIABLE_CNODE, caps);
    let cap = unpacker.pop_typed(CapMode::MOVE, Rights::WRITE).unwrap();
    assert_eq!(cap.slot, 42);
    assert_eq!(cap.rights, Rights::WRITE);
    unpacker.finish().unwrap();
    assert_eq!(space.localized, vec![CapMode::MOVE]);
}

#[test]
fn inactive_variant_rejects_unexpected_caps() {
    let mut space = MockSpace::default();
    let mut caps = horn_ipc::WireCaps::default();
    caps.push(MockSpace::cap(42)).unwrap();
    let unpacker = CapUnpacker::new(&mut space, &VARIABLE_CNODE, caps);
    assert_eq!(unpacker.finish(), Err(RpcError::InvalidValue));
}

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

#[test]
fn failed_owning_import_deletes_the_fresh_localization() {
    let mut space = MockSpace::default();
    let cap = MockSpace::cap(42);
    space.slots.insert((0, 42), cap);
    let mut caps = horn_ipc::WireCaps::default();
    caps.push(cap).unwrap();
    let mut unpacker = CapUnpacker::new_localizing(&mut space, &DIRECT_LAYOUT, caps);
    // This mock deliberately does not support ownership adoption.
    assert!(matches!(
        unpacker.pop_owned(CapMode::COPY, Rights::ALL),
        Err(RpcError::NotLocal)
    ));
    assert_eq!(space.localized, [CapMode::COPY]);
    assert_eq!(space.deleted, [cap]);
    assert!(space.slots.is_empty());
}

#[test]
fn owning_import_rejects_borrow_and_nonlocalizing_decode_before_consumption() {
    let mut space = MockSpace::default();
    let cap = MockSpace::cap(42);
    let mut caps = horn_ipc::WireCaps::default();
    caps.push(cap).unwrap();
    let mut unpacker = CapUnpacker::new_localizing(&mut space, &DIRECT_LAYOUT, caps);
    assert!(matches!(
        unpacker.pop_owned(CapMode::BORROW, Rights::ALL),
        Err(RpcError::NotLocal)
    ));
    assert_eq!(unpacker.pop_typed(CapMode::COPY, Rights::ALL), Ok(cap));
    assert_eq!(space.localized.len(), 1);
    assert!(space.deleted.is_empty());

    let mut caps = horn_ipc::WireCaps::default();
    caps.push(cap).unwrap();
    let mut unpacker = CapUnpacker::new(&mut space, &DIRECT_LAYOUT, caps);
    assert!(matches!(
        unpacker.pop_owned(CapMode::COPY, Rights::ALL),
        Err(RpcError::NotLocal)
    ));
    assert_eq!(unpacker.pop_typed(CapMode::COPY, Rights::ALL), Ok(cap));
    assert_eq!(space.localized.len(), 1);
}

#[test]
fn kernel_unwrapped_badges_are_borrow_only_and_cannot_be_forwarded() {
    static BORROW: CapLayout = CapLayout {
        direct: &[CapSlot {
            path: "memory",
            slot: 0,
            mode: CapMode::BORROW,
            rights: Rights::ALL,
        }],
        cnode: None,
        variable: false,
    };
    let mut space = MockSpace::default();
    let badge = CapRef::from_unwrapped_badge(42);
    let mut caps = horn_ipc::WireCaps::default();
    caps.push(badge).unwrap();
    let mut unpacker = CapUnpacker::new_localizing(&mut space, &BORROW, caps);
    let value = unpacker.pop_typed(CapMode::BORROW, Rights::READ).unwrap();
    assert_eq!(value.unwrapped_badge(), Some(42));
    unpacker.finish().unwrap();
    assert!(space.localized.is_empty());
    assert_eq!(
        CapPacker::new(&mut space, &BORROW).push(value),
        Err(RpcError::NotLocal)
    );
    for mode in [CapMode::MOVE, CapMode::COPY] {
        let mut caps = horn_ipc::WireCaps::default();
        caps.push(badge).unwrap();
        let mut unpacker = CapUnpacker::new(&mut space, &BORROW, caps);
        assert_eq!(
            unpacker.pop_typed(mode, Rights::ALL),
            Err(RpcError::InvalidValue)
        );
    }
}
