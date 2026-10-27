//! ACPI MCFG defines the only PCI configuration apertures we scan.
//! No BIOS scanning and no legacy CF8/CFC fallback.
use acpi::{AcpiHandler, AcpiTables, mcfg::Mcfg};
use alloc::vec::Vec;

use crate::protocol::DeviceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamRegion {
    pub base: u64,
    pub segment: u16,
    pub first_bus: u8,
    pub last_bus: u8,
}
impl EcamRegion {
    pub fn address(&self, bdf: u16, offset: u16) -> Result<u64, DeviceError> {
        let bus = (bdf >> 8) as u8;
        if !(self.first_bus..=self.last_bus).contains(&bus) || offset > 4092 || offset & 3 != 0 {
            return Err(DeviceError::Invalid);
        }
        // MCFG's base is relative to bus ZERO, even for a nonzero first_bus.
        // Do not use acpi 5.2's PciConfigRegions bus-minus-start calculation.
        self.base
            .checked_add(((bdf as u64) << 12) + offset as u64)
            .ok_or(DeviceError::Invalid)
    }
}

pub fn ecam_regions<H: AcpiHandler>(
    tables: &AcpiTables<H>,
) -> Result<Vec<EcamRegion>, DeviceError> {
    let mcfg = tables
        .find_table::<Mcfg>()
        .map_err(|_| DeviceError::Unsupported)?;
    let mut regions = Vec::new();
    if mcfg.entries().len() > 64 {
        return Err(DeviceError::Exhausted);
    }
    regions
        .try_reserve(mcfg.entries().len())
        .map_err(|_| DeviceError::Exhausted)?;
    for entry in mcfg.entries() {
        regions.push(EcamRegion {
            base: entry.base_address,
            segment: entry.pci_segment_group,
            first_bus: entry.bus_number_start,
            last_bus: entry.bus_number_end,
        });
    }
    regions.sort_unstable_by_key(|r| (r.segment, r.first_bus));
    validate_regions(&regions)?;
    Ok(regions)
}

pub(crate) fn validate_regions(regions: &[EcamRegion]) -> Result<(), DeviceError> {
    if regions.is_empty() || regions.len() > 64 {
        return Err(DeviceError::Invalid);
    }
    for (index, region) in regions.iter().enumerate() {
        if region.first_bus > region.last_bus || region.base & ((1 << 20) - 1) != 0 {
            return Err(DeviceError::Invalid);
        }
        let start = region
            .base
            .checked_add((region.first_bus as u64) << 20)
            .ok_or(DeviceError::Invalid)?;
        let end = region
            .base
            .checked_add(((region.last_bus as u64) + 1) << 20)
            .ok_or(DeviceError::Invalid)?;
        if index != 0
            && (regions[index - 1].segment, regions[index - 1].last_bus)
                >= (region.segment, region.first_bus)
        {
            return Err(DeviceError::Invalid);
        }
        for other in &regions[..index] {
            let other_start = other.base + ((other.first_bus as u64) << 20);
            let other_end = other.base + (((other.last_bus as u64) + 1) << 20);
            if start < other_end && other_start < end {
                return Err(DeviceError::Invalid);
            }
        }
    }
    Ok(())
}
