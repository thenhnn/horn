// SPDX-License-Identifier: GPL-2.0-only
//! DisplayPort AUX channel: native DPCD transfers and I2C-over-AUX, which is
//! how EDID reaches us from a DisplayPort monitor.
//!
//! Register layout and the handshake follow nouveau's GM200-and-later AUX
//! implementation (`nvkm/subdev/i2c/auxgm200.c`, `padgm200.c`), which is what
//! Ampere uses. The command byte is the DisplayPort definition: bit 0 selects a
//! read, bit 2 is "more to come", and bit 3 is clear for I2C-over-AUX.

use alloc::vec::Vec;

use crate::i2c::Registers;
use crate::{Error, Result};

/// Registers, relative to `channel * 0x50`.
const CTRL: u32 = 0x0000_d954;
const STAT: u32 = 0x0000_d958;
const ADDR: u32 = 0x0000_d950;
const WDATA: u32 = 0x0000_d930;
const RDATA: u32 = 0x0000_d940;
const AUTODPCD: u32 = 0x0000_d968;
/// Pad registers, relative to `pad * 0x50`.
const PAD_CTRL: u32 = 0x0000_d970;
const PAD_OFF: u32 = 0x0000_d97c;

const STRIDE: u32 = 0x50;

/// `CTRL` fields.
const CTRL_BUSY: u32 = 0x0701_0000;
/// Mask used to request the handshake (bits 20-22).
const CTRL_MAGIC_SET: u32 = 0x0070_0000;
const CTRL_MAGIC_REQUEST: u32 = 0x0010_0000;
/// Mask used to observe it (bits 24-26): the hardware moves the request bit up
/// to the reply bit, so the two masks differ.
const CTRL_MAGIC_CHECK: u32 = 0x0700_0000;
const CTRL_MAGIC_REPLY: u32 = 0x0100_0000;
const CTRL_TYPE: u32 = 0x0001_f1ff;
const CTRL_TYPE_SHIFT: u32 = 12;
const CTRL_REQUEST: u32 = 0x0001_0000;
const CTRL_RESET: u32 = 0x8000_0000;

/// `STAT` fields.
const STAT_SINK: u32 = 0x1000_0000;
const STAT_REPLY: u32 = 0x000f_0000;
const STAT_TIMEOUT: u32 = 0x0000_0100;
const STAT_ERROR: u32 = 0x0000_0e00;
const STAT_BYTES: u32 = 0x0000_001f;
/// Reply codes that mean the transaction completed.
const REPLY_ACK: u32 = 0x8;
const REPLY_DEFER: u32 = 0x2;

/// Largest payload one AUX transaction can carry.
pub const CHUNK: usize = 16;
/// `nvkm_i2c_aux_i2c_xfer` retries a chunk this many times while the sink
/// reports zero bytes.
const I2C_RETRIES: u32 = 32;

/// DPCD address of the receiver capability field, used to confirm the link.
pub const DPCD_REV: u32 = 0x00000;

/// Command byte for one transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    NativeRead,
    NativeWrite,
    I2cRead,
    I2cWrite,
}

impl Kind {
    /// Bits written into `CTRL` bits 12 and above.
    pub fn command(self, more: bool) -> u8 {
        let base = match self {
            // Bit 3 marks a native transfer; I2C leaves it clear.
            Kind::NativeRead => 0x9,
            Kind::NativeWrite => 0x8,
            Kind::I2cRead => 0x1,
            Kind::I2cWrite => 0x0,
        };
        if more {
            base | 0x4
        } else {
            base
        }
    }
    fn is_read(self) -> bool {
        matches!(self, Kind::NativeRead | Kind::I2cRead)
    }
}

/// One AUX channel on one pad.
pub struct Aux<'a, R: Registers> {
    registers: &'a mut R,
    pad: u8,
    channel: u8,
    /// Status word of the last transaction. A failing transfer reports a named
    /// error, and this keeps the hardware's own answer for the log.
    pub last_stat: u32,
}

