// SPDX-License-Identifier: GPL-2.0-only
//! Local display RAMHT, following nouveau r535_dmac_bind, nvkm_ramht_hash
//! and gv100_dmaobj_bind. These are not speculative GSP context-DMA RPCs.
use crate::{Error, Result, falcon::Platform, raw};

pub const RAMHT_BYTES: u32 = 0x2000; // ga102_disp.ramht_size, inherited by AD102
pub const DESCRIPTOR: u32 = RAMHT_BYTES;
pub const INSTANCE_BYTES: u64 = 0x10000;
const PRAMIN: u32 = 0x700000; // Nova PRAMIN (BAR0, not BAR1)

pub fn hash(handle: u32, channel: u32) -> Result<u32> {
    if channel >= 16 {
        return Err(Error::Range);
    }
    let mut value = handle;
    let mut hash = 0;
    // 8192 bytes / 8-byte entries = 1024 entries, 10 hash bits.
    while value != 0 {
        hash ^= value & 1023;
        value >>= 10;
    }
    Ok(hash ^ (channel << 6))
}

/// PCI-coherent, read/write, small-page, linear descriptor. Start/limit are
/// in 256-byte units, exactly as in nouveau gv100_dmaobj_bind.
pub fn coherent_descriptor(iova: u64, bytes: usize) -> Result<[u32; 6]> {
    if iova & 4095 != 0 || bytes == 0 || bytes & 4095 != 0 {
        return Err(Error::Range);
    }
    let end = iova.checked_add(bytes as u64).ok_or(Error::Range)?;
    if end > 1 << 47 {
        return Err(Error::Range);
    }
    let start = iova >> 8;
    let limit = (end - 1) >> 8;
    Ok([
        0x46,
        start as u32,
        (start >> 32) as u32,
        limit as u32,
        (limit >> 32) as u32,
        0,
    ])
}

