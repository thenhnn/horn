// SPDX-License-Identifier: GPL-2.0-only
//! RGB8 SST policy and nouveau nv50_sor_dp_watermark_sst integer arithmetic.
//! RM owns electrical training; this module never implements a PHY/AUX LT loop.
use super::timing::Timing;
use crate::{Error, Result, raw};

#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub display_id: u32,
    pub sor: u32,
    /// RM OR_GET_INFO link: 1=A, 2=B. Not a physical authority grant.
    pub link: u32,
    pub rate: u8,
    pub lanes: u8,
    pub enhanced: bool,
    pub watermark: u32,
    pub hblank: u32,
    pub vblank: u32,
}

pub fn source_rate(caps: &raw::NV0073_CTRL_CMD_DP_GET_CAPS_PARAMS) -> Result<u8> {
    Ok(match caps.maxLinkRate & raw::HORN_DP_CAP_RATE_MASK {
        raw::NV0073_CTRL_CMD_DP_GET_CAPS_MAX_LINK_RATE_1_62 => 6,
        raw::NV0073_CTRL_CMD_DP_GET_CAPS_MAX_LINK_RATE_2_70 => 10,
        raw::NV0073_CTRL_CMD_DP_GET_CAPS_MAX_LINK_RATE_5_40 => 20,
        raw::NV0073_CTRL_CMD_DP_GET_CAPS_MAX_LINK_RATE_8_10 => 30,
        _ => return Err(Error::Unsupported),
    })
}
impl Link {
    pub fn new(
        t: &Timing,
        display_id: u32,
        sor: u32,
        link: u32,
        rate: u8,
        lanes: u8,
        enhanced: bool,
        increased: bool,
    ) -> Result<Self> {
        if display_id.count_ones() != 1
            || sor >= 4
            || !matches!(link, 1 | 2)
            || !matches!(rate, 6 | 10 | 20 | 30)
            || !matches!(lanes, 1 | 2 | 4)
        {
            return Err(Error::Unsupported);
        }
        let clock = u64::from(t.pixel_clock_khz) * 1000;
        let link_clock = u64::from(rate) * 27_000_000;
        let lanes64 = u64::from(lanes);
        let width = u64::from(t.width);
        let total = u64::from(t.h_total);
        if clock == 0 || width <= 60 || total <= width || clock * 24 >= 8 * link_clock * lanes64 {
            return Err(Error::Unsupported);
        }
        const P: u64 = 100_000;
        let ratio = clock * 24 * P / (8 * link_clock * lanes64);
        let fraction = ratio * 64 * (P - ratio) / P;
        let (adjust, minimum) = if increased { (8, 22) } else { (2, 20) };
        let watermark = adjust + (2 * (24 * P / (8 * lanes64)) + fraction) / P;
        if watermark > 39 || watermark > width * 24 / (8 * lanes64) {
            return Err(Error::Unsupported);
        }
        let steering = if width % lanes64 == 0 {
            0
        } else {
            (lanes64 - width % lanes64) * 24
        };
        let blank_bits = 24 * lanes64 * (1 + u64::from(enhanced)) + 96 + steering;
        let blank_clocks = blank_bits * P / (8 * lanes64);
        let min_blank = (blank_clocks * clock / link_clock) / P + 12;
        if min_blank > total - width {
            return Err(Error::Unsupported);
        }
        let hlatency = match lanes {
            1 => 9,
            2 => 6,
            _ => 3,
        };
        let vlatency = match lanes {
            1 => 39,
            2 => 21,
            _ => 12,
        };
        let hblank =
            ((total - width - min_blank) * link_clock / clock).saturating_sub(4 + hlatency);
        let vblank = ((width - 40) * link_clock / clock).saturating_sub(1 + vlatency);
        Ok(Self {
            display_id,
            sor,
            link,
            rate,
            lanes,
            enhanced,
            watermark: watermark.max(minimum) as u32,
            hblank: hblank as u32,
            vblank: vblank as u32,
        })
    }
    pub fn training(&self, dpcd: &[u8; 16]) -> raw::NV0073_CTRL_DP_CTRL_PARAMS {
        let mut cmd = raw::HORN_DP_TRAIN;
        if self.enhanced {
            cmd |= raw::HORN_DP_ENHANCED;
        }
        if dpcd[2] & 0x20 != 0 && dpcd[3] & 0x80 == 0 {
            cmd |= raw::HORN_DP_POST_ADJUST;
        }
        raw::NV0073_CTRL_DP_CTRL_PARAMS {
            displayId: self.display_id,
            cmd,
            data: (u32::from(self.lanes) << raw::HORN_DP_LANES_SHIFT)
                | (u32::from(self.rate) << raw::HORN_DP_RATE_SHIFT),
            ..Default::default()
        }
    }
    pub fn check_training(&self, reply: &raw::NV0073_CTRL_DP_CTRL_PARAMS) -> Result<()> {
        if reply.subDeviceInstance != 0
            || reply.displayId != self.display_id
            || reply.err != 0
            || reply.retryTimeMs != 0
            || (reply.data >> raw::HORN_DP_LANES_SHIFT) & raw::HORN_DP_LANES_MASK
                != u32::from(self.lanes)
            || (reply.data >> raw::HORN_DP_RATE_SHIFT) & raw::HORN_DP_RATE_MASK
                != u32::from(self.rate)
        {
            return Err(Error::State);
        }
        Ok(())
    }
    pub fn stream(&self) -> raw::NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS {
        raw::NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS {
            sorIndex: self.sor,
            dpLink: u32::from(self.link == 2),
            bEnableOverride: 1,
            hBlankSym: self.hblank,
            vBlankSym: self.vblank,
            SST: raw::NV0073_CTRL_CMD_DP_CONFIG_STREAM_PARAMS__bindgen_ty_2 {
                bEnhancedFraming: u8::from(self.enhanced),
                tuSize: 64,
                waterMark: self.watermark,
                ..Default::default()
            },
            ..Default::default()
        }
    }
    /// DPCD 0x202..0x207: require CR_DONE, CHANNEL_EQ_DONE, SYMBOL_LOCKED on
    /// every active lane and INTERLANE_ALIGN_DONE. No retry or guessed success.
    pub fn check_status(&self, status: &[u8; 6]) -> Result<()> {
        for lane in 0..usize::from(self.lanes) {
            if (status[lane / 2] >> ((lane % 2) * 4)) & 7 != 7 {
                return Err(Error::State);
            }
        }
        if status[2] & 1 == 0 {
            return Err(Error::State);
        }
        Ok(())
    }
}
