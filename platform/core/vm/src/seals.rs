//! Monotonic backing policy; mapping authority is checked by the VM service.
use crate::protocol::{VmError, VmSeals};

impl VmSeals {
    pub(crate) fn blocks_new_writes(self) -> bool {
        self.0 & (Self::WRITE.0 | Self::FUTURE_WRITE.0) != 0
    }

    /// Return the complete new set without mutating anything on failure.
    pub(crate) fn adding(self, requested: Self, may_write: bool) -> Result<Self, VmError> {
        let known =
            Self::SEAL.0 | Self::SHRINK.0 | Self::GROW.0 | Self::WRITE.0 | Self::FUTURE_WRITE.0;
        if requested.0 & !known != 0 {
            return Err(VmError::Invalid);
        }
        if self.0 & Self::SEAL.0 != 0 {
            return Err(VmError::Denied);
        }
        if requested.0 & Self::WRITE.0 != 0 && may_write {
            return Err(VmError::Busy);
        }
        Ok(Self(self.0 | requested.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additions_are_monotonic_atomic_and_terminal() {
        let empty = VmSeals::default();
        let request = VmSeals(VmSeals::WRITE.0 | VmSeals::SEAL.0);
        assert_eq!(empty.adding(request, true), Err(VmError::Busy));
        assert_eq!(empty, VmSeals(0));
        assert_eq!(empty.adding(VmSeals(32), false), Err(VmError::Invalid));

        let future = empty.adding(VmSeals::FUTURE_WRITE, true).unwrap();
        assert!(future.blocks_new_writes());
        assert_eq!(future.adding(VmSeals::WRITE, true), Err(VmError::Busy));
        assert_eq!(future.adding(VmSeals(0), true), Ok(future));
        assert_eq!(future.adding(VmSeals::FUTURE_WRITE, true), Ok(future));
        let fixed = future
            .adding(VmSeals(VmSeals::GROW.0 | VmSeals::SHRINK.0), true)
            .unwrap();
        let sealed = fixed.adding(request, false).unwrap();
        assert_eq!(sealed.0, 31);
        assert_eq!(sealed.adding(VmSeals(0), false), Err(VmError::Denied));
        assert_eq!(sealed.adding(VmSeals::WRITE, false), Err(VmError::Denied));
        assert_eq!(sealed.adding(VmSeals(32), false), Err(VmError::Invalid));
    }

    #[test]
    fn only_content_seals_block_new_writes() {
        assert!(!VmSeals::default().blocks_new_writes());
        assert!(
            !VmSeals(VmSeals::SEAL.0 | VmSeals::GROW.0 | VmSeals::SHRINK.0).blocks_new_writes()
        );
        assert!(VmSeals::WRITE.blocks_new_writes());
        assert!(VmSeals::FUTURE_WRITE.blocks_new_writes());
    }
}
