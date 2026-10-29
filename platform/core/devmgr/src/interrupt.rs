//! Conventional PCI message-interrupt capabilities. Never enables INTx.
use crate::protocol::DeviceError;
type Result<T> = core::result::Result<T, DeviceError>;

pub trait Config {
    fn read32(&mut self, offset: u16) -> u32;
    fn write(&mut self, offset: u16, width: u32, value: u32);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msi {
    pub offset: u16,
    wide: bool,
    maskable: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msix {
    pub offset: u16,
    pub bir: u32,
    pub table_offset: u32,
    pub entries: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub msi: Option<Msi>,
    pub msix: Option<Msix>,
}
impl Capabilities {
    pub fn discover(c: &mut impl Config) -> Result<Self> {
        let mut out = Self::default();
        if c.read32(4) & (1 << 20) == 0 {
            return Ok(out);
        }
        let mut at = (c.read32(0x34) & 255) as u16;
        let mut seen = 0u64;
        while at != 0 {
            if !(0x40..=0xfc).contains(&at) || at & 3 != 0 || seen & (1 << (at / 4)) != 0 {
                return Err(DeviceError::Invalid);
            }
            seen |= 1 << (at / 4);
            let word = c.read32(at);
            let control = word >> 16;
            match word & 255 {
                5 => {
                    let wide = control & 128 != 0;
                    let maskable = control & 256 != 0;
                    let size = if maskable {
                        if wide { 24 } else { 20 }
                    } else if wide {
                        14
                    } else {
                        10
                    };
                    if out.msi.is_some() || at as u32 + size > 256 || (control >> 1) & 7 > 5 {
                        return Err(DeviceError::Invalid);
                    }
                    out.msi = Some(Msi {
                        offset: at,
                        wide,
                        maskable,
                    });
                }
                0x11 => {
                    if out.msix.is_some() || at > 0xf4 {
                        return Err(DeviceError::Invalid);
                    }
                    let table = c.read32(at + 4);
                    out.msix = Some(Msix {
                        offset: at,
                        bir: table & 7,
                        table_offset: table & !7,
                        entries: (control & 2047) + 1,
                    });
                }
                _ => {}
            }
            at = ((word >> 8) & 255) as u16;
        }
        Ok(out)
    }
    /// Disable BOTH mechanisms, including whichever is not selected. Read back
    /// controls so posted ECAM writes complete before route removal/BAR sizing.
    pub fn disable(&self, c: &mut impl Config) {
        if let Some(m) = self.msi {
            m.disable(c);
        }
        if let Some(x) = self.msix {
            let control = c.read32(x.offset) >> 16;
            c.write(x.offset + 2, 2, (control & !0x8000) | 0x4000);
            let _ = c.read32(x.offset);
        }
    }
}
impl Msi {
    pub fn disable(self, c: &mut impl Config) {
        let control = c.read32(self.offset) >> 16;
        c.write(self.offset + 2, 2, control & !(1 | 0x70)); // MSI off, MME=0
        let _ = c.read32(self.offset);
    }
    fn mask_offset(self) -> u16 {
        self.offset + if self.wide { 16 } else { 12 }
    }
    /// Program disabled single-message MSI after the SID-checked IRTE exists.
    /// No CPU vector/address supplied by a client enters this method.
    pub fn prepare(self, c: &mut impl Config, address: u64, data: u32) -> Result<()> {
        if data > u16::MAX as u32 || (!self.wide && address > u32::MAX as u64) {
            return Err(DeviceError::Invalid);
        }
        self.disable(c);
        if self.maskable {
            let mask = c.read32(self.mask_offset());
            c.write(self.mask_offset(), 4, mask | 1);
        }
        c.write(self.offset + 4, 4, address as u32);
        if self.wide {
            c.write(self.offset + 8, 4, (address >> 32) as u32);
        }
        c.write(self.offset + if self.wide { 12 } else { 8 }, 2, data);
        let _ = c.read32(self.offset);
        Ok(())
    }
    /// Called only after the kernel IRQ is bound and armed.
    pub fn enable(self, c: &mut impl Config) {
        if self.maskable {
            let mask = c.read32(self.mask_offset());
            c.write(self.mask_offset(), 4, mask & !1);
        }
        let control = c.read32(self.offset) >> 16;
        c.write(self.offset + 2, 2, (control & !0x70) | 1);
        let _ = c.read32(self.offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    struct Bus {
        words: [u32; 64],
        writes: Vec<(u16, u32, u32)>,
    }
    impl Config for Bus {
        fn read32(&mut self, o: u16) -> u32 {
            self.words[o as usize / 4]
        }
        fn write(&mut self, o: u16, w: u32, v: u32) {
            self.writes.push((o, w, v));
            let shift = (o & 3) * 8;
            let mask = if w == 2 { 0xffff } else { u32::MAX };
            self.words[o as usize / 4] =
                (self.words[o as usize / 4] & !(mask << shift)) | ((v & mask) << shift);
        }
    }
    fn bus(control: u32) -> Bus {
        let mut b = Bus {
            words: [0; 64],
            writes: Vec::new(),
        };
        b.words[1] = 1 << 20;
        b.words[0x34 / 4] = 0x68;
        b.words[0x68 / 4] = (control << 16) | 5;
        b
    }
    #[test]
    fn single_64bit_unmaskable_msi_uses_word_data_and_clears_mme() {
        let mut b = bus(128 | 1 | 0x30 | 6); // MMC=3, old MME=3
        let m = Capabilities::discover(&mut b).unwrap().msi.unwrap();
        m.prepare(&mut b, 0xfee0_0050, 0).unwrap();
        assert_eq!(
            b.writes,
            [
                (0x6a, 2, 128 | 6),
                (0x6c, 4, 0xfee0_0050),
                (0x70, 4, 0),
                (0x74, 2, 0)
            ]
        );
        m.enable(&mut b);
        assert_eq!(b.writes.last(), Some(&(0x6a, 2, 128 | 6 | 1)));
        assert_eq!(b.words[1], 1 << 20); // no W1C status/command DWORD write
        m.disable(&mut b);
        assert_eq!(b.words[0x68 / 4] >> 16, 128 | 6);
    }
    #[test]
    fn maskable_32bit_msi_preserves_other_mask_bits() {
        let mut b = bus(256);
        b.words[0x74 / 4] = 0xa;
        let m = Capabilities::discover(&mut b).unwrap().msi.unwrap();
        m.prepare(&mut b, 0xfee0_0030, 0).unwrap();
        assert_eq!(b.words[0x74 / 4], 0xb);
        m.enable(&mut b);
        assert_eq!(b.words[0x74 / 4], 0xa);
        assert!(b.writes.contains(&(0x70, 2, 0)));
        let before = b.writes.len();
        assert_eq!(m.prepare(&mut b, 1 << 32, 0), Err(DeviceError::Invalid));
        assert_eq!(b.writes.len(), before);
    }
    #[test]
    fn both_capabilities_disabled_and_no_intx_fallback() {
        let mut b = bus(129);
        b.words[0x68 / 4] |= 0x80 << 8;
        b.words[0x80 / 4] = 0x80000011;
        b.words[0x84 / 4] = 0x2004;
        let caps = Capabilities::discover(&mut b).unwrap();
        assert_eq!(caps.msix.unwrap().bir, 4);
        caps.disable(&mut b);
        assert_eq!(b.words[0x80 / 4] >> 16, 0x4000);
        assert_eq!(b.words[0x68 / 4] >> 16, 128);
        let mut empty = bus(0);
        empty.words[0x34 / 4] = 0;
        assert_eq!(
            Capabilities::discover(&mut empty).unwrap(),
            Capabilities::default()
        );
    }
    #[test]
    fn malformed_chains_and_truncated_payloads_fail_closed() {
        let mut b = bus(128);
        b.words[0x68 / 4] |= 0x68 << 8;
        assert_eq!(Capabilities::discover(&mut b), Err(DeviceError::Invalid));
        let mut b = bus(0);
        b.words[0x34 / 4] = 0xfc;
        b.words[0xfc / 4] = 0x800005;
        assert_eq!(Capabilities::discover(&mut b), Err(DeviceError::Invalid));
    }
}
