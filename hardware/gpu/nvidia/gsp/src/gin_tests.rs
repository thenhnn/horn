// SPDX-License-Identifier: GPL-2.0-only
//! Narrow register-order and host-cause selection checks; no IRQ/GPU model.
use crate::{Result, falcon::Platform, gin};
use alloc::{collections::BTreeMap, vec::Vec};
#[derive(Default)]
struct Registers {
    values: BTreeMap<u32, u32>,
    writes: Vec<(u32, u32)>,
}
impl Platform for Registers {
    fn read32(&mut self, address: u32) -> Result<u32> {
        Ok(*self.values.get(&address).unwrap_or(&0))
    }
    fn write32(&mut self, address: u32, value: u32) -> Result<()> {
        self.writes.push((address, value));
        Ok(())
    }
    fn now_us(&mut self) -> Result<u64> {
        unreachable!()
    }
    fn sleep_until(&mut self, _: u64) -> Result<()> {
        unreachable!()
    }
    fn dma_write_barrier(&mut self) {}
}
#[test]
fn notification_clears_only_leaf155_and_host_swgen0_before_single_msi_rearm() {
    let mut p = Registers::default();
    for (address, value) in [
        (0xb81010, (1 << 27) | 8),
        (0x111388, 1 << 7),
        (0x110008, 0x44),
        (0x111528, 0x44),
        (0x11152c, 0x40),
    ] {
        p.values.insert(address, value);
    }
    let report = gin::service(&mut p).unwrap();
    assert!(report.messages);
    assert_eq!(report.faults, 0);
    assert_eq!(
        p.writes,
        [
            (0xb81010, 1 << 27),
            (0x110004, 0x40),
            (0x1103e8, 1),
            (0x88704, 0)
        ]
    );
}
#[test]
fn startup_drain_clears_read_pending_bits_before_swgen0() {
    let mut p = Registers::default();
    p.values.insert(0xb81010, 1 << 27);
    gin::quiesce(&mut p).unwrap();
    assert_eq!(p.writes[0], (0xb81610, 15));
    assert_eq!(
        &p.writes[9..],
        &[
            (0xb81010, 1 << 27),
            (0x88704, 0),
            (0xb81610, 15),
            (0x110004, 0x40)
        ]
    );
}
