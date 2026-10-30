// SPDX-License-Identifier: GPL-2.0-only
//! EDID decoding, delegated to the `piaf` crate.
//!
//! Everything structural (header, checksum, extension blocks, CEA-861 and
//! DisplayID descriptors) belongs to the crate. This module only narrows the
//! result to what a modeset needs: the display name, its physical size and the
//! modes it advertises, ordered with the preferred detailed timing first.

use alloc::{string::String, vec::Vec};
use piaf::{ExtensionLibrary, ScreenSize, capabilities_from_edid, parse_edid};

use crate::{Error, Result};

/// One advertised mode, reduced to the numbers a mode set programs.
///
/// The engine needs totals, and EDID states them implicitly: the crate gives the
/// horizontal blanking and the porches, so the vertical total is derived from
/// the pixel clock and the exact refresh rate instead of being guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u16,
    pub height: u16,
    /// Refresh rate in millihertz, exact when the source stated it.
    pub refresh_mhz: Option<u32>,
    pub interlaced: bool,
    pub pixel_clock_khz: Option<u32>,
    pub h_total: Option<u16>,
    pub h_front_porch: u16,
    pub h_sync: u16,
    pub h_back_porch: Option<u16>,
    pub v_total: Option<u16>,
    pub v_front_porch: u16,
    pub v_sync: u16,
    pub v_back_porch: Option<u16>,
}

/// Decoded monitor description.
#[derive(Clone, Debug)]
pub struct Monitor {
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    pub width_cm: Option<u8>,
    pub height_cm: Option<u8>,
    pub modes: Vec<Mode>,
}

