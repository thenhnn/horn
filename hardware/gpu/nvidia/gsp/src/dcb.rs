// SPDX-License-Identifier: GPL-2.0-only
//! Display control block parsing: which connectors exist and which I2C or
//! AUX channel carries their DDC traffic.
//!
//! Field layout follows the DCB tables as nouveau reads them (`nvkm/subdev/bios/
//! dcb.c`, `i2c.c`). Nothing here touches hardware: it only reads the VBIOS
//! image we already snapshot for FWSEC.

use crate::{Error, Result};

/// Offset of the DCB pointer inside the ROM image.
const DCB_POINTER: usize = 0x36;
/// `0x4edcbdcb`, the DCB signature.
const DCB_SIGNATURE: u32 = 0x4edc_bdcb;

/// Connector kinds we care about.
pub const OUTPUT_ANALOG: u8 = 0x0;
pub const OUTPUT_TMDS: u8 = 0x2;
pub const OUTPUT_LVDS: u8 = 0x3;
pub const OUTPUT_DP: u8 = 0x6;
pub const OUTPUT_UNUSED: u8 = 0xf;

/// I2C transport kinds.
pub const I2C_UNUSED: u8 = 0xff;
pub const I2C_NV04_BIT: u8 = 0x00;
pub const I2C_NV4E_BIT: u8 = 0x04;
pub const I2C_NVIO_BIT: u8 = 0x05;
pub const I2C_NVIO_AUX: u8 = 0x06;
/// DCB 4.1+ packs an I2C port and an AUX channel into one entry.
pub const I2C_PMGR: u8 = 0x80;

/// One `dcb_i2c_entry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2cEntry {
    pub kind: u8,
    /// Bit-banged SCL pad, or the I2C port number for `I2C_PMGR`.
    pub drive: u8,
    /// Bit-banged SDA pad.
    pub sense: u8,
    /// AUX channel number, when the connector is DisplayPort.
    pub auxch: u8,
}

impl I2cEntry {
    pub const UNUSED: Self = Self {
        kind: I2C_UNUSED,
        drive: I2C_UNUSED,
        sense: I2C_UNUSED,
        auxch: I2C_UNUSED,
    };
    /// DDC on this entry runs over DisplayPort AUX rather than plain I2C.
    pub fn is_aux(&self) -> bool {
        self.auxch != I2C_UNUSED && (self.kind == I2C_NVIO_AUX || self.kind == I2C_PMGR)
    }
}

/// One `dcb_output` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    pub index: u8,
    pub kind: u8,
    pub location: u8,
    pub i2c_index: u8,
    pub connector: u8,
    pub heads: u8,
    pub or: u8,
    pub dp_rate: u8,
    pub dp_lanes: u8,
}

/// Parsed DCB table.
#[derive(Clone, Debug)]
pub struct Dcb {
    pub version: u8,
    pub header: u8,
    pub count: u8,
    pub len: u8,
    pub i2c_version: u8,
    pub outputs: alloc::vec::Vec<Output>,
    pub i2c: alloc::vec::Vec<I2cEntry>,
}

fn rd8(image: &[u8], at: usize) -> Result<u8> {
    image.get(at).copied().ok_or(Error::Range)
}

