// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//! NOVA 3497a403 irq/{gsp,interrupt_tree,regs,hal/tu102}.rs and GA102
//! RISC-V routing. Exact AD106 single MSI only; vector155 is NOT MSI-X slot2.
use crate::{Error, Result, falcon::Platform};
const LEAF: u32 = 0xb81000;
const ENABLE: u32 = 0xb81200;
const DISABLE: u32 = 0xb81400;
const TOP_ENABLE: u32 = 0xb81608;
const TOP_DISABLE: u32 = 0xb81610;
const MSI_EOI: u32 = 0x88704;
const GSP_LEAF: u32 = 4;
const GSP_BIT: u32 = 1 << 27;
const SWGEN0: u32 = 1 << 6;
fn read(p: &mut impl Platform, address: u32) -> Result<u32> {
    let value = p.read32(address)?;
    if value == u32::MAX || value & 0xffffff00 == 0xbadf4100 {
        return Err(Error::Unavailable);
    }
    Ok(value)
}
fn guarded<P: Platform, T>(p: &mut P, f: impl FnOnce(&mut P) -> Result<T>) -> Result<T> {
    let result = f(p);
    if result.is_err() {
        p.quarantine();
    }
    result
}
fn host_status(p: &mut impl Platform) -> Result<u32> {
    Ok(read(p, 0x110008)? & read(p, 0x111528)? & read(p, 0x11152c)?)
}
/// Stop deliveries, disable every implemented vector and drain read pending
/// bits only. Rearm the actual shared MSI, leave TOP disabled, THEN clear
/// SWGEN0. This is not a DMA drain or proof that firmware is quiescent.
pub fn quiesce(p: &mut impl Platform) -> Result<()> {
    guarded(p, |p| {
        p.write32(TOP_DISABLE, 0xf)?;
        for leaf in 0..8 {
            p.write32(DISABLE + 4 * leaf, u32::MAX)?;
        }
        for leaf in 0..8 {
            let pending = read(p, LEAF + 4 * leaf)?;
            if pending != 0 {
                p.write32(LEAF + 4 * leaf, pending)?;
            }
        }
        p.write32(MSI_EOI, 0)?;
        p.write32(TOP_DISABLE, 0xf)?;
        p.write32(0x110004, SWGEN0)?;
        read(p, LEAF + 4 * GSP_LEAF)?; // posted writes complete before kernel IRQ ack
        Ok(())
    })
}
/// Call only with the remapped handler bound and initial kernel ack completed.
/// Caller must immediately drain queues, including pre-clear notifications.
pub fn enable(p: &mut impl Platform) -> Result<()> {
    guarded(p, |p| {
        if read(p, 0x111388)? & (1 << 7) == 0 {
            return Err(Error::State);
        }
        p.write32(ENABLE + 4 * GSP_LEAF, GSP_BIT)?;
        p.write32(TOP_ENABLE, 1 << 2)?;
        read(p, LEAF + 4 * GSP_LEAF)?;
        Ok(())
    })
}
pub fn disable(p: &mut impl Platform) -> Result<()> {
    guarded(p, |p| {
        p.write32(DISABLE + 4 * GSP_LEAF, GSP_BIT)?;
        p.write32(TOP_DISABLE, 1 << 2)?;
        read(p, LEAF + 4 * GSP_LEAF)?;
        Ok(())
    })
}
#[derive(Debug)]
pub struct Report {
    pub messages: bool,
    pub faults: u32,
    pub remaining: u32,
}
/// Clear leaf155 BEFORE handling causes, preserve unrelated leaf bits, touch
/// ONLY host-routed falcon latches, retrigger notifications, rearm PCI MSI.
/// Caller ACKs seL4 only after this succeeds, then handles faults/drains queues.
/// Any non-SWGEN0 cause is fatal here (stricter than upstream's log-and-continue).
pub fn service(p: &mut impl Platform) -> Result<Report> {
    guarded(p, |p| {
        let pending = read(p, LEAF + 4 * GSP_LEAF)?;
        if pending & GSP_BIT == 0 {
            p.write32(MSI_EOI, 0)?;
            return Ok(Report {
                messages: false,
                faults: 0,
                remaining: 0,
            });
        }
        p.write32(LEAF + 4 * GSP_LEAF, GSP_BIT)?;
        if read(p, 0x111388)? & (1 << 7) == 0 {
            return Err(Error::State);
        }
        let status = host_status(p)?;
        if status & SWGEN0 != 0 {
            p.write32(0x110004, SWGEN0)?;
        }
        let faults = status & !SWGEN0;
        let remaining = if faults != 0 {
            p.write32(0x110004, faults)?;
            host_status(p)? & !SWGEN0
        } else {
            0
        };
        if faults != 0 || remaining != 0 {
            p.write32(DISABLE + 4 * GSP_LEAF, GSP_BIT)?;
        } else {
            p.write32(0x1103e8, 1)?;
        }
        p.write32(MSI_EOI, 0)?;
        read(p, LEAF + 4 * GSP_LEAF)?; // device acknowledgment precedes kernel ack
        Ok(Report {
            messages: status & SWGEN0 != 0,
            faults,
            remaining,
        })
    })
}
