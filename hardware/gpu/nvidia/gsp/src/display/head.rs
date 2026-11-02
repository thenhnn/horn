// SPDX-License-Identifier: GPL-2.0-only
//! Head-0 RGB8 progressive modeset, following nouveau headc57d/corec37d.
//! Identity ILUT/OLUT are real initialized, bounded VRAM objects, not bypass
//! assumptions (both upstream funcs advertise *_identity=true).
use super::{
    commands::{Core, Push, notifier_control},
    dp::Link,
    timing::Timing,
};
use crate::{Error, Result, raw};
use alloc::vec::Vec;

pub const LUT_HANDLE: u32 = 0x3000;
pub const LUT_BYTES: usize = 0x3000;
pub const MODE_NOTIFIER: usize = 32;
pub const WINDOW_NOTIFIER: usize = 64;

fn pair(x: u16, y: u16) -> Result<u32> {
    if u32::from(x) > raw::HORN_HEAD_X_MASK || u32::from(y) > raw::HORN_HEAD_Y_MASK {
        return Err(Error::Range);
    }
    Ok((u32::from(x) << raw::HORN_HEAD_X_SHIFT) | (u32::from(y) << raw::HORN_HEAD_Y_SHIFT))
}

pub fn append(core: &mut Core, t: &Timing, link: &Link) -> Result<()> {
    let clock = t
        .pixel_clock_khz
        .checked_mul(1000)
        .filter(|v| *v <= 0x7fffffff)
        .ok_or(Error::Range)?;
    let p = &mut core.push;
    p.method(
        raw::HORN_HEAD_RASTER_SIZE,
        &[
            pair(t.h_total, t.v_total)?,
            pair(t.h_sync_end, t.v_sync_end)?,
            pair(t.h_blank_end, t.v_blank_end)?,
            pair(t.h_blank_start, t.v_blank_start)?,
        ],
    )?;
    // Progressive only. Do not emit legacy reserved 0x2074 (interlaced blank2)
    // into C77D: the pinned C77D SDK no longer publishes that method.
    p.method(raw::HORN_HEAD_CONTROL, &[0])?;
    p.method(raw::HORN_HEAD_PIXEL_CLOCK_FREQUENCY, &[clock])?;
    p.method(raw::HORN_HEAD_PIXEL_CLOCK_FREQUENCY_MAX, &[clock])?;
    p.method(raw::HORN_HEAD_HEAD_USAGE_BOUNDS, &[raw::HORN_HEAD_BOUNDS])?;
    let size = pair(t.width, t.height)?;
    p.method(raw::HORN_HEAD_VIEWPORT_POINT_IN, &[0, size])?;
    p.method(raw::HORN_HEAD_VIEWPORT_SIZE_OUT, &[size])?;
    p.method(raw::HORN_HEAD_PROCAMP, &[0])?; // RGB / VESA / chroma LPF disabled
    p.method(raw::HORN_HEAD_DITHER_CONTROL, &[0])?;
    p.method(raw::HORN_HEAD_OVERSCAN_COLOR, &[0])?;
    p.method(raw::HORN_HEAD_CONTROL_CURSOR, &[raw::HORN_HEAD_CURSOR_OFF])?;
    p.method(raw::HORN_HEAD_CURSOR_DMA, &[0])?;
    p.method(raw::HORN_HEAD_OCSC1CONTROL, &[0])?;
    p.method(
        raw::HORN_HEAD_OLUT_CONTROL,
        &[raw::HORN_HEAD_OLUT, u32::MAX, LUT_HANDLE, 0],
    )?;
    p.method(raw::HORN_HEAD_DISPLAY_ID, &[link.display_id])?;
    p.method(
        raw::HORN_SOR_CONTROL + link.sor * raw::HORN_SOR_STRIDE,
        &[if link.link == 1 {
            raw::HORN_SOR_DP_A
        } else {
            raw::HORN_SOR_DP_B
        }],
    )?;
    p.method(
        raw::HORN_HEAD_CONTROL_OUTPUT_RESOURCE,
        &[raw::HORN_HEAD_RGB8
            | if t.h_positive { 0 } else { raw::HORN_HEAD_HNEG }
            | if t.v_positive { 0 } else { raw::HORN_HEAD_VNEG }],
    )?;
    p.method(
        raw::NVC77D_SET_NOTIFIER_CONTROL,
        &[notifier_control(MODE_NOTIFIER as u32)?],
    )?;
    p.method(raw::NVC77D_SET_INTERLOCK_FLAGS, &[0, 1])?; // cursor none, window 0
    p.method(raw::NVC77D_UPDATE, &[raw::HORN_DISP_UPDATE])?;
    p.method(
        raw::NVC77D_SET_NOTIFIER_CONTROL,
        &[raw::HORN_DISP_NOTIFIER_DISABLED],
    )
}

pub fn append_window(p: &mut Push, notifier: u32) -> Result<()> {
    p.method(
        raw::NVC57E_SET_ILUT_CONTROL,
        &[raw::HORN_WINDOW_ILUT, LUT_HANDLE, 0],
    )?;
    p.method(
        raw::NVC57E_SET_CONTEXT_DMA_NOTIFIER,
        &[notifier, raw::HORN_WINDOW_NOTIFIER_64],
    )?;
    p.method(
        raw::NVC57E_SET_INTERLOCK_FLAGS,
        &[raw::HORN_WINDOW_INTERLOCK_CORE, 1],
    )?;
    p.method(raw::NVC57E_UPDATE, &[raw::HORN_WINDOW_UPDATE])
}

/// nv50_lut_load identity ramp (i << 6), headc57d_olut_load and
/// wndwc57e_ilut_load. Both have a 32-byte zero VSS header and duplicated last
/// entry. Head stores U0.16, window stores FP16. Remaining backing is zeroed.
pub fn identity_luts() -> Result<Vec<u32>> {
    let mut words = Vec::new();
    words
        .try_reserve_exact(2 * LUT_BYTES / 4)
        .map_err(|_| Error::Exhausted)?;
    words.resize(2 * LUT_BYTES / 4, 0);
    for i in 0..=1024usize {
        let fixed = (i.min(1023) as u16) << 6;
        let fp = if fixed == 0 {
            0
        } else {
            let shift = fixed.leading_zeros();
            (((14 - shift) as u16) << 10) | (((fixed << shift) >> 5) & 0x3ff)
        };
        for (base, v) in [(0, fixed), (LUT_BYTES / 4, fp)] {
            let at = base + 8 + i * 2;
            words[at] = u32::from(v) | (u32::from(v) << 16);
            words[at + 1] = u32::from(v);
        }
    }
    Ok(words)
}
