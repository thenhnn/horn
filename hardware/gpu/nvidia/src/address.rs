// SPDX-License-Identifier: BSD-2-Clause
//! Distinct numerical domains; NONE of these types confers memory authority.
use crate::Error;

macro_rules! address {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name(u64);
        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}
address!(Iova);
address!(VramOffset);
address!(GpuVa);

impl Iova {
    /// Validate the complete interval against Ada's NOVA DMA mask (47 bits).
    /// Must be called on an actual devmgr grant; this cannot authorize a map.
    pub fn check_range(self, length: u64) -> Result<(), Error> {
        if length == 0 || self.0.checked_add(length).is_none_or(|end| end > (1 << 47)) {
            return Err(Error::InvalidRange);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dma_mask_checks_last_byte_and_overflow() {
        assert_eq!(Iova::new((1 << 47) - 4096).check_range(4096), Ok(()));
        assert_eq!(
            Iova::new((1 << 47) - 4096).check_range(4097),
            Err(Error::InvalidRange)
        );
        assert_eq!(Iova::new(0).check_range(0), Err(Error::InvalidRange));
        assert_eq!(Iova::new(u64::MAX).check_range(2), Err(Error::InvalidRange));
    }
}
