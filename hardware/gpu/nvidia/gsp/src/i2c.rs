// SPDX-License-Identifier: GPL-2.0-only
//! Bit-banged DDC/I2C master, following the GF119-and-later register layout
//! nouveau uses (`nvkm/subdev/i2c/busgf119.c`): one 32-bit register per bus at
//! `0x00d014 + drive * 0x20`, bit 0 drives SCL, bit 1 drives SDA, bit 4 senses
//! SCL and bit 5 senses SDA.
//!
//! The protocol itself lives here, not in the caller: a DDC read is a write of
//! the offset byte, a repeated start, and then a run of reads, with an ACK after
//! every byte but the last.

use alloc::vec::Vec;

use crate::{Error, Result};

/// Register stride between I2C buses.
pub const BUS_STRIDE: u32 = 0x20;
/// Register of bus 0.
pub const BUS_BASE: u32 = 0x0000_d014;
/// Value written once before a transfer.
pub const BUS_INIT: u32 = 0x7;

pub const DRIVE_SCL: u32 = 0x1;
pub const DRIVE_SDA: u32 = 0x2;
pub const SENSE_SCL: u32 = 0x10;
pub const SENSE_SDA: u32 = 0x20;

/// Address of the I2C bus carrying a DCB entry's DDC traffic.
pub fn bus_register(drive: u8) -> u32 {
    BUS_BASE + u32::from(drive) * BUS_STRIDE
}

/// Register-level access, so the protocol is testable without hardware.
pub trait Registers {
    fn read32(&mut self, offset: u32) -> Result<u32>;
    fn write32(&mut self, offset: u32, value: u32) -> Result<()>;
    /// Wait at least `us` microseconds. The AUX and DDC protocols are timed in
    /// microseconds and a bare spin loop is orders of magnitude too short, so a
    /// backend that can sleep must say so. The default keeps pure models free of
    /// a clock.
    fn delay_us(&mut self, _us: u32) -> Result<()> {
        Ok(())
    }
}

/// One bit-banged I2C bus.
pub struct Bus<'a, R: Registers> {
    registers: &'a mut R,
    address: u32,
}

impl<'a, R: Registers> Bus<'a, R> {
    pub fn new(registers: &'a mut R, drive: u8) -> Self {
        Self {
            registers,
            address: bus_register(drive),
        }
    }

    /// Leave both lines released and driven low, the idle state the hardware
    /// expects before the first start condition.
    pub fn init(&mut self) -> Result<()> {
        self.registers.write32(self.address, BUS_INIT)
    }

    fn drive(&mut self, mask: u32, high: bool) -> Result<()> {
        let value = self.registers.read32(self.address)?;
        let value = if high { value | mask } else { value & !mask };
        self.registers.write32(self.address, value)
    }

    fn sense(&mut self, mask: u32) -> Result<bool> {
        Ok(self.registers.read32(self.address)? & mask != 0)
    }

    pub fn set_scl(&mut self, high: bool) -> Result<()> {
        self.drive(DRIVE_SCL, high)
    }

    pub fn set_sda(&mut self, high: bool) -> Result<()> {
        self.drive(DRIVE_SDA, high)
    }

    pub fn scl(&mut self) -> Result<bool> {
        self.sense(SENSE_SCL)
    }

    pub fn sda(&mut self) -> Result<bool> {
        self.sense(SENSE_SDA)
    }

    /// Clock out one bit, then return whether the slave acknowledged.
    fn write_bit(&mut self, bit: bool) -> Result<()> {
        self.set_sda(bit)?;
        self.set_scl(true)?;
        self.set_scl(false)?;
        Ok(())
    }

    fn read_bit(&mut self) -> Result<bool> {
        self.set_sda(true)?;
        self.set_scl(true)?;
        let bit = self.sda()?;
        self.set_scl(false)?;
        Ok(bit)
    }

    fn start(&mut self) -> Result<()> {
        self.set_sda(true)?;
        self.set_scl(true)?;
        self.set_sda(false)?;
        self.set_scl(false)?;
        Ok(())
    }

