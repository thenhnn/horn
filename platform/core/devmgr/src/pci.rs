//! Read-only PCIe ECAM discovery. No config writes, BAR sizing or INTx setup.
use alloc::vec::Vec;
use horn_ipc::List;

use crate::{firmware::EcamRegion, protocol::*};

pub trait ConfigRead {
    fn read(&mut self, physical_address: u64) -> Result<u32, DeviceError>;
}

pub(crate) fn scan(
    config: &mut impl ConfigRead,
    regions: &[EcamRegion],
    limit: usize,
) -> Result<Vec<PciDeviceInfo>, DeviceError> {
    if limit > 256 {
        return Err(DeviceError::Invalid);
    }
    let mut devices = Vec::new();
    for region in regions {
        for bus in region.first_bus..=region.last_bus {
            for device in 0..32 {
                let bdf = ((bus as u16) << 8) | (device << 3);
                let Some(first) = read_device(config, region, bdf)? else {
                    continue;
                };
                let multifunction = first.device.header_type & 0x80 != 0;
                append(&mut devices, first, limit)?;
                if multifunction {
                    for function in 1..8 {
                        if let Some(info) = read_device(config, region, bdf | function)? {
                            append(&mut devices, info, limit)?;
                        }
                    }
                }
            }
        }
    }
    Ok(devices)
}

fn append(
    devices: &mut Vec<PciDeviceInfo>,
    info: PciDeviceInfo,
    limit: usize,
) -> Result<(), DeviceError> {
    if devices.len() == limit {
        return Err(DeviceError::Exhausted);
    }
    devices.try_reserve(1).map_err(|_| DeviceError::Exhausted)?;
    devices.push(info);
    Ok(())
}

fn read_device(
    config: &mut impl ConfigRead,
    region: &EcamRegion,
    bdf: u16,
) -> Result<Option<PciDeviceInfo>, DeviceError> {
    let base = region.address(bdf, 0)?;
    let identity = config.read(base)?;
    if identity & 0xffff == 0xffff {
        return Ok(None);
    }
    let mut read = |offset: u16| {
        if offset > 4092 || offset & 3 != 0 {
            return Err(DeviceError::Backend);
        }
        config.read(base + offset as u64)
    };
    let class = read(8)?;
    let header_type = (read(12)? >> 16) & 0xff;
    let command_status = read(4)?;
    let interrupt = read(0x3c)?;
    let bar_count = match header_type & 0x7f {
        0 => 6,
        1 => 2,
        _ => 0,
    };
    let mut bars = List::default();
    let mut index = 0;
    while index < bar_count {
        let low = read(0x10 + index * 4)?;
        if low == 0 || low == u32::MAX {
            index += 1;
            continue;
        }
        let mut bar = PciBar {
            index: index as u32,
            kind: PciBarKind::Io,
            address: (low & !3) as u64,
            prefetchable: false,
        };
        if low & 1 == 0 {
            bar.prefetchable = low & 8 != 0;
            bar.address = (low & !15) as u64;
            bar.kind = match (low >> 1) & 3 {
                0 => PciBarKind::Memory32,
                1 => PciBarKind::MemoryBelow1M,
                2 => {
                    if index + 1 >= bar_count {
                        return Err(DeviceError::Backend);
                    }
                    bar.address |= (read(0x14 + index * 4)? as u64) << 32;
                    index += 1;
                    PciBarKind::Memory64
                }
                _ => return Err(DeviceError::Backend),
            };
        }
        bars.data[bars.len as usize] = bar;
        bars.len += 1;
        index += 1;
    }
    let mut info = PciDeviceInfo {
        device: PciDevice {
            id: (((region.segment as u64) << 16) | bdf as u64) + 1,
            address: PciAddress {
                segment: region.segment as u32,
                bus: (bdf >> 8) as u32,
                device: ((bdf >> 3) & 31) as u32,
                function: (bdf & 7) as u32,
            },
            vendor_id: identity & 0xffff,
            device_id: identity >> 16,
            class_code: class >> 24,
            subclass: (class >> 16) & 0xff,
            programming_interface: (class >> 8) & 0xff,
            revision: class & 0xff,
            header_type,
        },
        command: command_status & 0xffff,
        interrupt_line: interrupt & 0xff,
        interrupt_pin: (interrupt >> 8) & 0xff,
        bars,
        msi: false,
        msix: false,
        resizable_bars: List::default(),
    };
    if command_status & (1 << 20) != 0 && bar_count != 0 {
        let mut offset = (read(0x34)? & 0xff) as u16;
        let mut seen = 0u64;
        let mut pcie = false;
        while offset != 0 {
            if !(0x40..=0xfc).contains(&offset)
                || offset & 3 != 0
                || seen & (1 << (offset / 4)) != 0
            {
                return Err(DeviceError::Backend);
            }
            seen |= 1 << (offset / 4);
            let cap = read(offset)?;
            match cap & 0xff {
                0x05 => info.msi = true,
                0x11 => info.msix = true,
                0x10 => pcie = true,
                _ => (),
            }
            offset = ((cap >> 8) & 0xff) as u16;
        }
        if pcie {
            extended_caps(&mut read, &mut info)?;
        }
    }
    Ok(Some(info))
}