fn rd16(image: &[u8], at: usize) -> Result<u16> {
    let bytes = image.get(at..at + 2).ok_or(Error::Range)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn rd32(image: &[u8], at: usize) -> Result<u32> {
    let bytes = image.get(at..at + 4).ok_or(Error::Range)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Parse the DCB out of a PROM snapshot. A card without a usable DCB is not an
/// error we can paper over: without it there is no DDC route to a connector.
pub fn parse(image: &[u8]) -> Result<Dcb> {
    let dcb = rd16(image, DCB_POINTER)? as usize;
    if dcb == 0 {
        return Err(Error::Unsupported);
    }
    let version = rd8(image, dcb)?;
    // Only the 3.x layout carries a self-describing table; older versions put
    // the same fields at fixed offsets and predate every card we support.
    if !(0x30..0x42).contains(&version) {
        return Err(Error::Unsupported);
    }
    if rd32(image, dcb + 6)? != DCB_SIGNATURE {
        return Err(Error::Malformed);
    }
    let header = rd8(image, dcb + 1)?;
    let count = rd8(image, dcb + 2)?;
    let len = rd8(image, dcb + 3)?;
    // The signature occupies +6..+10, so a header shorter than that would put
    // the first output entry on top of it.
    if header < 10 || len < 8 || count == 0 {
        return Err(Error::Malformed);
    }

    let mut outputs = alloc::vec::Vec::new();
    outputs
        .try_reserve_exact(count as usize)
        .map_err(|_| Error::Exhausted)?;
    for index in 0..count {
        let at = dcb + header as usize + index as usize * len as usize;
        let conn = rd32(image, at)?;
        let conf = rd32(image, at + 4)?;
        let dp = version >= 0x40 && conn & 15 == u32::from(OUTPUT_DP);
        let output = Output {
            index,
            kind: (conn & 0x0000_000f) as u8,
            location: ((conn & 0x0030_0000) >> 20) as u8,
            i2c_index: ((conn & 0x0000_00f0) >> 4) as u8,
            connector: ((conn & 0x0000_f000) >> 12) as u8,
            heads: ((conn & 0x0000_0f00) >> 8) as u8,
            or: ((conn & 0x0f00_0000) >> 24) as u8,
            // Nouveau nvbios dcb_outp_parse, DCB 4.x DP configuration.
            dp_rate: if dp {
                match (conf >> 21) & 7 {
                    0 => 6,
                    1 => 10,
                    2 => 20,
                    3 => 30,
                    _ => 0,
                }
            } else {
                0
            },
            dp_lanes: if dp {
                match (conf >> 24) & 15 {
                    15 | 4 => 4,
                    3 | 2 => 2,
                    1 => 1,
                    _ => 0,
                }
            } else {
                0
            },
        };
        outputs.push(output);
    }

    // The I2C table pointer moved between DCB revisions.
    let i2c = rd16(image, dcb + 4)? as usize;
    let mut i2c_version = version;
    let (i2c_header, i2c_count, i2c_len) = if i2c == 0 {
        return Ok(Dcb {
            version,
            header,
            count,
            len,
            i2c_version,
            outputs,
            i2c: alloc::vec::Vec::new(),
        });
    } else {
        i2c_version = rd8(image, i2c)?;
        (
            rd8(image, i2c + 1)? as usize,
            rd8(image, i2c + 2)?,
            rd8(image, i2c + 3)? as usize,
        )
    };
    if i2c_len < 4 || i2c_count == 0 {
        return Err(Error::Malformed);
    }

    let mut entries = alloc::vec::Vec::new();
    entries
        .try_reserve_exact(i2c_count as usize)
        .map_err(|_| Error::Exhausted)?;
    for index in 0..i2c_count {
        let at = i2c + i2c_header + index as usize * i2c_len;
        entries.push(parse_i2c_entry(image, at, i2c_version)?);
    }

    Ok(Dcb {
        version,
        header,
        count,
        len,
        i2c_version,
        outputs,
        i2c: entries,
    })
}

fn parse_i2c_entry(image: &[u8], at: usize, version: u8) -> Result<I2cEntry> {
    if version >= 0x41 {
        // One word packs both transports; 0x1f means "not wired".
        let value = rd32(image, at)?;
        let port = (value & 0x1f) as u8;
        let aux = ((value >> 5) & 0x1f) as u8;
        if port == 0x1f && aux == 0x1f {
            return Ok(I2cEntry::UNUSED);
        }
        // 0x1f means "this transport is not wired" and is normalised to the
        // sentinel the rest of the driver tests against.
        return Ok(I2cEntry {
            kind: I2C_PMGR,
            drive: if port == 0x1f { I2C_UNUSED } else { port },
            sense: I2C_UNUSED,
            auxch: if aux == 0x1f { I2C_UNUSED } else { aux },
        });
    }
    let kind = rd8(image, at + 3)?;
    Ok(match kind {
        I2C_NV04_BIT => I2cEntry {
            kind,
            drive: rd8(image, at)?,
            sense: rd8(image, at + 1)?,
            auxch: I2C_UNUSED,
        },
        I2C_NV4E_BIT => I2cEntry {
            kind,
            drive: rd8(image, at + 1)?,
            sense: I2C_UNUSED,
            auxch: I2C_UNUSED,
        },
        I2C_NVIO_BIT => I2cEntry {
            kind,
            drive: rd8(image, at)? & 0x0f,
            sense: I2C_UNUSED,
            auxch: I2C_UNUSED,
        },
        I2C_NVIO_AUX => I2cEntry {
            kind,
            drive: I2C_UNUSED,
            sense: I2C_UNUSED,
            auxch: rd8(image, at)? & 0x0f,
        },
        _ => I2cEntry::UNUSED,
    })
}

impl Dcb {
    /// Connectors that can carry a monitor, with their DDC route.
    pub fn connectors(&self) -> impl Iterator<Item = (Output, I2cEntry)> + '_ {
        self.outputs
            .iter()
            .filter(|output| {
                matches!(
                    output.kind,
                    OUTPUT_TMDS | OUTPUT_DP | OUTPUT_ANALOG | OUTPUT_LVDS
                )
            })
            .map(|output| {
                let entry = self
                    .i2c
                    .get(output.i2c_index as usize)
                    .copied()
                    .unwrap_or(I2cEntry::UNUSED);
                (*output, entry)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};

    /// Build a minimal DCB 4.1 image: one DisplayPort connector on AUX channel 2
    /// and one HDMI connector on I2C port 4.
    fn image() -> Vec<u8> {
        let mut rom = vec![0u8; 1024];
        let dcb = 0x100usize;
        rom[DCB_POINTER..DCB_POINTER + 2].copy_from_slice(&(dcb as u16).to_le_bytes());
        rom[dcb] = 0x41; // version
        rom[dcb + 1] = 10; // header: four fixed words plus the signature
        rom[dcb + 2] = 2; // two outputs
        rom[dcb + 3] = 8; // output entry length
        let i2c = 0x180usize;
        rom[dcb + 4..dcb + 6].copy_from_slice(&(i2c as u16).to_le_bytes());
        rom[dcb + 6..dcb + 10].copy_from_slice(&DCB_SIGNATURE.to_le_bytes());
        // Output 0: DP on i2c index 0, one head.
        let conn = (1 << 8) | OUTPUT_DP as u32;
        rom[dcb + 10..dcb + 14].copy_from_slice(&conn.to_le_bytes());
        // Output 1: TMDS (HDMI) on i2c index 1, one head.
        let conn = (1 << 8) | (1 << 4) | OUTPUT_TMDS as u32;
        rom[dcb + 18..dcb + 22].copy_from_slice(&conn.to_le_bytes());
        // I2C table: two entries, 4 bytes each.
        rom[i2c] = 0x41;
        rom[i2c + 1] = 4;
        rom[i2c + 2] = 2;
        rom[i2c + 3] = 4;
        // Entry 0: I2C unused (0x1f), AUX channel 2.
        let value = 0x1fu32 | (2 << 5);
        rom[i2c + 4..i2c + 8].copy_from_slice(&value.to_le_bytes());
        // Entry 1: I2C port 4, AUX unused (0x1f).
        let value = 4u32 | (0x1f << 5);
        rom[i2c + 8..i2c + 12].copy_from_slice(&value.to_le_bytes());
        rom
    }

    #[test]
    fn connectors_carry_their_ddc_route() {
        let dcb = parse(&image()).unwrap();
        assert_eq!(dcb.version, 0x41);
        assert_eq!(dcb.outputs.len(), 2);
        let connectors: Vec<_> = dcb.connectors().collect();
        assert_eq!(connectors.len(), 2);

        let (dp, dp_ddc) = connectors[0];
        assert_eq!(dp.kind, OUTPUT_DP);
        assert!(dp_ddc.is_aux());
        assert_eq!(dp_ddc.auxch, 2);
        assert_eq!(dp_ddc.drive, I2C_UNUSED);

        let (hdmi, hdmi_ddc) = connectors[1];
        assert_eq!(hdmi.kind, OUTPUT_TMDS);
        assert!(!hdmi_ddc.is_aux());
        assert_eq!(hdmi_ddc.drive, 4);
    }

    #[test]
    fn a_missing_or_bad_table_is_rejected() {
        let mut rom = image();
        rom[DCB_POINTER..DCB_POINTER + 2].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(parse(&rom), Err(Error::Unsupported)));

        let mut rom = image();
        rom[0x100 + 6] ^= 0xff;
        assert!(matches!(parse(&rom), Err(Error::Malformed)));

        // An unsupported (pre-3.x) layout is refused instead of misread.
        let mut rom = image();
        rom[0x100] = 0x20;
        assert!(matches!(parse(&rom), Err(Error::Unsupported)));
    }
}
