//! DMA pins are physical backing references, not reservations or extra charges.
use crate::protocol::{VmError, VmProtection};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Pins {
    pub count: usize,
    pub writers: usize,
}
impl Pins {
    pub fn adding(
        self,
        access: VmProtection,
        ceiling: VmProtection,
        immutable: bool,
    ) -> Result<Self, VmError> {
        let rw = VmProtection::READ.0 | VmProtection::WRITE.0;
        if access.0 == 0 || access.0 & !rw != 0 {
            return Err(VmError::Invalid);
        }
        if access.0 & !ceiling.0 != 0 || access.0 & VmProtection::WRITE.0 != 0 && immutable {
            return Err(VmError::Denied);
        }
        Ok(Self {
            count: self.count.checked_add(1).ok_or(VmError::Exhausted)?,
            writers: self
                .writers
                .checked_add(usize::from(access.0 & VmProtection::WRITE.0 != 0))
                .ok_or(VmError::Exhausted)?,
        })
    }
    pub fn remove(&mut self, writer: bool) {
        assert!(self.count != 0 && (!writer || self.writers != 0));
        self.count -= 1;
        self.writers -= usize::from(writer);
    }
}

/// Trusted root token. Dropping it intentionally does not release the pin.
/// Physical addresses must never be exported to an ordinary driver.
#[must_use = "DMA backing stays pinned until explicitly released after acknowledged invalidation"]
pub struct Pin {
    pub(crate) memory: usize,
    pub(crate) badge: u64,
    pub(crate) writer: bool,
    pub(crate) pages: alloc::vec::Vec<u64>,
}
impl Pin {
    pub fn pages(&self) -> &[u64] {
        &self.pages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pins_obey_rights_and_immutability() {
        let rw = VmProtection(3);
        let empty = Pins::default();
        assert_eq!(
            empty.adding(VmProtection(0), rw, false),
            Err(VmError::Invalid)
        );
        assert_eq!(
            empty.adding(VmProtection::EXECUTE, rw, false),
            Err(VmError::Invalid)
        );
        assert_eq!(
            empty.adding(rw, VmProtection::READ, false),
            Err(VmError::Denied)
        );
        assert_eq!(empty.adding(rw, rw, true), Err(VmError::Denied));
        assert_eq!(
            empty.adding(VmProtection::WRITE, rw, true),
            Err(VmError::Denied)
        );
        let mut pins = empty.adding(rw, rw, false).unwrap();
        assert_eq!(pins.writers, 1);
        pins = pins.adding(VmProtection::READ, rw, true).unwrap();
        pins.remove(true);
        assert_eq!(pins.count, 1);
        assert_eq!(pins.writers, 0);
        pins.remove(false);
        assert_eq!(pins, empty);
        assert_eq!(
            Pins {
                count: usize::MAX,
                writers: 0
            }
            .adding(VmProtection::READ, rw, false),
            Err(VmError::Exhausted)
        );
    }
}
