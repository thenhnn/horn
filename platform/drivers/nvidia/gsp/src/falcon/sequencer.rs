// SPDX-License-Identifier: GPL-2.0-only
// NOVA 3497a403 gsp/sequencer.rs: bounded cold-boot CPU sequence.
use super::*;
fn word(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4)
            .ok_or(Error::Malformed)?
            .try_into()
            .map_err(|_| Error::Malformed)?,
    ))
}
fn length(op: u32) -> Result<usize> {
    match op {
        0 => Ok(12),
        1 => Ok(16),
        2 => Ok(24),
        3 => Ok(8),
        4 => Ok(12),
        5..=8 => Ok(4),
        _ => Err(Error::Unsupported),
    }
}
fn address(addr: u32, write: bool) -> Result<()> {
    if addr & 3 != 0 || addr >= 0x1000000 {
        return Err(Error::Range);
    }
    // CPU config ownership, OTP/fuses, PROM and GIN remain outside firmware
    // register-write requests. Unknown writes fail with the register diagnostic.
    if write
        && !(addr == 0x200
            || (0x1400..0x1500).contains(&addr)
            || (0x100000..0x120000).contains(&addr)
            || (0x1fa000..0x1fb000).contains(&addr)
            || (0x840000..0x842000).contains(&addr))
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}
impl<P: Platform> Ad106<P> {
    fn sequence_start(&mut self, engine: Engine) -> Result<()> {
        let p = engine.base();
        let alias = self.read(p + 0x100)? & (1 << 6) != 0;
        self.platform.dma_write_barrier();
        self.write(p + if alias { 0x130 } else { 0x100 }, 2)
    }
    /// # Safety
    /// Payload is a validated private snapshot from this epoch's signed GSP,
    /// never a client submit. LibOS and every nested resource remain retained.
    pub unsafe fn run_sequence(
        &mut self,
        payload: &[u8],
        libos: crate::boot::Span,
        version: u32,
        outer: u64,
    ) -> Result<()> {
        if payload.len() < 40 || libos.bytes() != 4096 {
            return Err(Error::Malformed);
        }
        let words = word(payload, 0)? as usize;
        let commands = word(payload, 4)? as usize;
        if words > 16384 || commands > 1024 || (payload.len() - 40) & 3 != 0 {
            return Err(Error::Range);
        }
        // bufferSizeDWord can describe allocated capacity; cmdIndex and the
        // actual RPC tail bound execution, as in NOVA's GspSeqIter.
        let data = &payload[40..];
        // Validate the COMPLETE stream before the first register mutation.
        let mut at = 0;
        for _ in 0..commands {
            let op = word(data, at)?;
            let len = length(op)?;
            if at.checked_add(len).is_none_or(|end| end > data.len()) {
                return Err(Error::Malformed);
            }
            if matches!(op, 0 | 1 | 2 | 4) {
                let addr = word(data, at + 4)?;
                self.execute(|s| {
                    s.stage = Stage::Sequencer;
                    s.register = addr;
                    s.value = None;
                    address(addr, op <= 1)
                })?;
            }
            at += len;
        }
        self.execute(|s| {
            s.stage = Stage::Sequencer;
            let mut at = 0;
            for _ in 0..commands {
                if s.now()? >= outer {
                    return Err(Error::Timeout);
                }
                let op = word(data, at)?;
                let len = length(op)?;
                let arg = |n: usize| word(data, at + 4 + n * 4);
                if matches!(op, 0 | 1 | 2 | 4) {
                    let addr = arg(0)?;
                    s.register = addr;
                    s.value = None;
                    address(addr, op <= 1)?;
                }
                match op {
                    0 => s.write(arg(0)?, arg(1)?)?,
                    1 => {
                        let old = s.read(arg(0)?)?;
                        s.write(arg(0)?, (old & !arg(1)?) | arg(2)?)?;
                    }
                    2 => {
                        let budget = match arg(3)? {
                            0 => 4_000_000,
                            n => u64::from(n),
                        };
                        let deadline = s.limit(outer, budget)?;
                        s.poll(arg(0)?, arg(1)?, arg(2)?, deadline)?;
                    }
                    3 => {
                        let until = s
                            .now()?
                            .checked_add(u64::from(arg(0)?))
                            .ok_or(Error::Range)?;
                        if until > outer {
                            return Err(Error::Timeout);
                        }
                        s.sleep(until)?;
                    }
                    4 => {
                        if arg(1)? >= 8 {
                            return Err(Error::Range);
                        }
                        s.read(arg(0)?)?;
                    }
                    5 => {
                        s.reset(Engine::Gsp, outer)?;
                        s.update(0x110624, 1 << 7, 1 << 7)?;
                        s.write(0x11010c, 0)?;
                    }
                    6 => s.sequence_start(Engine::Gsp)?,
                    7 => {
                        let deadline = s.limit(outer, 2_000_000)?;
                        s.poll(0x110100, 1 << 4, 1 << 4, deadline)?;
                    }
                    8 => {
                        s.reset(Engine::Gsp, outer)?;
                        s.write(0x110040, libos.address() as u32)?;
                        s.write(0x110044, (libos.address() >> 32) as u32)?;
                        s.sequence_start(Engine::Sec2)?;
                        let deadline = s.limit(outer, 2_000_000)?;
                        s.poll(0x1180f8, 1 << 26, 1 << 26, deadline)?;
                        let status = s.read(0x840040)?;
                        if status != 0 {
                            return Err(Error::FirmwareStatus(status));
                        }
                        s.write(0x110080, version)?;
                        if s.read(0x111388)? & (1 << 7) == 0 {
                            return Err(Error::State);
                        }
                    }
                    _ => return Err(Error::Unsupported),
                }
                at += len;
                s.stage = Stage::Sequencer;
            }
            Ok(())
        })
    }
}
