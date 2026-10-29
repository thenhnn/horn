// SPDX-License-Identifier: BSD-2-Clause
//! Read-only AD106 identification and GFW status.
//!
//! Register locations/fields: NOVA 3497a403, gpu/regs.rs and regs.rs.
//! No bus-master enable, reset, BAR sizing or register writes.
use crate::Error;
use core::{marker::PhantomData, ptr::NonNull};

pub const BAR0_BYTES: usize = 16 * 1024 * 1024;
const BOOT0: usize = 0;
const BOOT42: usize = 0xa00;
const GFW_PRIVILEGE: usize = 0x118128;
const GFW_PROGRESS: usize = 0x118234;
const AD106: u32 = 0x196;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciIdentity {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
}

/// A board allowlist supplied from the host inventory, not device authority.
/// Deliberately no guessed PCI ID / VRAM-size default for the user's board.
#[derive(Clone, Copy, Debug)]
pub struct TargetBoard(pub PciIdentity);

/// Host-confirmed GPU function only; audio 10de:22bd is deliberately excluded.
/// BDF/BAR addresses are assigned at runtime, not part of this allowlist.
pub const PALIT_RTX_4060_TI: TargetBoard = TargetBoard(PciIdentity {
    vendor: 0x10de,
    device: 0x2803,
    subsystem_vendor: 0x1569,
    subsystem_device: 0xf306,
});

pub trait ReadBar0 {
    /// Logical BAR extent, not necessarily one contiguous CPU mapping. Reads
    /// outside retained register windows may still return InvalidRange.
    fn length(&self) -> usize;
    fn read32(&mut self, offset: usize) -> Result<u32, Error>;
}

