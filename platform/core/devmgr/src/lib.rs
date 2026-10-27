//! Immutable PCI discovery snapshot. No resource delegation or DMA authority.
#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use horn_ipc::List;
use protocol::*;

pub mod firmware;
pub mod pci;
#[cfg(test)]
mod tests;

#[allow(dead_code, unused_imports, non_camel_case_types)]
pub mod protocol {
    include!(concat!(env!("OUT_DIR"), "/protocol.rs"));
}

/// Published only after a complete, bounded scan; IDs are (segment:BDF) + 1.
/// This is not a hotplug inventory: a new process starts a new snapshot.
pub struct Inventory {
    devices: Vec<PciDeviceInfo>,
}
impl Inventory {
    pub fn discover(
        config: &mut impl pci::ConfigRead,
        regions: &[firmware::EcamRegion],
        limit: usize,
    ) -> Result<Self, DeviceError> {
        firmware::validate_regions(regions)?;
        Ok(Self {
            devices: pci::scan(config, regions, limit)?,
        })
    }
}
impl DeviceObserver for Inventory {
    fn status(&mut self) -> Result<DeviceStatus, DeviceError> {
        Ok(DeviceStatus {
            phase: DevicePhase::DiscoveryReady,
            pci_devices: self.devices.len() as u32,
            iommu: IommuState::NotInitialized,
        })
    }
    fn list(&mut self, after: u64) -> Result<DevicePage, DeviceError> {
        if after > (1u64 << 32) {
            return Err(DeviceError::Invalid);
        }
        let mut entries = List::default();
        let mut remaining = self.devices.iter().filter(|d| d.device.id > after);
        for info in remaining.by_ref().take(4) {
            entries.data[entries.len as usize] = info.device.clone();
            entries.len += 1;
        }
        let next = if remaining.next().is_some() {
            entries.data[entries.len as usize - 1].id
        } else {
            0
        };
        Ok(DevicePage { entries, next })
    }
    fn info(&mut self, id: u64) -> Result<PciDeviceInfo, DeviceError> {
        self.devices
            .iter()
            .find(|d| d.device.id == id)
            .cloned()
            .ok_or(DeviceError::NotFound)
    }
}
