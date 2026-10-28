//! Small fail-closed DMAR profile: one segment-0 DRHD, root-bus endpoints.
//! RMRRs, multiple units and PCI bridge assignment need separate support.
use crate::protocol::DeviceError;
use acpi::{
    AcpiHandler, AcpiTable, AcpiTables,
    sdt::{SdtHeader, Signature},
};

#[repr(C, packed)]
struct Header {
    header: SdtHeader,
    host_width: u8,
    flags: u8,
    reserved: [u8; 10],
}
unsafe impl AcpiTable for Header {
    const SIGNATURE: Signature = Signature::DMAR;
    fn header(&self) -> &SdtHeader {
        &self.header
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dmar {
    pub registers: u64,
    pub host_width: u8,
    pub requesters: [u64; 4],
}
impl Dmar {
    pub fn discover<H: AcpiHandler>(tables: &AcpiTables<H>) -> Result<Self, DeviceError> {
        let table = tables
            .find_table::<Header>()
            .map_err(|_| DeviceError::Unsupported)?;
        let bytes = unsafe {
            core::slice::from_raw_parts(
                table.virtual_start().as_ptr().cast::<u8>(),
                table.region_length(),
            )
        };
        Self::parse(bytes)
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, DeviceError> {
        if bytes.len() < 48
            || &bytes[..4] != b"DMAR"
            || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize != bytes.len()
            || bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) != 0
        {
            return Err(DeviceError::Invalid);
        }
        let host_width = bytes[36].checked_add(1).ok_or(DeviceError::Invalid)?;
        if !(32..=52).contains(&host_width) || bytes[37] & 1 == 0 {
            return Err(DeviceError::Unsupported);
        }
        let mut at = 48;
        let mut registers = None;
        let mut requesters = [0u64; 4];
        while at < bytes.len() {
            let header = bytes.get(at..at + 4).ok_or(DeviceError::Invalid)?;
            let kind = u16::from_le_bytes(header[..2].try_into().unwrap());
            let length = u16::from_le_bytes(header[2..].try_into().unwrap()) as usize;
            if length < 4 {
                return Err(DeviceError::Invalid);
            }
            let entry = bytes.get(at..at + length).ok_or(DeviceError::Invalid)?;
            at += length;
            // In particular, never ignore a firmware RMRR and then free its RAM.
            if kind != 0 || registers.is_some() {
                return Err(DeviceError::Unsupported);
            }
            if length < 16 {
                return Err(DeviceError::Invalid);
            }
            if entry[4] & !1 != 0 || entry[6..8] != [0, 0] {
                return Err(DeviceError::Unsupported);
            }
            let base = u64::from_le_bytes(entry[8..16].try_into().unwrap());
            if base == 0
                || base & 4095 != 0
                || base
                    .checked_add(65536)
                    .is_none_or(|end| end > (1u64 << host_width))
            {
                return Err(DeviceError::Invalid);
            }
            let include_all = entry[4] & 1 != 0;
            if include_all {
                requesters.fill(u64::MAX);
            }
            let mut scopes = &entry[16..];
            while !scopes.is_empty() {
                if scopes.len() < 2 {
                    return Err(DeviceError::Invalid);
                }
                let length = scopes[1] as usize;
                if length < 8 || length % 2 != 0 || length > scopes.len() {
                    return Err(DeviceError::Invalid);
                }
                let scope = &scopes[..length];
                if length != 8 || scope[6] > 31 || scope[7] > 7 {
                    return Err(DeviceError::Unsupported);
                }
                match scope[0] {
                    1 if scope[5] == 0 => {
                        let sid = scope[6] as usize * 8 + scope[7] as usize;
                        requesters[sid / 64] |= 1 << (sid % 64);
                    }
                    // Interrupt sources do not grant any DMA authority.
                    3 | 4 => {}
                    _ => return Err(DeviceError::Unsupported),
                }
                scopes = &scopes[length..];
            }
            registers = Some(base);
        }
        if requesters.iter().all(|mask| *mask == 0) {
            return Err(DeviceError::Unsupported);
        }
        Ok(Self {
            registers: registers.ok_or(DeviceError::Unsupported)?,
            host_width,
            requesters,
        })
    }
}
