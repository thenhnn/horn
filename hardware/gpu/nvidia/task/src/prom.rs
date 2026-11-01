// SPDX-License-Identifier: GPL-2.0-only
//! Native adapter for NOVA 3497a403 vbios.rs's BAR0 PROM shadow reads.
//! No ROM BAR decoding or option-ROM execution. At most ONE temporary export.
use alloc::vec::Vec;
use horn_driver::DeviceSession;
use horn_nvidia_gsp::{Error, Result, vbios::PROM_BYTES};
const PROM_OFFSET: u64 = 0x300000;
const WINDOW: usize = 65536;

/// Caller has identified the exact assigned AD106 and observed completed GFW.
/// Snapshot is ordinary private CPU RAM, never exposed to GPU DMA.
pub fn snapshot(session: &mut DeviceSession, bar0_bytes: usize) -> Result<Vec<u8>> {
    if PROM_OFFSET
        .checked_add(PROM_BYTES as u64)
        .is_none_or(|end| end > bar0_bytes as u64)
    {
        return Err(Error::Range);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(PROM_BYTES)
        .map_err(|_| Error::Exhausted)?;
    bytes.resize(PROM_BYTES, 0);
    for (index, chunk) in bytes.chunks_mut(WINDOW).enumerate() {
        let window = session.mmio_window(0, PROM_OFFSET + (index * WINDOW) as u64, chunk.len());
        let pointer = window.pointer().as_ptr().cast::<u32>();
        for (word, target) in chunk.chunks_exact_mut(4).enumerate() {
            // SAFETY: page-aligned UC grant remains mapped for this entire loop;
            // word offsets fit the window. No pointer survives explicit close.
            let raw = unsafe { pointer.add(word).read_volatile() };
            target.copy_from_slice(&raw.to_ne_bytes());
        }
        // Acknowledged all-alias revocation, virtual reservation and cap cleanup
        // precede the next map. Failure is fatal, not silent quota recycling.
        session.close_window(window);
    }
    Ok(bytes)
}