impl<'a, R: Registers> Aux<'a, R> {
    pub fn new(registers: &'a mut R, pad: u8, channel: u8) -> Self {
        Self {
            registers,
            pad,
            channel,
            last_stat: 0,
        }
    }

    fn ctrl(&self) -> u32 {
        CTRL + u32::from(self.channel) * STRIDE
    }
    fn stat(&self) -> u32 {
        STAT + u32::from(self.channel) * STRIDE
    }
    fn addr(&self) -> u32 {
        ADDR + u32::from(self.channel) * STRIDE
    }
    fn wdata(&self, index: usize) -> u32 {
        WDATA + u32::from(self.channel) * STRIDE + index as u32 * 4
    }
    fn rdata(&self, index: usize) -> u32 {
        RDATA + u32::from(self.channel) * STRIDE + index as u32 * 4
    }
    fn autodpcd(&self) -> u32 {
        AUTODPCD + u32::from(self.channel) * STRIDE
    }
    fn pad_ctrl(&self) -> u32 {
        PAD_CTRL + u32::from(self.pad) * STRIDE
    }
    fn pad_off(&self) -> u32 {
        PAD_OFF + u32::from(self.pad) * STRIDE
    }

    fn mask(&mut self, register: u32, mask: u32, value: u32) -> Result<()> {
        let current = self.registers.read32(register)?;
        self.registers
            .write32(register, (current & !mask) | (value & mask))
    }

    /// Switch the pad to AUX and leave it powered.
    pub fn pad_aux(&mut self) -> Result<()> {
        self.mask(self.pad_ctrl(), 0x0000_c003, 0x0000_0002)?;
        self.mask(self.pad_off(), 0x0000_0001, 0)?;
        Ok(())
    }

    /// Release the pad again.
    pub fn pad_off_mode(&mut self) -> Result<()> {
        self.mask(self.pad_off(), 0x0000_0001, 1)
    }

    /// Wait for the channel to be idle and perform the magic handshake. A
    /// channel that never reaches the reply state is reported, not retried
    /// forever.
    fn init(&mut self) -> Result<()> {
        let ctrl = self.ctrl();
        let mut spins = 0u32;
        while self.registers.read32(ctrl)? & CTRL_BUSY != 0 {
            spins += 1;
            if spins > 1000 {
                return Err(Error::Step("aux-busy"));
            }
            self.registers.delay_us(1)?;
        }
        self.mask(ctrl, CTRL_MAGIC_SET, CTRL_MAGIC_REQUEST)?;
        let mut spins = 0u32;
        while self.registers.read32(ctrl)? & CTRL_MAGIC_CHECK != CTRL_MAGIC_REPLY {
            spins += 1;
            if spins > 1000 {
                let _ = self.fini();
                return Err(Error::Step("aux-magic"));
            }
            self.registers.delay_us(1)?;
        }
        Ok(())
    }

    fn fini(&mut self) -> Result<()> {
        // Clear the request, the handshake state and the busy bits. The magic
        // reply bit stays set on this part after a completed transaction, and
        // `init` waits for it to be clear, so leaving it behind wedges the next
        // transfer (the DPCD read succeeded, the EDID write then timed out).
        self.mask(self.ctrl(), CTRL_BUSY | CTRL_MAGIC_SET, 0)
    }

    /// One AUX transaction. `size` is the payload length (0 for an address-only
    /// transaction); on a read it is replaced by the number of bytes the sink
    /// returned.
    pub fn xfer(
        &mut self,
        kind: Kind,
        address: u32,
        data: &mut [u8],
        size: usize,
    ) -> Result<usize> {
        self.xfer_mot(kind, address, data, size, false)
    }

    /// `xfer` with an explicit "more to come" bit, which I2C-over-AUX needs to
    /// keep a multi-chunk transaction open.
    pub fn xfer_mot(
        &mut self,
        kind: Kind,
        address: u32,
        data: &mut [u8],
        size: usize,
        more: bool,
    ) -> Result<usize> {
        if size > CHUNK || data.len() < size {
            return Err(Error::Range);
        }
        self.init()?;
        let result = self.transfer(kind, address, data, size, more);
        let _ = self.fini();
        result
    }