    /// A repeated start is a start without releasing the bus in between.
    fn restart(&mut self) -> Result<()> {
        self.set_sda(true)?;
        self.set_scl(true)?;
        self.set_sda(false)?;
        self.set_scl(false)?;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.set_sda(false)?;
        self.set_scl(true)?;
        self.set_sda(true)?;
        Ok(())
    }

    /// Send one byte; `Ok(true)` when the slave acknowledged.
    fn write_byte(&mut self, byte: u8) -> Result<bool> {
        for shift in (0..8).rev() {
            self.write_bit(byte & (1 << shift) != 0)?;
        }
        let ack = !self.read_bit()?;
        Ok(ack)
    }

    fn read_byte(&mut self, ack: bool) -> Result<u8> {
        let mut byte = 0u8;
        for _ in 0..8 {
            byte = (byte << 1) | u8::from(self.read_bit()?);
        }
        // Acknowledge by pulling SDA low for the ninth clock.
        self.write_bit(!ack)?;
        Ok(byte)
    }
}

/// Read `bytes` from an I2C device at `address` starting at register `offset`.
///
/// DDC-EDID answers a 128-byte block per request, so callers read in block
/// multiples and use the base block's extension count to decide how much more to
/// ask for.
pub fn read<B: BusLike>(bus: &mut B, address: u8, offset: u8, bytes: usize) -> Result<Vec<u8>> {
    if bytes == 0 || bytes > 1024 {
        return Err(Error::Range);
    }
    bus.init()?;
    bus.start()?;
    if !bus.write_byte(address << 1)? {
        bus.stop()?;
        return Err(Error::Unavailable);
    }
    if !bus.write_byte(offset)? {
        bus.stop()?;
        return Err(Error::Unavailable);
    }
    bus.restart()?;
    if !bus.write_byte((address << 1) | 1)? {
        bus.stop()?;
        return Err(Error::Unavailable);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(bytes).map_err(|_| Error::Exhausted)?;
    for index in 0..bytes {
        out.push(bus.read_byte(index + 1 != bytes)?);
    }
    bus.stop()?;
    Ok(out)
}

/// The subset of [`Bus`] a transfer needs, so a test can drive the protocol
/// with its own state machine.
pub trait BusLike {
    fn init(&mut self) -> Result<()>;
    fn start(&mut self) -> Result<()>;
    fn restart(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn write_byte(&mut self, byte: u8) -> Result<bool>;
    fn read_byte(&mut self, ack: bool) -> Result<u8>;
}

impl<R: Registers> BusLike for Bus<'_, R> {
    fn init(&mut self) -> Result<()> {
        Bus::init(self)
    }
    fn start(&mut self) -> Result<()> {
        Bus::start(self)
    }
    fn restart(&mut self) -> Result<()> {
        Bus::restart(self)
    }
    fn stop(&mut self) -> Result<()> {
        Bus::stop(self)
    }
    fn write_byte(&mut self, byte: u8) -> Result<bool> {
        Bus::write_byte(self, byte)
    }
    fn read_byte(&mut self, ack: bool) -> Result<u8> {
        Bus::read_byte(self, ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-level register file: no slave logic, just the four bits the driver
    /// drives and senses.
    struct Pins {
        value: u32,
        writes: Vec<u32>,
    }

    impl Registers for Pins {
        fn read32(&mut self, _offset: u32) -> Result<u32> {
            // Open-drain lines with pull-ups: an undriven line reads high, and
            // the sense bits mirror what the line is doing.
            let mut value = self.value & 0x3;
            if value & DRIVE_SCL != 0 {
                value |= SENSE_SCL;
            }
            if value & DRIVE_SDA != 0 {
                value |= SENSE_SDA;
            }
            Ok(value)
        }
        fn write32(&mut self, _offset: u32, value: u32) -> Result<()> {
            // Only the drive bits are writable; sense bits are read-only.
            self.writes.push(value & 0x3);
            self.value = value & 0x3;
            Ok(())
        }
    }

    /// I2C slave that decodes the byte stream and serves a canned buffer. It
    /// models what a monitor does on DDC, so the protocol above it is exercised
    /// without hardware.
    struct Slave {
        data: Vec<u8>,
        cursor: usize,
        /// Address latched by the last start, `None` before one.
        address: Option<u8>,
        reading: bool,
        offset: u8,
        started: usize,
        stops: usize,
        /// Set when the master sent a NACK on the final read byte.
        ended_cleanly: bool,
    }

    impl Slave {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                cursor: 0,
                address: None,
                reading: false,
                offset: 0,
                started: 0,
                stops: 0,
                ended_cleanly: false,
            }
        }
    }

    impl BusLike for Slave {
        fn init(&mut self) -> Result<()> {
            Ok(())
        }
        fn start(&mut self) -> Result<()> {
            self.address = None;
            self.reading = false;
            self.started += 1;
            Ok(())
        }
        fn restart(&mut self) -> Result<()> {
            self.start()
        }
        fn stop(&mut self) -> Result<()> {
            self.stops += 1;
            Ok(())
        }
        fn write_byte(&mut self, byte: u8) -> Result<bool> {
            match self.address {
                None => {
                    let address = byte >> 1;
                    if address != 0x50 {
                        return Ok(false);
                    }
                    self.reading = byte & 1 == 1;
                    self.address = Some(address);
                    Ok(true)
                }
                Some(_) if !self.reading => {
                    self.offset = byte;
                    self.cursor = self.offset as usize;
                    Ok(true)
                }
                Some(_) => {
                    // Master acknowledging a byte the slave sent.
                    self.ended_cleanly = byte & 1 != 0;
                    Ok(true)
                }
            }
        }
        fn read_byte(&mut self, _ack: bool) -> Result<u8> {
            let byte = self.data.get(self.cursor).copied().unwrap_or(0xff);
            self.cursor += 1;
            Ok(byte)
        }
    }

    #[test]
    fn a_ddc_read_returns_the_edid_bytes() {
        let edid: Vec<u8> = (0..128u8).map(|i| i.wrapping_mul(3)).collect();
        let mut slave = Slave::new(edid.clone());
        let bytes = read(&mut slave, 0x50, 0, 128).unwrap();
        assert_eq!(bytes, edid);
        // Address phase, offset, repeated start, then one stop at the end.
        assert_eq!(slave.started, 2);
        assert_eq!(slave.stops, 1);
    }

    #[test]
    fn an_absent_device_is_reported_not_hung() {
        let mut slave = Slave::new(Vec::new());
        // Nothing answers 0x51, so the address phase is not acknowledged.
        assert!(matches!(read(&mut slave, 0x51, 0, 128), Err(Error::Unavailable)));
        // The bus is still released: a failed read must not leave it held.
        assert_eq!(slave.stops, 1);
    }

    #[test]
    fn the_offset_byte_selects_where_the_read_starts() {
        let edid: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let mut slave = Slave::new(edid.clone());
        let bytes = read(&mut slave, 0x50, 128, 128).unwrap();
        assert_eq!(bytes, edid[128..].to_vec());
    }

    #[test]
    fn driving_and_sensing_use_the_documented_bits() {
        let mut pins = Pins {
            value: 0,
            writes: Vec::new(),
        };
        {
            let mut bus = Bus::new(&mut pins, 0);
            bus.set_scl(true).unwrap();
            assert_eq!(bus.registers.value & DRIVE_SCL, DRIVE_SCL);
            bus.set_sda(true).unwrap();
            assert_eq!(bus.registers.value & DRIVE_SDA, DRIVE_SDA);
            bus.set_scl(false).unwrap();
            assert_eq!(bus.registers.value & DRIVE_SCL, 0);
            // Sense lines read the same register.
            assert!(!bus.scl().unwrap());
            assert!(bus.sda().unwrap());
            bus.init().unwrap();
            // Only the drive bits are writable, so init lands as 0b11.
            assert_eq!(bus.registers.value, BUS_INIT & 0x3);
        }
    }

    #[test]
    fn the_bus_register_follows_the_dcb_drive_number() {
        assert_eq!(bus_register(0), 0x0000_d014);
        assert_eq!(bus_register(1), 0x0000_d034);
        assert_eq!(bus_register(4), 0x0000_d094);
    }
}