/// Decode an EDID blob (one base block plus any extensions the monitor sent).
pub fn parse(bytes: &[u8]) -> Result<Monitor> {
    let library = ExtensionLibrary::with_standard_handlers();
    let parsed = parse_edid(bytes, &library).map_err(|_| Error::Checksum)?;
    let caps = capabilities_from_edid(&parsed, &library);

    let (width_cm, height_cm) = match caps.screen_size {
        Some(ScreenSize::Physical {
            width_cm,
            height_cm,
        }) => (Some(width_cm), Some(height_cm)),
        _ => (None, None),
    };

    let mut modes = Vec::new();
    modes
        .try_reserve_exact(caps.supported_modes.len())
        .map_err(|_| Error::Exhausted)?;
    for mode in &caps.supported_modes {
        let refresh_mhz = mode.refresh_rate.map(|rate| {
            // Millihertz keeps 59.94 distinct from 60.
            u32::try_from(u64::from(rate.numer()) * 1000 / u64::from(rate.denom())).unwrap_or(0)
        });
        let h_total = mode.h_blank.map(|blank| mode.width.saturating_add(blank));
        let h_back_porch = mode
            .h_blank
            .map(|blank| blank.saturating_sub(mode.h_front_porch + mode.h_sync_width));
        // refresh = pixel_clock / (h_total * v_total), so v_total follows from
        // the exact rate; without either input it stays unknown.
        let v_total = match (mode.pixel_clock_khz, h_total, mode.refresh_rate) {
            (Some(clock), Some(htotal), Some(rate)) if htotal != 0 && rate.numer() != 0 => {
                let numerator = u64::from(clock) * 1000 * u64::from(rate.denom());
                let denominator = u64::from(htotal) * u64::from(rate.numer());
                u16::try_from(numerator / denominator).ok()
            }
            _ => None,
        };
        let v_back_porch = v_total.map(|total| {
            total.saturating_sub(mode.height + mode.v_front_porch + mode.v_sync_width)
        });
        modes.push(Mode {
            width: mode.width,
            height: mode.height,
            refresh_mhz,
            interlaced: mode.interlaced,
            pixel_clock_khz: mode.pixel_clock_khz,
            h_total,
            h_front_porch: mode.h_front_porch,
            h_sync: mode.h_sync_width,
            h_back_porch,
            v_total,
            v_front_porch: mode.v_front_porch,
            v_sync: mode.v_sync_width,
            v_back_porch,
        });
    }

    // EDID lists established and standard timings before the detailed ones, but
    // only a detailed timing carries a pixel clock and blanking, so only those
    // can be programmed. Put them first and keep the rest as a fallback.
    modes.sort_by_key(|mode| mode.pixel_clock_khz.is_none());

    Ok(Monitor {
        name: caps.display_name.as_ref().map(|name| name.as_str().into()),
        manufacturer: caps.manufacturer.as_ref().map(|id| id.as_str().into()),
        width_cm,
        height_cm,
        modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920x1080@60 detailed timing descriptor with 148.5 MHz pixel clock.
    fn edid() -> Vec<u8> {
        let mut block = [0u8; 128];
        block[0..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        // Manufacturer "HOR" (1-5-15-18-15 in the packed 5-bit encoding).
        block[8] = 0x1c;
        block[9] = 0x6e;
        block[10] = 0x00;
        block[11] = 0x01; // product code
        block[12] = 0x01;
        block[13] = 0x00;
        block[16] = 50; // week
        block[17] = 30; // year - 1990
        block[18] = 1;
        block[19] = 4; // EDID 1.4
        block[20] = 0xa2; // digital input
        block[21] = 52; // 52 cm
        block[22] = 29; // 29 cm
        block[23] = 120; // gamma 2.20
        // Preferred detailed timing: 1920x1080@60, 148.5 MHz.
        let dtd = &mut block[54..72];
        dtd[0] = 0x02;
        dtd[1] = 0x3a; // pixel clock 14850 * 10 kHz
        dtd[2] = 0x80; // h active low
        dtd[3] = 0x18; // h blank low
        dtd[4] = 0x71; // h active high | h blank high
        dtd[5] = 0x38; // v active low
        dtd[6] = 0x2d; // v blank low
        dtd[7] = 0x40; // v active high | v blank high
        dtd[8] = 0x58; // h front porch low
        dtd[9] = 0x2c; // h sync low
        dtd[10] = 0x45; // v front porch / v sync low nibbles
        dtd[11] = 0x00; // sync offsets high bits
        dtd[12] = 0xfd; // 1920 mm x 1080 mm
        dtd[13] = 0x1e;
        dtd[14] = 0x11;
        // Monitor name descriptor.
        block[72..90].copy_from_slice(&[
            0x00, 0x00, 0x00, 0xfc, 0x00, b'H', b'O', b'R', b'N', b' ', b'P', b'A', b'N', b'E',
            b'L', b'\n', b' ', b' ',
        ]);
        // Range limits descriptor: not needed for this test.
        block[90..108].copy_from_slice(&[
            0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ]);
        // Extension count and checksum.
        block[126] = 0;
        let sum: u8 = block.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        block[127] = 0u8.wrapping_sub(sum);
        block.to_vec()
    }

    #[test]
    fn preferred_timing_and_name_are_recovered() {
        let monitor = parse(&edid()).unwrap();
        assert_eq!(monitor.name.as_deref(), Some("HORN PANEL"));
        assert_eq!(monitor.width_cm, Some(52));
        assert_eq!(monitor.height_cm, Some(29));
        assert!(!monitor.modes.is_empty());
        let preferred = monitor.modes[0];
        assert_eq!((preferred.width, preferred.height), (1920, 1080));
        assert_eq!(preferred.pixel_clock_khz, Some(148_500));
        assert!(!preferred.interlaced);
        // 1080p60 has a 88/44/148 horizontal split and 4/5/36 vertical split.
        assert_eq!(preferred.h_front_porch, 88);
        assert_eq!(preferred.h_sync, 44);
        assert_eq!(preferred.h_back_porch, Some(148));
        assert_eq!(preferred.h_total, Some(2200));
        assert_eq!(preferred.v_front_porch, 4);
        assert_eq!(preferred.v_sync, 5);
        assert_eq!(preferred.v_total, Some(1125));
        assert_eq!(preferred.v_back_porch, Some(36));
    }

    #[test]
    fn a_corrupt_block_is_rejected() {
        let mut bytes = edid();
        bytes[127] ^= 0x01;
        assert!(matches!(parse(&bytes), Err(Error::Checksum)));

        let short = &edid()[..64];
        assert!(matches!(parse(short), Err(Error::Checksum)));
    }
}
