// SPDX-License-Identifier: GPL-2.0-only
//! Bounded CPU access to an owned RM VRAM allocation through Nova's BAR0 PRAMIN.
//! No BAR1, GPU virtual address, reset, retry, or implicit cleanup.
use crate::{Error, Result, falcon::Platform, raw};
const PRAMIN: u32 = 0x700000;
const BANK_BYTES: u64 = 0x10000; // published NV_PBUS_BAR0_WINDOW_BASE_SHIFT

#[derive(Clone, Copy, Debug)]
pub struct Region {
    base: u64,
    bytes: u64,
}
impl Region {
    /// Addresses are selectors, not authority. The caller must own this entire
    /// allocation in the currently assigned GPU epoch, or have explicit RM
    /// delegation for a shared BAR1 page-table entry (bootstrap only). Read-only
    /// BAR1 binding diagnostics may inspect its register-selected VRAM instance.
    /// CPU writes additionally
    /// require excluding concurrent GPU writes; live reads are provisional.
    pub fn new(base: u64, bytes: u64) -> Result<Self> {
        if base & 3 != 0
            || bytes == 0
            || bytes & 3 != 0
            || base.checked_add(bytes).is_none_or(|end| end > 1 << 40)
        {
            return Err(Error::Range);
        }
        Ok(Self { base, bytes })
    }
    /// Read only this owned allocation. Live GPU-written data is an observation,
    /// not an atomic snapshot or completion proof; caller must establish those.
    pub fn read<P: Platform>(
        &self,
        p: &mut P,
        offset: u64,
        words: &mut [u32],
        deadline: u64,
    ) -> Result<()> {
        let length = (words.len() as u64).checked_mul(4).ok_or(Error::Range)?;
        if offset & 3 != 0
            || offset
                .checked_add(length)
                .is_none_or(|end| end > self.bytes)
        {
            return Err(Error::Range);
        }
        if words.is_empty() {
            return Ok(());
        }
        if p.now_us()? >= deadline {
            return Err(Error::Timeout);
        }
        let previous = p.read32(raw::HORN_PRAMIN_WINDOW)?;
        if previous & !(raw::HORN_PRAMIN_BASE_MASK | raw::HORN_PRAMIN_TARGET_MASK) != 0 {
            return Err(Error::State);
        }
        let mut bank = None;
        for (i, word) in words.iter_mut().enumerate() {
            if i % 256 == 0 && p.now_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let address = self.base + offset + i as u64 * 4;
            let selector = u32::try_from(address / BANK_BYTES).map_err(|_| Error::Range)?;
            if selector & !raw::HORN_PRAMIN_BASE_MASK != 0 {
                return Err(Error::Range);
            }
            let selector = selector << raw::HORN_PRAMIN_BASE_SHIFT;
            if bank != Some(selector) {
                p.write32(raw::HORN_PRAMIN_WINDOW, selector)?;
                if p.read32(raw::HORN_PRAMIN_WINDOW)? != selector {
                    return Err(Error::State);
                }
                bank = Some(selector);
            }
            *word = p.read32(PRAMIN + (address % BANK_BYTES) as u32)?;
        }
        p.write32(raw::HORN_PRAMIN_WINDOW, previous)?;
        if p.read32(raw::HORN_PRAMIN_WINDOW)? != previous {
            return Err(Error::State);
        }
        Ok(())
    }
    pub fn write<P: Platform>(
        &self,
        p: &mut P,
        offset: u64,
        words: &[u32],
        deadline: u64,
    ) -> Result<()> {
        let length = (words.len() as u64).checked_mul(4).ok_or(Error::Range)?;
        if offset & 3 != 0
            || offset
                .checked_add(length)
                .is_none_or(|end| end > self.bytes)
        {
            return Err(Error::Range);
        }
        if words.is_empty() {
            return Ok(());
        }
        if p.now_us()? >= deadline {
            return Err(Error::Timeout);
        }
        let previous = p.read32(raw::HORN_PRAMIN_WINDOW)?;
        if previous & !(raw::HORN_PRAMIN_BASE_MASK | raw::HORN_PRAMIN_TARGET_MASK) != 0 {
            return Err(Error::State);
        }
        let mut bank = None;
        for (i, &value) in words.iter().enumerate() {
            if i % 256 == 0 && p.now_us()? >= deadline {
                return Err(Error::Timeout);
            }
            let address = self.base + offset + i as u64 * 4;
            let selector = u32::try_from(address / BANK_BYTES).map_err(|_| Error::Range)?;
            if selector & !raw::HORN_PRAMIN_BASE_MASK != 0 {
                return Err(Error::Range);
            }
            let selector = selector << raw::HORN_PRAMIN_BASE_SHIFT;
            if bank != Some(selector) {
                p.write32(raw::HORN_PRAMIN_WINDOW, selector)?;
                if p.read32(raw::HORN_PRAMIN_WINDOW)? != selector {
                    return Err(Error::State);
                }
                bank = Some(selector);
            }
            let register = PRAMIN + (address % BANK_BYTES) as u32;
            p.write32(register, value)?;
            if p.read32(register)? != value {
                return Err(Error::State);
            }
        }
        // Readbacks drain all writes. Restore the selector only on success.
        p.write32(raw::HORN_PRAMIN_WINDOW, previous)?;
        if p.read32(raw::HORN_PRAMIN_WINDOW)? != previous {
            return Err(Error::State);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{collections::BTreeMap, vec::Vec};
    #[derive(Default)]
    struct Mock {
        selector: u32,
        vram: BTreeMap<u64, u32>,
        writes: Vec<(u32, u32)>,
        now: u64,
        corrupt: bool,
    }
    impl Platform for Mock {
        fn read32(&mut self, r: u32) -> Result<u32> {
            if r == raw::HORN_PRAMIN_WINDOW {
                return Ok(self.selector);
            }
            if self.corrupt {
                return Ok(0xbad0ac00);
            }
            Ok(*self
                .vram
                .get(&(u64::from(self.selector) * BANK_BYTES + u64::from(r - PRAMIN)))
                .unwrap_or(&0))
        }
        fn write32(&mut self, r: u32, v: u32) -> Result<()> {
            self.writes.push((r, v));
            if r == raw::HORN_PRAMIN_WINDOW {
                self.selector = v;
            } else {
                self.vram.insert(
                    u64::from(self.selector) * BANK_BYTES + u64::from(r - PRAMIN),
                    v,
                );
            }
            Ok(())
        }
        fn now_us(&mut self) -> Result<u64> {
            self.now += 1;
            Ok(self.now)
        }
        fn sleep_until(&mut self, _: u64) -> Result<()> {
            unreachable!()
        }
        fn dma_write_barrier(&mut self) {}
    }
    #[test]
    fn bank_crossing_writes_only_owned_bytes_and_restores_selector() {
        let mut p = Mock {
            selector: 0x1234,
            ..Default::default()
        };
        let region = Region::new(0x1ee60fffc, 8).unwrap();
        region
            .write(&mut p, 0, &[0x12345678, 0xabcdef98], 100)
            .unwrap();
        assert_eq!(p.selector, 0x1234);
        assert_eq!(p.vram[&0x1ee60fffc], 0x12345678);
        assert_eq!(p.vram[&0x1ee610000], 0xabcdef98);
        assert_eq!(p.vram.len(), 2);
    }
    #[test]
    fn range_and_initial_deadline_reject_before_mmio() {
        let mut p = Mock::default();
        let region = Region::new(0x10000, 4).unwrap();
        assert!(region.write(&mut p, 0, &[1, 2], 100).is_err());
        assert!(region.write(&mut p, 1, &[1], 100).is_err());
        assert!(matches!(
            region.write(&mut p, 0, &[1], 1),
            Err(Error::Timeout)
        ));
        assert!(p.writes.is_empty());
        assert!(Region::new((1 << 40) - 4, 8).is_err());
    }
    #[test]
    fn failed_readback_stops_without_retry_or_selector_restore() {
        let mut p = Mock {
            corrupt: true,
            ..Default::default()
        };
        assert!(matches!(
            Region::new(0x10000, 8)
                .unwrap()
                .write(&mut p, 0, &[1, 2], 100),
            Err(Error::State)
        ));
        assert_eq!(p.writes.len(), 2);
        assert_eq!(p.selector, 1);
        assert_eq!(p.vram.len(), 1);
    }
}