    fn transfer(
        &mut self,
        kind: Kind,
        address: u32,
        data: &mut [u8],
        size: usize,
        more: bool,
    ) -> Result<usize> {
        if self.registers.read32(self.stat())? & STAT_SINK == 0 {
            return Err(Error::Unavailable);
        }
        self.mask(self.autodpcd(), 0x0001_0000, 0)?;

        if !kind.is_read() {
            let mut words = [0u32; 4];
            for (index, byte) in data.iter().take(size).enumerate() {
                words[index / 4] |= u32::from(*byte) << (8 * (index % 4));
            }
            for (index, word) in words.iter().enumerate() {
                self.registers.write32(self.wdata(index), *word)?;
            }
        }

        let ctrl = self.registers.read32(self.ctrl())? & !CTRL_TYPE;
        let ctrl = ctrl
            | (u32::from(kind.command(more)) << CTRL_TYPE_SHIFT)
            | if size == 0 {
                0x0000_0100
            } else {
                (size - 1) as u32
            };
        self.registers.write32(self.addr(), address)?;

        // A reset pulse precedes every request.
        self.registers.write32(self.ctrl(), CTRL_RESET | ctrl)?;
        self.registers.write32(self.ctrl(), ctrl)?;
        self.registers.write32(self.ctrl(), CTRL_REQUEST | ctrl)?;
        // An AUX transaction can take milliseconds, so poll with a real delay
        // and record the status even on timeout: "no answer" and "the sink said
        // no" are different problems.
        let mut spins = 0u32;
        while self.registers.read32(self.ctrl())? & CTRL_REQUEST != 0 {
            spins += 1;
            if spins > 2000 {
                self.last_stat = self.registers.read32(self.stat())?;
                let _ = self.mask(self.autodpcd(), 0x0001_0000, 0x0001_0000);
                return Err(Error::Step("aux-request"));
            }
            self.registers.delay_us(1)?;
        }

        let stat = self.registers.read32(self.stat())?;
        self.last_stat = stat;
        let _ = self.mask(self.autodpcd(), 0x0001_0000, 0x0001_0000);
        if stat & STAT_TIMEOUT != 0 {
            return Err(Error::Step("aux-timeout"));
        }
        if stat & STAT_ERROR != 0 {
            return Err(Error::Step("aux-error"));
        }
        let reply = (stat & STAT_REPLY) >> 16;
        // 0x8 is ACK and 0x2 DEFER on the parts nouveau documents. This one also
        // answers 0 while returning the full payload, which is a completed
        // transaction, not a rejection: only an empty reply is treated as one.
        let completed = reply == REPLY_ACK
            || reply == REPLY_DEFER
            || (reply == 0 && (stat & STAT_BYTES) > 0);
        if !completed {
            return Err(Error::Step("aux-reply"));
        }
        if !kind.is_read() {
            return Ok(size);
        }
        let mut words = [0u32; 4];
        for (index, word) in words.iter_mut().enumerate() {
            *word = self.registers.read32(self.rdata(index))?;
        }
        let count = (stat & STAT_BYTES) as usize;
        if count > size {
            return Err(Error::Malformed);
        }
        for (index, byte) in words.iter().flat_map(|word| word.to_le_bytes()).enumerate() {
            if index < count {
                data[index] = byte;
            }
        }
        Ok(count)
    }

    /// Native read of `out.len()` DPCD bytes.
    pub fn read_dpcd(&mut self, address: u32, out: &mut [u8]) -> Result<()> {
        if out.is_empty() {
            return Err(Error::Range);
        }
        let mut done = 0;
        while done < out.len() {
            let want = (out.len() - done).min(CHUNK);
            let count = self.xfer(
                Kind::NativeRead,
                address + done as u32,
                &mut out[done..],
                want,
            )?;
            if count == 0 {
                return Err(Error::Unavailable);
            }
            done += count;
        }
        Ok(())
    }