/// Read-only CPU view of a BAR grant. Not a grant owner; Drop releases nothing.
pub struct MappedBar0<'grant> {
    base: NonNull<u8>,
    length: usize,
    _grant: PhantomData<&'grant mut [u8]>,
}
impl<'grant> MappedBar0<'grant> {
    /// # Safety
    /// The entire range must remain mapped UC device memory for 'grant.
    /// The assignment and its CPU mapping must outlive this view. The caller
    /// must serialize accesses and prevent device removal/revocation during a
    /// read. No CPU RAM references may alias the MMIO range.
    pub unsafe fn from_grant(base: NonNull<u8>, length: usize) -> Result<Self, Error> {
        if base.as_ptr().addr() & 3 != 0
            || length < BAR0_BYTES
            || base.as_ptr().addr().checked_add(length).is_none()
        {
            return Err(Error::InvalidRange);
        }
        Ok(Self {
            base,
            length,
            _grant: PhantomData,
        })
    }
}
impl ReadBar0 for MappedBar0<'_> {
    fn length(&self) -> usize {
        self.length
    }
    fn read32(&mut self, offset: usize) -> Result<u32, Error> {
        if offset & 3 != 0 || offset.checked_add(4).is_none_or(|end| end > self.length) {
            return Err(Error::InvalidRange);
        }
        // SAFETY: construction contract and the bounds/alignment checks above.
        Ok(u32::from_le(unsafe {
            self.base
                .as_ptr()
                .wrapping_add(offset)
                .cast::<u32>()
                .read_volatile()
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ad106 {
    pub boot0: u32,
    pub boot42: u32,
    pub revision_major: u8,
    pub revision_minor: u8,
}
impl Ad106 {
    /// Called only AFTER devmgr has assigned the particular PCI function.
    /// class_subclass is the PCI class/subclass pair (0300 VGA or 0302 3D).
    pub fn identify(
        target: TargetBoard,
        actual: PciIdentity,
        class_subclass: u16,
        bar: &mut impl ReadBar0,
    ) -> Result<Self, Error> {
        if actual.vendor != 0x10de || !matches!(class_subclass, 0x0300 | 0x0302) {
            return Err(Error::Unsupported);
        }
        if actual != target.0 {
            return Err(Error::WrongBoard);
        }
        if bar.length() < BAR0_BYTES {
            return Err(Error::InvalidRange);
        }
        let boot0 = bar.read32(BOOT0)?;
        if boot0 == u32::MAX {
            return Err(Error::DeviceUnavailable);
        }
        // Do not touch BOOT42 on an older-than-Fermi device.
        if boot0 & (1 << 8) == 0 && (boot0 >> 24) & 31 < 0xc {
            return Err(Error::Unsupported);
        }
        let boot42 = bar.read32(BOOT42)?;
        if boot42 == u32::MAX {
            return Err(Error::DeviceUnavailable);
        }
        if (boot42 >> 20) & 0x3ff != AD106 {
            return Err(Error::Unsupported);
        }
        Ok(Self {
            boot0,
            boot42,
            revision_major: ((boot42 >> 16) & 15) as u8,
            revision_minor: ((boot42 >> 12) & 15) as u8,
        })
    }

    /// Nonblocking; caller applies an absolute clock deadline and bounded sleeps.
    /// FWSEC must lower the privilege mask BEFORE the progress register is read.
    pub fn poll_gfw(&self, bar: &mut impl ReadBar0) -> Result<GfwStatus, Error> {
        let privilege = bar.read32(GFW_PRIVILEGE)?;
        if privilege == u32::MAX {
            return Err(Error::DeviceUnavailable);
        }
        if privilege & 1 == 0 {
            return Ok(GfwStatus::PrivilegeLocked);
        }
        let progress = bar.read32(GFW_PROGRESS)?;
        if progress == u32::MAX {
            return Err(Error::DeviceUnavailable);
        }
        Ok(if progress & 255 == 255 {
            GfwStatus::Complete
        } else {
            GfwStatus::InProgress(progress as u8)
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GfwStatus {
    PrivilegeLocked,
    InProgress(u8),
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;
    const BOARD: PciIdentity = PciIdentity {
        vendor: 0x10de,
        device: 0x2803,
        subsystem_vendor: 0x1569,
        subsystem_device: 0xf306,
    };
    struct Registers {
        reads: usize,
        locked: bool,
    }
    impl ReadBar0 for Registers {
        fn length(&self) -> usize {
            BAR0_BYTES
        }
        fn read32(&mut self, offset: usize) -> Result<u32, Error> {
            self.reads += 1;
            match offset {
                BOOT0 => Ok(0x196000a1),
                BOOT42 => Ok(0x196a1000),
                GFW_PRIVILEGE => Ok(u32::from(!self.locked)),
                GFW_PROGRESS if !self.locked => Ok(255),
                _ => panic!("unexpected/forbidden register read"),
            }
        }
    }
    #[test]
    fn exact_board_and_chipset_then_privilege_before_progress() {
        let mut bar = Registers {
            reads: 0,
            locked: true,
        };
        let chip = Ad106::identify(PALIT_RTX_4060_TI, BOARD, 0x0300, &mut bar).unwrap();
        assert_eq!((chip.revision_major, chip.revision_minor), (10, 1));
        assert_eq!(chip.poll_gfw(&mut bar), Ok(GfwStatus::PrivilegeLocked));
        assert_eq!(bar.reads, 3);
        bar.locked = false;
        assert_eq!(chip.poll_gfw(&mut bar), Ok(GfwStatus::Complete));
        assert_eq!(bar.reads, 5);
    }
    #[test]
    fn reject_board_or_non_display_before_any_mmio() {
        let mut bar = Registers {
            reads: 0,
            locked: false,
        };
        let other = PciIdentity {
            device: 0x4321,
            ..BOARD
        };
        assert_eq!(
            Ad106::identify(TargetBoard(BOARD), other, 0x0300, &mut bar),
            Err(Error::WrongBoard)
        );
        assert_eq!(
            Ad106::identify(TargetBoard(BOARD), BOARD, 0x0403, &mut bar),
            Err(Error::Unsupported)
        );
        assert_eq!(bar.reads, 0);
    }
    #[test]
    fn reject_other_chipset_and_disconnected_reads() {
        struct Raw(u32);
        impl ReadBar0 for Raw {
            fn length(&self) -> usize {
                BAR0_BYTES
            }
            fn read32(&mut self, _: usize) -> Result<u32, Error> {
                Ok(self.0)
            }
        }
        for word in [0, 0x194000a1, u32::MAX] {
            assert!(Ad106::identify(TargetBoard(BOARD), BOARD, 0x0300, &mut Raw(word)).is_err());
        }
    }
}