fn extended_caps(
    read: &mut impl FnMut(u16) -> Result<u32, DeviceError>,
    info: &mut PciDeviceInfo,
) -> Result<(), DeviceError> {
    let mut offset = 0x100u16;
    let mut seen = [0u64; 16];
    while offset != 0 {
        let word = offset as usize / 4;
        if !(0x100..=0xffc).contains(&offset)
            || offset & 3 != 0
            || seen[word / 64] & (1 << (word % 64)) != 0
        {
            return Err(DeviceError::Backend);
        }
        seen[word / 64] |= 1 << (word % 64);
        let header = read(offset)?;
        if header == 0 || header == u32::MAX {
            break;
        }
        // PCIe Resizable BAR v1. Unknown extended capabilities are skipped.
        if header & 0xffff == 0x15 && (header >> 16) & 0xf == 1 {
            if info.resizable_bars.len != 0 || offset > 0xff4 {
                return Err(DeviceError::Backend);
            }
            let count = (read(offset + 8)? >> 5) & 7;
            if count == 0 || count > 6 || offset as u32 + 4 + count * 8 > 4096 {
                return Err(DeviceError::Backend);
            }
            let mut indices = 0u8;
            for entry in 0..count as u16 {
                let supported = (read(offset + 4 + entry * 8)? >> 4) as u64;
                let control = read(offset + 8 + entry * 8)?;
                let index = control & 7;
                let size = (control >> 8) & 0x1f;
                let bar_count = if info.device.header_type & 0x7f == 0 {
                    6
                } else {
                    2
                };
                if index >= bar_count || indices & (1 << index) != 0 || supported & (1 << size) == 0
                {
                    return Err(DeviceError::Backend);
                }
                let bars = &info.bars.data[..info.bars.len as usize];
                // Do not mistake the upper half of a 64-bit BAR for a BAR.
                if bars
                    .iter()
                    .any(|b| b.kind == PciBarKind::Memory64 && b.index + 1 == index)
                {
                    return Err(DeviceError::Backend);
                }
                let memory_bar = match bars.iter().find(|b| b.index == index) {
                    Some(bar) => matches!(bar.kind, PciBarKind::Memory32 | PciBarKind::Memory64),
                    // A zero 32-bit BAR can be implemented but not assigned.
                    // ReBAR metadata identifies it without write-based sizing.
                    None => read(0x10 + index as u16 * 4)? == 0,
                };
                if !memory_bar {
                    return Err(DeviceError::Backend);
                }
                indices |= 1 << index;
                info.resizable_bars.data[entry as usize] = PciResizableBar {
                    index,
                    supported_sizes: supported,
                    current_size: 1u64 << (20 + size),
                };
                info.resizable_bars.len += 1;
            }
        }
        offset = (header >> 20) as u16;
    }
    Ok(())
}