    /// Read an EDID blob from the monitor at I2C address 0x50 over AUX.
    ///
    /// I2C-over-AUX needs the offset written first, then a run of reads with the
    /// "more to come" bit set on everything except the final chunk.
    pub fn read_edid(&mut self, blocks: usize) -> Result<Vec<u8>> {
        if blocks == 0 || blocks > 8 {
            return Err(Error::Range);
        }
        let bytes = blocks * 128;
        let mut out = Vec::new();
        out.try_reserve_exact(bytes).map_err(|_| Error::Exhausted)?;
        out.resize(bytes, 0);

        let mut offset = [0u8; 1];
        let mut attempts = 0u32;
        loop {
            let count = self.xfer_mot(Kind::I2cWrite, 0x50, &mut offset, 1, true)?;
            if count == 1 {
                break;
            }
            attempts += 1;
            if attempts >= I2C_RETRIES {
                return Err(Error::Unavailable);
            }
        }

        let mut done = 0;
        while done < bytes {
            let want = (bytes - done).min(CHUNK);
            let more = done + want < bytes;
            let mut attempts = 0u32;
            loop {
                let count = self.xfer_mot(Kind::I2cRead, 0x50, &mut out[done..], want, more)?;
                if count > 0 {
                    done += count;
                    break;
                }
                attempts += 1;
                if attempts >= I2C_RETRIES {
                    return Err(Error::Unavailable);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// AUX sink: answers the handshake, records each command and serves DPCD
    /// and EDID bytes. It is the smallest model that still checks the real
    /// protocol: handshake, reset pulse, reply codes, MOT bit and chunk sizes.
    struct Sink {
        channel: u8,
        registers: Vec<u32>,
        dpcd: Vec<u8>,
        edid: Vec<u8>,
        cursor: usize,
        commands: Vec<(u8, u32)>,
    }

    impl Sink {
        fn new(channel: u8, dpcd: Vec<u8>, edid: Vec<u8>) -> Self {
            let mut sink = Self {
                channel,
                // One word per register offset below 0x10000.
                registers: vec![0u32; 0x4000],
                dpcd,
                edid,
                cursor: 0,
                commands: Vec::new(),
            };
            let stat = sink.stat();
            sink.registers[Self::index(stat)] = STAT_SINK;
            sink
        }
        fn ctrl(&self) -> u32 {
            CTRL + u32::from(self.channel) * STRIDE
        }
        fn stat(&self) -> u32 {
            STAT + u32::from(self.channel) * STRIDE
        }
        fn addr(&self) -> u32 {
            ADDR + u32::from(self.channel) * STRIDE
        }
        fn wdata(&self, index: u32) -> u32 {
            WDATA + u32::from(self.channel) * STRIDE + index * 4
        }
        fn rdata(&self, index: u32) -> u32 {
            RDATA + u32::from(self.channel) * STRIDE + index * 4
        }
        fn index(register: u32) -> usize {
            (register / 4) as usize
        }
        fn execute(&mut self) {
            let ctrl_index = Self::index(self.ctrl());
            let ctrl = self.registers[ctrl_index];
            if ctrl & CTRL_REQUEST == 0 {
                return;
            }
            // Command field is bits 12..15; the request bit above it is not part
            // of it.
            let kind = ((ctrl >> CTRL_TYPE_SHIFT) & 0xf) as u8;
            let size = ((ctrl & 0xff) as usize) + 1;
            let address = self.registers[Self::index(self.addr())];
            self.commands.push((kind, address));
            let is_read = kind & 1 != 0;
            let is_i2c = kind & 0x8 == 0;
            let mut payload = Vec::new();
            for word in 0..4 {
                let value = self.registers[Self::index(self.wdata(word))];
                payload.extend_from_slice(&value.to_le_bytes());
            }
            let count = if is_i2c && !is_read {
                // Offset write: the next read starts here.
                self.cursor = usize::from(payload[0]);
                size
            } else if is_read {
                let source = if is_i2c {
                    self.edid.clone()
                } else {
                    self.dpcd.clone()
                };
                let start = if is_i2c { self.cursor } else { address as usize };
                let available = source.len().saturating_sub(start);
                let count = size.min(available);
                for (index, byte) in source[start..start + count].iter().enumerate() {
                    let register = self.rdata((index / 4) as u32);
                    let word = Self::index(register);
                    let shift = 8 * (index % 4);
                    self.registers[word] &= !(0xffu32 << shift);
                    self.registers[word] |= u32::from(*byte) << shift;
                }
                if is_i2c {
                    self.cursor += count;
                }
                count
            } else {
                size
            };
            let stat = self.stat();
            self.registers[Self::index(stat)] = STAT_SINK | (REPLY_ACK << 16) | count as u32;
            self.registers[ctrl_index] &= !CTRL_REQUEST;
        }
    }

    impl Registers for Sink {
        fn read32(&mut self, offset: u32) -> Result<u32> {
            let index = Self::index(offset);
            let value = *self.registers.get(index).ok_or(Error::Range)?;
            // The magic handshake completes once the driver asks for it.
            if offset == self.ctrl() && value & CTRL_MAGIC_SET == CTRL_MAGIC_REQUEST {
                self.registers[index] = (value & !CTRL_MAGIC_SET) | CTRL_MAGIC_REPLY;
            }
            Ok(*self.registers.get(index).ok_or(Error::Range)?)
        }
        fn write32(&mut self, offset: u32, value: u32) -> Result<()> {
            let index = Self::index(offset);
            if offset == self.ctrl() {
                // Reset and request are pulses, not stored state.
                let stored = if value & CTRL_RESET != 0 {
                    value & !CTRL_RESET
                } else {
                    value
                };
                *self.registers.get_mut(index).ok_or(Error::Range)? = stored;
                if value & CTRL_REQUEST != 0 {
                    self.execute();
                }
                return Ok(());
            }
            *self.registers.get_mut(index).ok_or(Error::Range)? = value;
            Ok(())
        }
    }

    fn edid() -> Vec<u8> {
        (0..128u32).map(|i| (i.wrapping_mul(7) ^ 0x5a) as u8).collect()
    }

    #[test]
    fn native_dpcd_reads_split_into_chunks() {
        let dpcd: Vec<u8> = (0..64u32).map(|i| i as u8).collect();
        let mut sink = Sink::new(0, dpcd.clone(), edid());
        {
            let mut aux = Aux::new(&mut sink, 0, 0);
            aux.pad_aux().unwrap();
            let mut out = [0u8; 40];
            aux.read_dpcd(0, &mut out).unwrap();
            assert_eq!(out.to_vec(), dpcd[..40].to_vec());
        }
        // 16 + 16 + 8: three native reads, all at the expected addresses.
        let addresses: Vec<u32> = sink.commands.iter().map(|(_, address)| *address).collect();
        assert_eq!(addresses, vec![0, 16, 32]);
    }

    #[test]
    fn edid_over_aux_writes_the_offset_then_reads_with_mot() {
        let expected = edid();
        let mut sink = Sink::new(2, vec![0u8; 16], expected.clone());
        let bytes = {
            let mut aux = Aux::new(&mut sink, 0, 2);
            aux.pad_aux().unwrap();
            match aux.read_edid(1) {
                Ok(bytes) => bytes,
                Err(error) => panic!("edid read failed: {error:?}"),
            }
        };
        assert_eq!(bytes, expected);
        let log = &sink.commands;
        // Offset write with MOT, then reads with MOT except the last chunk.
        assert_eq!(log[0], (Kind::I2cWrite.command(true), 0x50));
        assert_eq!(log[1], (Kind::I2cRead.command(true), 0x50));
        assert_eq!(
            log.last().copied(),
            Some((Kind::I2cRead.command(false), 0x50))
        );
        // 128 bytes in 16-byte chunks after the offset write.
        assert_eq!(log.len(), 1 + 8);
    }

    #[test]
    fn a_sink_that_is_not_present_is_reported() {
        let mut sink = Sink::new(0, vec![0u8; 16], edid());
        sink.registers[Sink::index(STAT)] = 0;
        let mut aux = Aux::new(&mut sink, 0, 0);
        assert!(matches!(
            aux.read_dpcd(0, &mut [0u8; 4]),
            Err(Error::Unavailable)
        ));
    }
}
