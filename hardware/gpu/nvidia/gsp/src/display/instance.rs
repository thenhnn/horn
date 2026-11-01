// SPDX-License-Identifier: GPL-2.0-only
//! Local display RAMHT, following nouveau r535_dmac_bind, nvkm_ramht_hash
//! and gv100_dmaobj_bind. These are not speculative GSP context-DMA RPCs.
#[cfg(test)]
use crate::raw;
use crate::{Error, Result, falcon::Platform};

pub const RAMHT_BYTES: u32 = 0x2000; // ga102_disp.ramht_size, inherited by AD102
pub const DESCRIPTOR: u32 = RAMHT_BYTES;
pub const INSTANCE_BYTES: u64 = 0x10000;
#[cfg(test)]
const PRAMIN: u32 = 0x700000;

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

/// VRAM analogue of the proven coherent descriptor, from gv100_dmaobj_new.
pub fn vram_descriptor(offset: u64, bytes: usize) -> Result<[u32; 6]> {
    if offset & 4095 != 0
        || bytes == 0
        || bytes & 4095 != 0
        || offset
            .checked_add(bytes as u64)
            .is_none_or(|end| end > 1 << 40)
    {
        return Err(Error::Range);
    }
    let start = offset >> 8;
    let limit = (offset + bytes as u64 - 1) >> 8;
    Ok([
        0x45,
        start as u32,
        (start >> 32) as u32,
        limit as u32,
        (limit >> 32) as u32,
        0,
    ])
}
#[derive(Clone, Copy)]
pub struct Binding {
    pub channel: u32,
    pub handle: u32,
    descriptor: [u32; 6],
}
impl Binding {
    pub fn coherent(channel: u32, handle: u32, iova: u64, bytes: usize) -> Result<Self> {
        Ok(Self {
            channel,
            handle,
            descriptor: coherent_descriptor(iova, bytes)?,
        })
    }
    pub fn vram(channel: u32, handle: u32, offset: u64, bytes: usize) -> Result<Self> {
        Ok(Self {
            channel,
            handle,
            descriptor: vram_descriptor(offset, bytes)?,
        })
    }
}

/// Build all entries before touching VRAM. Bounded linear probing mirrors
/// nvkm_ramht_insert. Repeated handles on different channels are intentional.
pub fn table(client: u32, bindings: &[Binding]) -> Result<alloc::vec::Vec<u32>> {
    if bindings.is_empty() || bindings.len() > 32 {
        return Err(Error::Range);
    }
    let mut words = alloc::vec::Vec::new();
    words
        .try_reserve_exact(RAMHT_BYTES as usize / 4 + bindings.len() * 8)
        .map_err(|_| Error::Exhausted)?;
    words.resize(RAMHT_BYTES as usize / 4 + bindings.len() * 8, 0);
    for (i, b) in bindings.iter().enumerate() {
        if b.handle == 0
            || b.channel >= 16
            || !matches!(b.descriptor[0], 0x45 | 0x46)
            || bindings[..i]
                .iter()
                .any(|old| old.handle == b.handle && old.channel == b.channel)
        {
            return Err(Error::Range);
        }
        let descriptor = DESCRIPTOR + i as u32 * 32; // 16-byte GPU object alignment
        words[descriptor as usize / 4..descriptor as usize / 4 + 6].copy_from_slice(&b.descriptor);
        let mut slot = hash(b.handle, b.channel)? as usize;
        while words[slot * 2] != 0 {
            slot = (slot + 1) % (RAMHT_BYTES as usize / 8);
        }
        words[slot * 2] = b.handle;
        words[slot * 2 + 1] = (b.channel << 25) | (descriptor << 9) | (client & 0x3fff);
    }
    Ok(words)
}

/// Caller owns this fresh aligned 64-KiB instance; no table lookup may be in
/// flight. Never call this on a live table to add or recycle bindings.
pub fn install<P: Platform>(
    p: &mut P,
    base: u64,
    client: u32,
    bindings: &[Binding],
    deadline: u64,
) -> Result<()> {
    if base & (INSTANCE_BYTES - 1) != 0 {
        return Err(Error::Range);
    }
    let words = table(client, bindings)?;
    super::pramin::Region::new(base, INSTANCE_BYTES)?.write(p, 0, &words, deadline)
}

pub fn install_notifier<P: Platform>(
    p: &mut P,
    base: u64,
    client: u32,
    handle: u32,
    iova: u64,
    bytes: usize,
    deadline: u64,
) -> Result<()> {
    install(
        p,
        base,
        client,
        &[Binding {
            channel: 0,
            handle,
            descriptor: coherent_descriptor(iova, bytes)?,
        }],
        deadline,
    )
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
                    || (PRAMIN..PRAMIN + DESCRIPTOR + 32).contains(&offset))
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
        assert_eq!(p.writes.len(), 0); // expired before MMIO; no reset or retry
    }
    #[test]
    fn invalid_descriptor_or_unowned_geometry_does_not_touch_mmio() {
        let mut p = PlatformMock::default();
        assert!(install_notifier(&mut p, 1, 1, 0x1000, 0x1000, 4096, 10000).is_err());
        assert!(install_notifier(&mut p, 0x10000, 1, 0x1000, 1, 4096, 10000).is_err());
        assert!(p.writes.is_empty());
    }
    #[test]
    fn collisions_probe_and_channel_bindings_use_distinct_contexts() {
        let d = coherent_descriptor(0x1000, 4096).unwrap();
        let entries = [
            Binding {
                channel: 0,
                handle: 0x1000,
                descriptor: d,
            },
            Binding {
                channel: 0,
                handle: 0x400000,
                descriptor: d,
            },
            Binding {
                channel: 1,
                handle: 0x1000,
                descriptor: vram_descriptor(0x1ee600000, 0x10000).unwrap(),
            },
        ];
        let words = table(0xc2d00007, &entries).unwrap();
        assert_eq!(words[8], 0x1000);
        assert_eq!(words[10], 0x400000);
        assert_eq!(words[11], (0x2020 << 9) | 7);
        assert_eq!(words[68 * 2 + 1], (1 << 25) | (0x2040 << 9) | 7);
        assert_eq!(
            &words[0x2040 / 4..0x2040 / 4 + 6],
            &[0x45, 0x1ee6000, 0, 0x1ee60ff, 0, 0]
        );
        assert!(table(1, &[entries[0], entries[0]]).is_err());
        assert!(vram_descriptor((1 << 40) - 4096, 8192).is_err());
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
