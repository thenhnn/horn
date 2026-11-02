// SPDX-License-Identifier: GPL-2.0-only
//! Display bring-up step one: find the connector, reach the monitor, read its
//! EDID.
//!
//! The VBIOS DCB says which connector exists and which AUX channel or I2C port
//! carries its DDC traffic; the AUX channel and the EDID decoder are in the GSP
//! crate. Nothing here programs a mode yet: this establishes what the monitor is
//! and what it can display.

use horn_nvidia_gsp::i2c::Registers;
use horn_nvidia_gsp::{Error, Result, aux, dcb, edid};

use crate::native::Native;

/// What the probe learned, for the boot log.
pub struct Report {
    pub connector: u8,
    pub dcb_index: u8,
    pub heads: u8,
    pub dp_rate: u8,
    pub dp_lanes: u8,
    pub kind: u8,
    pub aux_channel: u8,
    pub pad: u8,
    pub dpcd: [u8; 16],
    pub monitor: edid::Monitor,
}

/// Implement the register view the AUX channel needs on top of the driver's
/// BAR0 access. Window caching, bounds checks and leases stay in `Native`.
impl Registers for Native {
    fn read32(&mut self, offset: u32) -> Result<u32> {
        horn_nvidia_gsp::falcon::Platform::read32(self, offset)
    }
    fn write32(&mut self, offset: u32, value: u32) -> Result<()> {
        horn_nvidia_gsp::falcon::Platform::write32(self, offset, value)
    }
    fn delay_us(&mut self, us: u32) -> Result<()> {
        // Real waiting, not a spin: an AUX transaction is measured in
        // milliseconds and a tight loop finishes far too early.
        let now = horn_nvidia_gsp::falcon::Platform::now_us(self)?;
        let deadline = now.checked_add(u64::from(us)).ok_or(Error::Range)?;
        horn_nvidia_gsp::falcon::Platform::sleep_until(self, deadline)
    }
}

/// Probe every DisplayPort connector that has an AUX route until one answers.
///
/// A card lists more connectors than are wired, and an unplugged one simply
/// does not acknowledge, so "first DP entry in the DCB" is not the same as "the
/// monitor". The VBIOS bytes are the ones captured before GSP boot: once the
/// firmware owns the GPU the ROM window is no longer a reliable source.
pub fn probe(native: &mut Native, vbios: &[u8]) -> Result<Report> {
    let table = dcb::parse(vbios)?;
    let mut candidates = 0usize;
    let mut first_error = None;
    for (output, route) in table.connectors() {
        if output.kind != dcb::OUTPUT_DP || !route.is_aux() {
            continue;
        }
        candidates += 1;
        let pad = output.i2c_index;
        let aux_channel = route.auxch;
        sel4::debug_println!(
            "NVIDIA_DISPLAY_STEP name=try connector={} aux={} pad={} or={}",
            output.connector,
            aux_channel,
            pad,
            output.or
        );
        match attempt(native, pad, aux_channel) {
            Ok((dpcd, monitor)) => {
                return Ok(Report {
                    connector: output.connector,
                    dcb_index: output.index,
                    heads: output.heads,
                    dp_rate: output.dp_rate,
                    dp_lanes: output.dp_lanes,
                    kind: output.kind,
                    aux_channel,
                    pad,
                    dpcd,
                    monitor,
                });
            }
            Err(error) => {
                sel4::debug_println!(
                    "NVIDIA_DISPLAY_STEP name=skip connector={} error={:?}",
                    output.connector,
                    error
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if candidates == 0 {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_STEP name=dcb outputs={}",
            table.outputs.len()
        );
        return Err(Error::Unsupported);
    }
    Err(first_error.unwrap_or(Error::Unavailable))
}

/// One connector: switch its pad to AUX, confirm the link with a DPCD read, then
/// fetch and decode the EDID.
fn attempt(native: &mut Native, pad: u8, aux_channel: u8) -> Result<([u8; 16], edid::Monitor)> {
    let mut aux = aux::Aux::new(native, pad, aux_channel);
    aux.pad_aux()?;
    let mut dpcd = [0u8; 16];
    if let Err(error) = aux.read_dpcd(aux::DPCD_REV, &mut dpcd) {
        sel4::debug_println!(
            "NVIDIA_DISPLAY_STEP name=dpcd error={:?} stat={:#x}",
            error,
            aux.last_stat
        );
        return Err(error);
    }
    if dpcd[0] == 0 || dpcd[0] == 0xff {
        sel4::debug_println!("NVIDIA_DISPLAY_STEP name=dpcd invalid={:#x}", dpcd[0]);
        return Err(Error::Unavailable);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_STEP name=dpcd revision={}.{} lanes={:#x} raw={:02x?}",
        dpcd[0] >> 4,
        dpcd[0] & 0xf,
        dpcd[2],
        dpcd
    );
    let base = match aux.read_edid(1) {
        Ok(bytes) => bytes,
        Err(error) => {
            sel4::debug_println!(
                "NVIDIA_DISPLAY_STEP name=edid error={:?} stat={:#x}",
                error,
                aux.last_stat
            );
            return Err(error);
        }
    };
    if base.len() != 128 {
        return Err(Error::Malformed);
    }
    sel4::debug_println!(
        "NVIDIA_DISPLAY_STEP name=edid bytes={} extensions={}",
        base.len(),
        base[126]
    );
    let extensions = usize::from(base[126]);
    let bytes = if extensions == 0 {
        base
    } else {
        // The base block states how many extension blocks follow; read exactly
        // that many so the decoder sees a complete blob.
        aux.read_edid(1 + extensions.min(7))?
    };
    Ok((dpcd, edid::parse(&bytes)?))
}
