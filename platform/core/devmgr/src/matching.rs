//! PCI identity matching only. Selecting a record never grants device authority.
use crate::protocol::PciDevice;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PciMatch {
    pub vendor: Option<u16>,
    pub device: Option<u16>,
    /// Packed class:subclass:programming-interface, in the low 24 bits.
    pub class: u32,
    pub class_mask: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchError {
    Invalid,
    Ambiguous,
}
impl PciMatch {
    pub fn validate(self) -> Result<(), MatchError> {
        if self.class_mask & !0xffffff != 0
            || self.class & !self.class_mask != 0
            || self.vendor.is_none() && self.device.is_none() && self.class_mask == 0
        {
            return Err(MatchError::Invalid);
        }
        Ok(())
    }
    pub fn matches(self, device: &PciDevice) -> bool {
        let class =
            (device.class_code << 16) | (device.subclass << 8) | device.programming_interface;
        self.vendor.is_none_or(|v| device.vendor_id == v as u32)
            && self.device.is_none_or(|v| device.device_id == v as u32)
            && class & self.class_mask == self.class
    }
}
/// Refuse ambiguous configurations instead of silently depending on declaration order.
pub fn select(rules: &[PciMatch], device: &PciDevice) -> Result<Option<usize>, MatchError> {
    let mut selected = None;
    for (index, rule) in rules.iter().enumerate() {
        rule.validate()?;
        if rule.matches(device) {
            if selected.is_some() {
                return Err(MatchError::Ambiguous);
            }
            selected = Some(index);
        }
    }
    Ok(selected)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_and_class_masks() {
        let mut d = PciDevice::default();
        d.vendor_id = 0x1af4;
        d.device_id = 0x1043;
        d.class_code = 7;
        d.subclass = 0x80;
        let exact = PciMatch {
            vendor: Some(0x1af4),
            device: Some(0x1043),
            ..Default::default()
        };
        let class = PciMatch {
            class: 7 << 16,
            class_mask: 0xff0000,
            ..Default::default()
        };
        assert_eq!(select(&[exact], &d), Ok(Some(0)));
        assert!(class.matches(&d));
        assert_eq!(select(&[exact, class], &d), Err(MatchError::Ambiguous));
        d.vendor_id = 0x8086;
        assert_eq!(select(&[exact], &d), Ok(None));
    }
    #[test]
    fn invalid_rules_fail_closed() {
        assert_eq!(PciMatch::default().validate(), Err(MatchError::Invalid));
        assert!(
            PciMatch {
                class: 1,
                class_mask: 0xff0000,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            PciMatch {
                class: 0,
                class_mask: 1 << 24,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
