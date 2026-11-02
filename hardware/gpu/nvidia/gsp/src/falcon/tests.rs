// SPDX-License-Identifier: GPL-2.0-only
//! Register-write/deadline checks only. No GPU, firmware execution, or RPC mock.
use super::*;
use alloc::{collections::BTreeMap, vec, vec::Vec};
#[derive(Default)]
struct Recorder {
    time: u64,
    registers: BTreeMap<u32, u32>,
    writes: Vec<(u32, u32, u64)>,
    protected_reads: usize,
    reads: Vec<u32>,
    early_alarm: bool,
    quarantines: usize,
    ignore_flush_writes: bool,
}
impl Platform for Recorder {
    fn read32(&mut self, offset: u32) -> Result<u32> {
        self.reads.push(offset);
        if offset == 0x118234 {
            self.protected_reads += 1;
        }
        Ok(match offset {
            0 => 0x1960_00a1,
            0xa00 => 0x1960_0000,
            0x118234 => 255,
            0x1100f4 => 1 << 31,
            0x110118 => self.registers.get(&offset).copied().unwrap_or(0) | 2,
            _ => self.registers.get(&offset).copied().unwrap_or(0),
        })
    }
    fn write32(&mut self, offset: u32, value: u32) -> Result<()> {
        if !self.ignore_flush_writes || ![0x100c10, 0x100c40].contains(&offset) {
            self.registers.insert(offset, value);
        }
        self.writes.push((offset, value, self.time));
        Ok(())
    }
    fn now_us(&mut self) -> Result<u64> {
        Ok(self.time)
    }
    fn sleep_until(&mut self, deadline: u64) -> Result<()> {
        if !self.early_alarm {
            self.time = deadline;
        }
        Ok(())
    }
    fn dma_write_barrier(&mut self) {}
    fn quarantine(&mut self) {
        self.quarantines += 1;
    }
}
fn controller() -> Ad106<Recorder> {
    let mut io = Recorder::default();
    io.registers.insert(0x118128, 1);
    let mut controller = Ad106::new(io);
    controller.probe(100_000).unwrap();
    controller
}
#[test]
fn ada_flush_registration_matches_the_ga102_ga100_hal() {
    let mut c = controller();
    unsafe {
        c.register_flush_page(0x3dc3000, 100_000).unwrap();
    }
    assert_eq!(c.platform.registers[&0x100c10], 0x3dc30);
    let flush_writes: Vec<_> = c.platform.writes.iter().map(|w| (w.0, w.1)).collect();
    assert_eq!(flush_writes, vec![(0x100c40, 0), (0x100c10, 0x3dc30)]);
    assert_eq!(
        c.flush_page_diagnostic(),
        Some(FlushPageDiagnostic {
            requested_iova: 0x3dc3000,
            before: [Some(0), Some(0)],
            observed: [Some(0x3dc30), Some(0)],
        })
    );
}
#[test]
fn ada_flush_high_address_bits_are_programmed_and_verified() {
    let mut c = controller();
    unsafe {
        c.register_flush_page((1 << 40) + 0x1000, 100_000).unwrap();
    }
    assert_eq!(c.platform.registers[&0x100c10], 0x10);
    assert_eq!(c.platform.registers[&0x100c40], 1);
    assert_eq!(
        c.flush_page_diagnostic().unwrap().observed,
        [Some(0x10), Some(1)]
    );
}
#[test]
fn ignored_flush_write_is_diagnosed_without_retry_or_acceptance() {
    let mut c = controller();
    c.platform.ignore_flush_writes = true;
    assert_eq!(
        unsafe { c.register_flush_page(0x3dc3000, 100_000) },
        Err(Error::State)
    );
    assert_eq!(c.flush, None);
    assert_eq!(c.platform.writes.len(), 2);
    assert_eq!(c.platform.quarantines, 1);
    assert_eq!(
        c.flush_page_diagnostic(),
        Some(FlushPageDiagnostic {
            requested_iova: 0x3dc3000,
            before: [Some(0), Some(0)],
            observed: [Some(0), Some(0)],
        })
    );
}

#[test]
fn flush_address_policy_is_checked_before_register_io() {
    let mut c = controller();
    c.platform.reads.clear();
    assert_eq!(
        unsafe { c.register_flush_page(1 << 47, 100_000) },
        Err(Error::Range)
    );
    assert!(c.platform.reads.is_empty());
    assert!(c.platform.writes.is_empty());
}

#[test]
fn reset_needs_flush_page_and_holds_pulse_after_assertion() {
    let mut controller = controller();
    assert_eq!(controller.reset(Engine::Gsp, 100_000), Err(Error::State));
    assert!(controller.platform.writes.is_empty());
    // Test-only state: Recorder cannot access a GPU or allocate a DMA grant.
    controller.flush = Some(0x1000);
    controller.reset(Engine::Gsp, 100_000).unwrap();
    assert_eq!(
        controller.platform.writes,
        vec![
            (0x1103c0, 1, 0),
            (0x1103c0, 0, 10),
            (0x110084, 0x1960_00a1, 10)
        ]
    );
}
#[test]
fn gfw_timeout_never_reads_protected_progress_or_retries() {
    let mut controller = Ad106::new(Recorder::default());
    assert_eq!(controller.probe(2_000), Err(Error::Timeout));
    assert_eq!(controller.platform.protected_reads, 0);
    assert!(controller.platform.writes.is_empty());
    assert_eq!(controller.diagnostic().unwrap().stage, Stage::Gfw);
    assert_eq!(controller.wpr2(), Err(Error::Quarantined));
    assert_eq!(controller.platform.quarantines, 1);
}
#[test]
fn early_alarm_does_not_turn_into_unbounded_polling() {
    let io = Recorder {
        early_alarm: true,
        ..Recorder::default()
    };
    let mut controller = Ad106::new(io);
    assert_eq!(controller.probe(2_000), Err(Error::ClockStopped));
    assert!(controller.diagnostic().is_some());
}
#[test]
fn imem_tags_and_dmem_source_base_are_not_interchanged() {
    let bytes = [0u8; 1024];
    // Synthetic layout for arithmetic/register checks, NEVER firmware evidence.
    let prepared = Prepared::new(
        &bytes,
        Engine::Gsp,
        Segment {
            source: 256,
            destination: 256,
            length: 256,
        },
        Segment {
            source: 512,
            destination: 0,
            length: 512,
        },
        Brom {
            pkc: 0,
            engines: 0x400,
            ucode: 1,
        },
        0,
        None,
    )
    .unwrap();
    let source = PinnedSource {
        image: &prepared,
        iova: 0x1234_0000,
    };
    let mut controller = controller();
    controller.flush = Some(0x1000);
    controller.phase[0] = Phase::Reset;
    controller.load(Engine::Gsp, &source, 100_000).unwrap();
    let writes = &controller.platform.writes;
    let values = |offset| {
        writes
            .iter()
            .filter(|w| w.0 == offset)
            .map(|w| w.1)
            .collect::<Vec<_>>()
    };
    assert_eq!(values(0x110110), vec![0x123400, 0x123402]);
    assert_eq!(values(0x11011c), vec![256, 0, 256]);
    assert_eq!(values(0x110114), vec![256, 0, 256]);
    assert_eq!(values(0x110118), vec![0x614, 0x600, 0x600]);
    assert_eq!(values(0x111180), vec![1]); // RSA3K
}
