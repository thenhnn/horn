//! Upstream PCI transport sizes BARs by writing configuration space. Drivers
//! cannot do that to hardware: devmgr owns PCI configuration. Emulate those
//! probes against an immutable snapshot plus devmgr-validated BAR lengths.
use virtio_drivers::transport::pci::bus::{ConfigurationAccess, DeviceFunction};

#[derive(Clone)]
pub struct ConfigSnapshot {
    function: DeviceFunction,
    words: [u32; 64],
    sizes: [u64; 6],
    probing: [bool; 6],
}
impl ConfigSnapshot {
    pub fn new(function: DeviceFunction, words: [u32; 64], sizes: [u64; 6]) -> Self {
        assert!(function.valid());
        assert!(
            sizes
                .iter()
                .all(|size| *size == 0 || size.is_power_of_two())
        );
        Self {
            function,
            words,
            sizes,
            probing: [false; 6],
        }
    }
}
impl ConfigurationAccess for ConfigSnapshot {
    fn read_word(&self, function: DeviceFunction, offset: u8) -> u32 {
        if function != self.function || offset & 3 != 0 {
            return u32::MAX;
        }
        if (0x10..0x28).contains(&offset) {
            let bar = (offset as usize - 0x10) / 4;
            if self.probing[bar] {
                let mut base = 0;
                while base < bar {
                    if self.words[4 + base] & 7 == 4 {
                        if base + 1 == bar {
                            break;
                        }
                        base += 2;
                    } else {
                        base += 1;
                    }
                }
                let upper = base != bar;
                let size = self.sizes[base];
                if size == 0 {
                    return 0;
                }
                let mask = !(size - 1);
                return if upper {
                    (mask >> 32) as u32
                } else {
                    mask as u32 | (self.words[4 + bar] & 15)
                };
            }
        }
        self.words[offset as usize / 4]
    }
    fn write_word(&mut self, function: DeviceFunction, offset: u8, data: u32) {
        assert_eq!(function, self.function);
        assert_eq!(offset & 3, 0);
        if (0x10..0x28).contains(&offset) {
            let bar = (offset as usize - 0x10) / 4;
            // BAR relocation is not an operation on this adapter. Only probes
            // and restoring the original descriptor are accepted.
            assert!(data == u32::MAX || data == self.words[offset as usize / 4]);
            self.probing[bar] = data == u32::MAX;
        } else {
            // PciRoot::bar_info temporarily clears address decoding. Its writes
            // affect ONLY this shadow, never the real PCI command/status bits.
            assert_eq!(offset, 4);
            self.words[1] = data;
        }
    }
    unsafe fn unsafe_clone(&self) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use virtio_drivers::transport::pci::bus::PciRoot;
    #[test]
    fn upstream_bar_probes_use_shadow_configuration_only() {
        let device = DeviceFunction {
            bus: 0,
            device: 4,
            function: 0,
        };
        let mut words = [0; 64];
        words[1] = 0x0010_0006;
        words[4] = 0x8000_0004;
        words[5] = 4;
        words[6] = 0xe000_0000;
        words[8] = 0xf000_0000;
        let snapshot = ConfigSnapshot::new(device, words, [16384, 0, 8192, 0, 4096, 0]);
        let mut root = PciRoot::new(snapshot.clone());
        assert_eq!(
            root.bar_info(device, 0)
                .unwrap()
                .unwrap()
                .memory_address_size(),
            Some((0x4_8000_0000, 16384))
        );
        assert_eq!(
            root.bar_info(device, 4)
                .unwrap()
                .unwrap()
                .memory_address_size(),
            Some((0xf000_0000, 4096))
        );
        assert_eq!(
            root.bar_info(device, 2)
                .unwrap()
                .unwrap()
                .memory_address_size(),
            Some((0xe000_0000, 8192))
        );
        assert_eq!(root.bar_info(device, 3).unwrap(), None);
        assert_eq!(root.get_status_command(device).1.bits(), 6);
        assert_eq!(snapshot.read_word(device, 0x10), words[4]);
        assert_eq!(
            snapshot.read_word(
                DeviceFunction {
                    device: 5,
                    ..device
                },
                0
            ),
            u32::MAX
        );
    }
}
