// SPDX-License-Identifier: GPL-2.0-only
//! One immutable SF capture, using crcc57d_set_src/crcc37d_set_ctx and pinned
//! clc37dcrcnotif.h word offsets. No copied notifier ABI structure or recycling.
use super::commands::{Push, notifier_control};
use crate::{Error, Result, raw};

pub const HANDLE: u32 = 0x4000;
// nouveau crcc37d.h: 2047 entries in one 64-KiB context. No context flipping.
pub const BYTES: usize = 0x10000;
pub const MAX_ENTRIES: usize = 2047;
pub const STOP_NOTIFIER: usize = 96;
pub const STRIDE_WORDS: usize =
    (raw::NVC37D_NOTIFIER_CRC_CRC_ENTRY1_21 - raw::NVC37D_NOTIFIER_CRC_CRC_ENTRY0_13) as usize;
pub const FIELD_WORDS: [u32; 3] = [
    raw::NVC37D_NOTIFIER_CRC_CRC_ENTRY0_11,
    raw::NVC37D_NOTIFIER_CRC_CRC_ENTRY0_12,
    raw::NVC37D_NOTIFIER_CRC_CRC_ENTRY0_13,
];

pub fn enable(p: &mut Push) -> Result<()> {
    p.method(raw::HORN_CRC_CONTEXT, &[HANDLE])?;
    p.method(raw::HORN_CRC_CONTROL, &[raw::HORN_CRC_SF])
}
/// Prebuilt core-only capture detach, not a second modeset or a retry. Disabling
/// source before clearing context follows crcc57d_set_src(NONE). Fresh notifier.
pub fn stop(p: &mut Push) -> Result<()> {
    p.method(raw::HORN_CRC_CONTROL, &[0])?;
    p.method(raw::HORN_CRC_CONTEXT, &[0])?;
    p.method(
        raw::NVC77D_SET_NOTIFIER_CONTROL,
        &[notifier_control(STOP_NOTIFIER as u32)?],
    )?;
    p.method(raw::NVC77D_SET_INTERLOCK_FLAGS, &[0, 0])?;
    p.method(raw::NVC77D_UPDATE, &[raw::HORN_DISP_UPDATE])?;
    p.method(
        raw::NVC77D_SET_NOTIFIER_CONTROL,
        &[raw::HORN_DISP_NOTIFIER_DISABLED],
    )
}
pub fn count(status: u32) -> Result<usize> {
    let count = ((status >> raw::HORN_CRC_COUNT_SHIFT) & raw::HORN_CRC_COUNT_MASK) as usize;
    if count > MAX_ENTRIES {
        return Err(Error::Malformed);
    }
    Ok(count)
}
pub fn field_offset(entry: usize, field: usize) -> Result<u64> {
    if entry >= MAX_ENTRIES || field >= FIELD_WORDS.len() {
        return Err(Error::Range);
    }
    let offset = (FIELD_WORDS[field] as usize + entry * STRIDE_WORDS) * 4;
    if offset + 4 > BYTES {
        return Err(Error::Range);
    }
    Ok(offset as u64)
}