/// Initialize only the allocated instance's RAMHT and first descriptor, before
/// publishing the first command. Caller owns this fresh aligned 64-KiB allocation.
/// Every write is read back; failure is fail-stop, never a retry or BAR1 fallback.
pub fn install_notifier<P: Platform>(
    p: &mut P,
    base: u64,
    client: u32,
    handle: u32,
    iova: u64,
    bytes: usize,
    deadline: u64,
) -> Result<()> {
    if base & (INSTANCE_BYTES - 1) != 0
        || base.checked_add(INSTANCE_BYTES).is_none_or(|v| v > 1 << 40)
        || handle == 0
    {
        return Err(Error::Range);
    }
    let descriptor = coherent_descriptor(iova, bytes)?;
    let previous = p.read32(raw::HORN_PRAMIN_WINDOW)?;
    if previous & !(raw::HORN_PRAMIN_BASE_MASK | raw::HORN_PRAMIN_TARGET_MASK) != 0 {
        return Err(Error::State);
    }
    let window = u32::try_from(base >> 16).map_err(|_| Error::Range)?;
    if window & !raw::HORN_PRAMIN_BASE_MASK != 0 {
        return Err(Error::Range);
    }
    p.write32(
        raw::HORN_PRAMIN_WINDOW,
        window << raw::HORN_PRAMIN_BASE_SHIFT,
    )?;
    if p.read32(raw::HORN_PRAMIN_WINDOW)? != window << raw::HORN_PRAMIN_BASE_SHIFT {
        return Err(Error::State);
    }
    let mut write = |offset, value| -> Result<()> {
        if p.now_us()? >= deadline {
            return Err(Error::Timeout);
        }
        p.write32(PRAMIN + offset, value)?;
        if p.read32(PRAMIN + offset)? != value {
            return Err(Error::State);
        }
        Ok(())
    };
    // No channel can look up this table yet. No access outside our allocation.
    for offset in (0..RAMHT_BYTES).step_by(4) {
        write(offset, 0)?;
    }
    for (i, value) in descriptor.into_iter().enumerate() {
        write(DESCRIPTOR + i as u32 * 4, value)?;
    }
    let slot = hash(handle, 0)? * 8; // core channel user ID is zero
    write(slot, handle)?;
    write(slot + 4, (DESCRIPTOR << 9) | (client & 0x3fff))?;
    // Posted-write drain, then restore the aperture selector only after success.
    p.read32(PRAMIN + slot + 4)?;
    p.write32(raw::HORN_PRAMIN_WINDOW, previous)?;
    if p.read32(raw::HORN_PRAMIN_WINDOW)? != previous {
        return Err(Error::State);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct PlatformMock {
        words: alloc::collections::BTreeMap<u32, u32>,
        now: u64,
        corrupt: bool,
        writes: alloc::vec::Vec<(u32, u32)>,
    }
    impl Platform for PlatformMock {
        fn read32(&mut self, offset: u32) -> Result<u32> {
            if self.corrupt && offset == PRAMIN + DESCRIPTOR {
                return Ok(0xbad0_ac00);
            }
            Ok(*self.words.get(&offset).unwrap_or(&0))
        }
        fn write32(&mut self, offset: u32, value: u32) -> Result<()> {
            self.words.insert(offset, value);
            self.writes.push((offset, value));
            Ok(())
        }
        fn now_us(&mut self) -> Result<u64> {
            self.now += 1;
            Ok(self.now)
        }
        fn sleep_until(&mut self, deadline: u64) -> Result<()> {
            self.now = deadline;
            Ok(())
        }
        fn dma_write_barrier(&mut self) {}
    }
    #[test]
    fn table_upload_is_bounded_and_restores_selector_after_readback() {
        let mut p = PlatformMock::default();
        p.words.insert(raw::HORN_PRAMIN_WINDOW, 0x1234);
        install_notifier(
            &mut p,
            0x1ee5c0000,
            0xc2d00000,
            0x1000,
            0x3dc3000,
            4096,
            10000,
        )
        .unwrap();
        assert_eq!(p.words[&raw::HORN_PRAMIN_WINDOW], 0x1234);
        assert_eq!(p.words[&(PRAMIN + DESCRIPTOR)], 0x46);
        assert_eq!(p.words[&(PRAMIN + hash(0x1000, 0).unwrap() * 8)], 0x1000);
        assert_eq!(
            p.words[&(PRAMIN + hash(0x1000, 0).unwrap() * 8 + 4)],
            DESCRIPTOR << 9
        );
        assert!(
            p.writes
                .iter()
                .all(|&(offset, _)| offset == raw::HORN_PRAMIN_WINDOW
                    || (PRAMIN..PRAMIN + DESCRIPTOR + 24).contains(&offset))
        );
    }
    #[test]
    fn corrupt_readback_or_timeout_is_not_retried_or_restored() {
        let mut p = PlatformMock {
            corrupt: true,
            ..Default::default()
        };
        assert!(matches!(
            install_notifier(&mut p, 0x1ee5c0000, 1, 0x1000, 0x3dc3000, 4096, 10000),
            Err(Error::State)
        ));
        assert_eq!(p.words[&raw::HORN_PRAMIN_WINDOW], 0x1ee5c);
        assert_eq!(
            p.writes
                .iter()
                .filter(|&&(offset, _)| offset == PRAMIN + DESCRIPTOR)
                .count(),
            1
        );
        let mut p = PlatformMock::default();
        assert!(matches!(
            install_notifier(&mut p, 0x1ee5c0000, 1, 0x1000, 0x3dc3000, 4096, 1),
            Err(Error::Timeout)
        ));
        assert_eq!(p.writes.len(), 1); // selector only; no descriptor, reset or retry
    }
    #[test]
    fn invalid_descriptor_or_unowned_geometry_does_not_touch_mmio() {
        let mut p = PlatformMock::default();
        assert!(install_notifier(&mut p, 1, 1, 0x1000, 0x1000, 4096, 10000).is_err());
        assert!(install_notifier(&mut p, 0x10000, 1, 0x1000, 1, 4096, 10000).is_err());
        assert!(p.writes.is_empty());
    }
    #[test]
    fn descriptor_uses_coherent_iova_not_cpu_or_gpu_virtual_address() {
        assert_eq!(
            coherent_descriptor(0x3dc3000, 4096).unwrap(),
            [0x46, 0x3dc30, 0, 0x3dc3f, 0, 0]
        );
        assert!(coherent_descriptor(1, 4096).is_err());
        assert!(coherent_descriptor(0x1000, 0).is_err());
        assert!(coherent_descriptor((1 << 47) - 4096, 8192).is_err());
        assert!(coherent_descriptor(u64::MAX - 4095, 4096).is_err());
    }
    #[test]
    fn ramht_folds_handle_and_channel_as_nouveau() {
        assert_eq!(hash(0x1000, 0).unwrap(), 4);
        assert_eq!(hash(0x1000, 1).unwrap(), 68);
        assert!(hash(1, 16).is_err());
        const {
            assert!((DESCRIPTOR << 9) & 0x3fff == 0);
        }
    }
}
