// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//! NOVA 3497a403 gsp/cmdq.rs: bounded page-ring transport, no Linux slices
//! into device-owned RAM. Caller owns actual timing, GIN acknowledgment and
//! the MMIO doorbell; no timeout is silently extended by unrelated events.
use crate::{Error, Result, boot::*, wire};
use alloc::vec::Vec;
/// Backend accesses are aligned volatile words, in host byte order. Barriers
/// apply to device-visible WB RAM, not only compiler ordering.
pub trait Memory {
    fn bytes(&self) -> usize;
    fn read32(&mut self, offset: usize) -> Result<u32>;
    fn write32(&mut self, offset: usize, value: u32) -> Result<()>;
    fn acquire(&mut self);
    fn release(&mut self);
    fn quarantine(&mut self);
}
#[must_use = "ring publication requires a subsequent GSP doorbell"]
pub struct Published {
    pub transport_sequence: u32,
    pub pages: usize,
}
pub struct Queue<M> {
    memory: M,
    tx: usize,
    rx: usize,
    peer_rx: usize,
    peer_tx: usize,
    sequence: u32,
    poisoned: bool,
}
const RING_BYTES: usize = RING_PAGES * PAGE;
const TX_WRITE: usize = 16;
const RX_READ: usize = 32;
impl<M: Memory> Queue<M> {
    /// # Safety
    /// A fresh, initialized boot::queues allocation for this SID is retained
    /// with stable CPU mappings for the entire Queue lifetime. No other CPU
    /// writer or previous firmware epoch may use it. All shared memory is
    /// accessed via this backend, never ordinary Rust references after handoff.
    pub unsafe fn attach(mut memory: M) -> Result<Self> {
        if memory.bytes() != QUEUE_BYTES {
            return Err(Error::Range);
        }
        for (i, value) in [
            0,
            ((1 + RING_PAGES) * PAGE) as u32,
            PAGE as u32,
            RING_PAGES as u32,
            0,
            1,
            32,
            PAGE as u32,
        ]
        .into_iter()
        .enumerate()
        {
            if memory.read32(CPU_HEADER + i * 4)? != value {
                memory.quarantine();
                return Err(Error::State);
            }
        }
        if memory.read32(CPU_HEADER + RX_READ)? != 0 {
            memory.quarantine();
            return Err(Error::State);
        }
        Ok(Self {
            memory,
            tx: 0,
            rx: 0,
            peer_rx: 0,
            peer_tx: 0,
            sequence: 0,
            poisoned: false,
        })
    }
    fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.poisoned {
            return Err(Error::Quarantined);
        }
        let result = f(self);
        if result.is_err() {
            self.poisoned = true;
            self.memory.quarantine();
        }
        result
    }
    fn pointers(&mut self) -> Result<(usize, usize)> {
        if self.memory.read32(CPU_HEADER + TX_WRITE)? as usize != self.tx
            || self.memory.read32(CPU_HEADER + RX_READ)? as usize != self.rx
        {
            return Err(Error::State);
        }
        let consumed = self.memory.read32(GSP_HEADER + RX_READ)? as usize;
        let produced = self.memory.read32(GSP_HEADER + TX_WRITE)? as usize;
        if consumed >= RING_PAGES || produced >= RING_PAGES {
            return Err(Error::Malformed);
        }
        if (consumed + RING_PAGES - self.peer_rx) % RING_PAGES
            > (self.tx + RING_PAGES - self.peer_rx) % RING_PAGES
            || (produced + RING_PAGES - self.peer_tx) % RING_PAGES
                > RING_PAGES - 1 - (self.peer_tx + RING_PAGES - self.rx) % RING_PAGES
        {
            return Err(Error::Malformed);
        }
        self.memory.acquire();
        self.peer_rx = consumed;
        self.peer_tx = produced;
        Ok((consumed, produced))
    }
    /// No waiting/retry or MMIO here. None means retry under the SAME
    /// absolute caller deadline. A successful publication must ring the device.
    pub fn try_send(
        &mut self,
        function: u32,
        inner_sequence: u32,
        parts: [&[u8]; 2],
    ) -> Result<Option<Published>> {
        self.guarded(|s| {
            let bytes = wire::HEADER
                .checked_add(parts[0].len())
                .and_then(|n| n.checked_add(parts[1].len()))
                .ok_or(Error::Range)?;
            if bytes > wire::MAX_FRAME {
                return Err(Error::Range);
            }
            let pages = bytes.div_ceil(PAGE);
            let (consumed, _) = s.pointers()?;
            if (consumed + RING_PAGES - s.tx - 1) % RING_PAGES < pages {
                return Ok(None);
            }
            let mut snapshot = Vec::new();
            snapshot
                .try_reserve_exact(bytes)
                .map_err(|_| Error::Backend)?;
            snapshot.resize(bytes, 0);
            wire::encode(&mut snapshot, function, s.sequence, inner_sequence, parts)?;
            for (i, chunk) in snapshot.chunks(4).enumerate() {
                let mut word = [0; 4];
                word[..chunk.len()].copy_from_slice(chunk);
                let offset = CPU_DATA + (s.tx * PAGE + i * 4) % RING_BYTES;
                s.memory.write32(offset, u32::from_le_bytes(word))?;
            }
            s.memory.release();
            let tx = (s.tx + pages) % RING_PAGES;
            s.memory.write32(CPU_HEADER + TX_WRITE, tx as u32)?;
            s.memory.release(); // publication before subsequent UC doorbell
            let published = Published {
                transport_sequence: s.sequence,
                pages,
            };
            s.tx = tx;
            s.sequence = s.sequence.wrapping_add(1);
            Ok(Some(published))
        })
    }
    fn snapshot(&mut self, start: usize, out: &mut [u8]) -> Result<()> {
        for (i, chunk) in out.chunks_mut(4).enumerate() {
            let offset = GSP_DATA + (start * PAGE + i * 4) % RING_BYTES;
            let word = self.memory.read32(offset)?.to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }
    /// Return a bounded PRIVATE snapshot. Validate published pages, fixed
    /// framing, a stable copied header and checksum BEFORE advancing readPtr.
    /// Malformed input poisons the transport; no ring skip/resync or retry.
    pub fn try_receive(&mut self) -> Result<Option<Vec<u8>>> {
        self.guarded(|s| {
            let (_, produced) = s.pointers()?;
            let available = (produced + RING_PAGES - s.rx) % RING_PAGES;
            if available == 0 {
                return Ok(None);
            }
            let mut fixed = [0; wire::HEADER];
            s.snapshot(s.rx, &mut fixed)?;
            let header = wire::Header::parse(&fixed)?;
            if header.pages > available {
                return Err(Error::Malformed);
            }
            let mut snapshot = Vec::new();
            snapshot
                .try_reserve_exact(header.bytes)
                .map_err(|_| Error::Backend)?;
            snapshot.resize(header.bytes, 0);
            s.snapshot(s.rx, &mut snapshot)?;
            if snapshot[..wire::HEADER] != fixed {
                return Err(Error::State);
            }
            wire::Packet::decode(&snapshot)?;
            let rx = (s.rx + header.pages) % RING_PAGES;
            // All device-owned reads finish before releasing these pages.
            s.memory.release();
            s.memory.write32(CPU_HEADER + RX_READ, rx as u32)?;
            s.memory.release();
            s.rx = rx;
            Ok(Some(snapshot))
        })
    }
}
