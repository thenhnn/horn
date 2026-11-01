// SPDX-License-Identifier: GPL-2.0-only
//! Exact preferred timing and conservative uncompressed RGB8 SST preflight.
//! No synthesized timings or silent lower-resolution fallback.
use crate::{
    Error, Result,
    edid::{Mode, Monitor},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub width: u16,
    pub height: u16,
    pub h_total: u16,
    pub v_total: u16,
    pub h_sync_end: u16,
    pub v_sync_end: u16,
    pub h_blank_end: u16,
    pub v_blank_end: u16,
    pub h_blank_start: u16,
    pub v_blank_start: u16,
    pub h_positive: bool,
    pub v_positive: bool,
    pub pixel_clock_khz: u32,
    pub pitch: u32,
    pub bytes: usize,
}
fn axis(
    active: u16,
    total: Option<u16>,
    front: u16,
    sync: u16,
    back: Option<u16>,
) -> Result<(u16, u16, u16, u16)> {
    let total = total.ok_or(Error::Unsupported)?;
    let back = back.ok_or(Error::Unsupported)?;
    let expected = u32::from(active) + u32::from(front) + u32::from(sync) + u32::from(back);
    if active == 0
        || active > 16384
        || total > 32767
        || u32::from(total) != expected
        || sync == 0
        || back == 0
    {
        return Err(Error::Range);
    }
    let blank_end = sync
        .checked_add(back)
        .and_then(|v| v.checked_sub(1))
        .ok_or(Error::Range)?;
    let blank_start = blank_end.checked_add(active).ok_or(Error::Range)?;
    Ok((total, sync - 1, blank_end, blank_start))
}
impl Timing {
    pub fn preferred(monitor: &Monitor) -> Result<Self> {
        let mut preferred = monitor.modes.iter().filter(|mode| mode.preferred);
        let mode = preferred.next().ok_or(Error::Unsupported)?;
        if preferred.next().is_some() {
            return Err(Error::Malformed);
        }
        Self::from_mode(mode)
    }
    pub fn from_mode(mode: &Mode) -> Result<Self> {
        if !mode.preferred || mode.interlaced || mode.has_border {
            return Err(Error::Unsupported);
        }
        let (h_total, h_sync_end, h_blank_end, h_blank_start) = axis(
            mode.width,
            mode.h_total,
            mode.h_front_porch,
            mode.h_sync,
            mode.h_back_porch,
        )?;
        let (v_total, v_sync_end, v_blank_end, v_blank_start) = axis(
            mode.height,
            mode.v_total,
            mode.v_front_porch,
            mode.v_sync,
            mode.v_back_porch,
        )?;
        let pixel_clock_khz = mode
            .pixel_clock_khz
            .filter(|&v| v > 0)
            .ok_or(Error::Unsupported)?;
        let pitch = (u32::from(mode.width) * 4 + 255) & !255;
        let bytes = (pitch as usize)
            .checked_mul(usize::from(mode.height))
            .ok_or(Error::Range)?;
        if bytes > 64 * 1024 * 1024 {
            return Err(Error::Range);
        }
        Ok(Self {
            width: mode.width,
            height: mode.height,
            h_total,
            v_total,
            h_sync_end,
            v_sync_end,
            h_blank_end,
            v_blank_end,
            h_blank_start,
            v_blank_start,
            h_positive: mode.h_sync_positive.ok_or(Error::Unsupported)?,
            v_positive: mode.v_sync_positive.ok_or(Error::Unsupported)?,
            pixel_clock_khz,
            pitch,
            bytes,
        })
    }
    /// DPCD rate units are 270 Mbit/s per lane; 8b/10b leaves rate*27 MHz*8
    /// payload bits. Strict inequality follows nouveau's SST bandwidth check.
    /// GPU/source caps and watermark limits must still be checked before training.
    pub fn sink_link(&self, dpcd: &[u8; 16]) -> Result<(u8, u8)> {
        if dpcd[0] < 0x11 || dpcd[0] > 0x14 {
            return Err(Error::Unsupported);
        }
        let lanes = dpcd[2] & 31;
        if !matches!(lanes, 1 | 2 | 4) {
            return Err(Error::Unsupported);
        }
        let rate = match dpcd[1] {
            0x06 | 0x0a | 0x14 | 0x1e => dpcd[1],
            _ => return Err(Error::Unsupported),
        };
        if u64::from(self.pixel_clock_khz) * 24 >= u64::from(rate) * 27000 * 8 * u64::from(lanes) {
            return Err(Error::Unsupported);
        }
        Ok((rate, lanes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mode() -> Mode {
        Mode {
            preferred: true,
            has_border: false,
            h_sync_positive: Some(true),
            v_sync_positive: Some(true),
            width: 2560,
            height: 1440,
            refresh_mhz: Some(119000),
            interlaced: false,
            pixel_clock_khz: Some(497750),
            h_total: Some(2720),
            h_front_porch: 48,
            h_sync: 32,
            h_back_porch: Some(80),
            v_total: Some(1525),
            v_front_porch: 3,
            v_sync: 5,
            v_back_porch: Some(77),
        }
    }
    #[test]
    fn preferred_has_exact_totals_and_fits_hbr2_four_lanes() {
        let t = Timing::from_mode(&mode()).unwrap();
        assert_eq!(
            (t.h_total, t.v_total, t.h_blank_end, t.h_blank_start),
            (2720, 1525, 111, 2671)
        );
        assert_eq!((t.pitch, t.bytes), (10240, 14745600));
        assert_eq!(
            t.sink_link(&[0x12, 0x14, 0xc4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
                .unwrap(),
            (0x14, 4)
        );
        assert!(
            t.sink_link(&[0x12, 0x0a, 0xc4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
                .is_err()
        );
    }
    #[test]
    fn incomplete_inconsistent_or_nonpreferred_is_not_a_fallback() {
        let mut m = mode();
        m.v_total = None;
        assert!(Timing::from_mode(&m).is_err());
        m = mode();
        m.h_total = Some(2719);
        assert!(Timing::from_mode(&m).is_err());
        m = mode();
        m.interlaced = true;
        assert!(Timing::from_mode(&m).is_err());
        m = mode();
        m.preferred = false;
        assert!(Timing::from_mode(&m).is_err());
        m = mode();
        m.has_border = true;
        assert!(Timing::from_mode(&m).is_err());
        m = mode();
        m.h_sync_positive = None;
        assert!(Timing::from_mode(&m).is_err());
    }
}
