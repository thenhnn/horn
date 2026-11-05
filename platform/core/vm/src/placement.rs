//! Bounded reservation placement; the region registry is the source of truth.
use crate::protocol::VmError;
use core::ops::Range;

pub(crate) fn choose(
    window: Range<usize>,
    length: usize,
    alignment: usize,
    fixed: Option<usize>,
    occupied: impl Iterator<Item = Range<usize>> + Clone,
) -> Result<Range<usize>, VmError> {
    if length == 0 || !alignment.is_power_of_two() {
        return Err(VmError::Invalid);
    }
    let align = |value: usize| {
        value
            .checked_add(alignment - 1)
            .map(|v| v & !(alignment - 1))
    };
    let mut start = match fixed {
        Some(address) if address % alignment == 0 => address,
        Some(_) => return Err(VmError::Invalid),
        None => align(window.start).ok_or(VmError::Exhausted)?,
    };
    loop {
        let end = start.checked_add(length).ok_or(VmError::Invalid)?;
        if start < window.start || end > window.end {
            return Err(if fixed.is_some() {
                VmError::Invalid
            } else {
                VmError::Exhausted
            });
        }
        let collision = occupied
            .clone()
            .filter(|r| r.start < end && start < r.end)
            .map(|r| r.end)
            .max();
        match collision {
            None => return Ok(start..end),
            Some(_) if fixed.is_some() => return Err(VmError::Overlap),
            Some(end) => start = align(end).ok_or(VmError::Exhausted)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_and_anywhere_share_reservations_without_replacement() {
        let used = [0x2000..0x4000, 0x6000..0x7000];
        assert_eq!(
            choose(
                0x1000..0x9000,
                0x2000,
                0x1000,
                None,
                used.clone().into_iter()
            ),
            Ok(0x4000..0x6000)
        );
        assert_eq!(
            choose(
                0x1000..0x9000,
                0x1000,
                0x1000,
                Some(0x3000),
                used.clone().into_iter()
            ),
            Err(VmError::Overlap)
        );
        assert_eq!(
            choose(
                0x1000..0x9000,
                0x1000,
                0x1000,
                Some(0x4000),
                used.into_iter()
            ),
            Ok(0x4000..0x5000)
        );
    }
    #[test]
    fn bounds_alignment_and_overflow_are_checked() {
        assert_eq!(
            choose(0x1000..0x9000, 0x1000, 0x1000, Some(0), core::iter::empty()),
            Err(VmError::Invalid)
        );
        assert_eq!(
            choose(
                0x1000..0x9000,
                0x1000,
                0x1000,
                Some(0x1800),
                core::iter::empty()
            ),
            Err(VmError::Invalid)
        );
        assert_eq!(
            choose(
                0x1000..0x9000,
                usize::MAX,
                0x1000,
                Some(0x2000),
                core::iter::empty()
            ),
            Err(VmError::Invalid)
        );
        assert_eq!(
            choose(0x1000..0x9000, 0x1000, 0x4000, None, core::iter::empty()),
            Ok(0x4000..0x5000)
        );
    }
}
