//! Trusted per-assignment DMA policy. Pending reservations consume quota too.
use crate::protocol::DeviceError;
#[derive(Clone, Copy, Debug)]
pub struct DmaPolicy {
    pub bytes: u64,
    pub objects: usize,
}
impl DmaPolicy {
    pub const DEFAULT: Self = Self {
        bytes: 12 * 65536,
        objects: 12,
    };
    pub fn validate(self) -> Result<(), DeviceError> {
        if self.bytes == 0
            || self.bytes > 64 * 1024 * 1024
            || self.bytes & 4095 != 0
            || self.objects == 0
            || self.objects > 64
        {
            return Err(DeviceError::Invalid);
        }
        Ok(())
    }
    pub fn admit(self, used: u64, objects: usize, length: u64) -> Result<(), DeviceError> {
        self.validate()?;
        if length == 0 || length & 4095 != 0 {
            return Err(DeviceError::Invalid);
        }
        if objects >= self.objects || used.checked_add(length).is_none_or(|n| n > self.bytes) {
            return Err(DeviceError::Exhausted);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_bytes_and_objects_are_bounded_without_overflow() {
        let p = DmaPolicy::DEFAULT;
        assert_eq!(p.admit(0, 0, 65536), Ok(()));
        assert!(p.admit(p.bytes, 0, 4096).is_err());
        assert!(p.admit(0, 12, 4096).is_err());
        assert!(p.admit(u64::MAX, 0, 4096).is_err());
        assert!(p.admit(0, 0, 4097).is_err());
        assert!(
            DmaPolicy {
                bytes: 65 * 1024 * 1024,
                objects: 12
            }
            .validate()
            .is_err()
        );
    }
}
