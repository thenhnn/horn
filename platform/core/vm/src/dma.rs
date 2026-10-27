//! DMA pins are physical backing references, not reservations or extra charges.
use crate::protocol::{VmError, VmProtection, VmSeals};

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
        seals: VmSeals,
    ) -> Result<Self, VmError> {
        let rw = VmProtection::READ.0 | VmProtection::WRITE.0;
        if access.0 == 0 || access.0 & !rw != 0 {
            return Err(VmError::Invalid);
        }
        if access.0 & !ceiling.0 != 0
            || access.0 & VmProtection::WRITE.0 != 0 && seals.blocks_new_writes()
        {
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
    fn pins_obey_ceiling_and_seals_without_a_second_ram_charge() {
        let rw = VmProtection(VmProtection::READ.0 | VmProtection::WRITE.0);
        let empty = Pins::default();
        assert_eq!(
            empty.adding(VmProtection(0), rw, VmSeals(0)),
            Err(VmError::Invalid)
        );
        assert_eq!(
            empty.adding(VmProtection::EXECUTE, rw, VmSeals(0)),
            Err(VmError::Invalid)
        );
        assert_eq!(
            empty.adding(rw, VmProtection::READ, VmSeals(0)),
            Err(VmError::Denied)
        );
        for seals in [VmSeals::WRITE, VmSeals::FUTURE_WRITE] {
            assert_eq!(empty.adding(rw, rw, seals), Err(VmError::Denied));
            assert_eq!(
                empty.adding(VmProtection::READ, rw, seals).unwrap().writers,
                0
            );
        }
        let mut pins = empty.adding(rw, rw, VmSeals(0)).unwrap();
        assert_eq!(
            VmSeals(0).adding(VmSeals::WRITE, pins.writers != 0),
            Err(VmError::Busy)
        );
        pins = pins
            .adding(VmProtection::READ, rw, VmSeals::FUTURE_WRITE)
            .unwrap();
        pins.remove(true);
        assert_eq!(pins.count, 1);
        assert_eq!(
            VmSeals(0).adding(VmSeals::WRITE, pins.writers != 0),
            Ok(VmSeals::WRITE)
        );
        pins.remove(false);
        assert_eq!(pins, empty);
        assert_eq!(
            Pins {
                count: usize::MAX,
                writers: 0
            }
            .adding(VmProtection::READ, rw, VmSeals(0)),
            Err(VmError::Exhausted)
        );
    }
}
