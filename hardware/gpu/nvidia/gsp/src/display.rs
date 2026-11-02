// SPDX-License-Identifier: GPL-2.0-only
//! AD102 core DMA command probe, following nouveau's corec37d_update(ntfy=false).
//! The initial prefixes establish consumption; the third requests completion.
pub mod commands;
pub mod dp;
pub mod head;
pub mod instance;
pub mod pramin;
pub mod surface;
pub mod timing;
use crate::{Error, Result, raw};

/// r535_chan_user in nouveau maps the core class's user registers here.
/// This is a BAR0 offset, not a physical address or an authority grant.
pub const CORE_USER: u32 = raw::NV_UDISP_FE_CHN_ASSY_BASEADR_CORE;
pub const PUT: u32 = raw::NV917C_PUT;
pub const GET: u32 = raw::NV917C_GET;

/// Nouveau's MMIO accessors use the legacy byte-pointer field (11:2),
/// not the logical C77D class PUT/GET field (9:0).
pub fn put_word(dwords: u32) -> Result<u32> {
    if dwords > raw::HORN_DISP_PUT_MASK {
        return Err(Error::Range);
    }
    Ok(dwords << raw::HORN_DISP_PUT_SHIFT)
}
pub fn get_dwords(word: u32) -> u32 {
    (word & raw::HORN_DISP_GET_MASK) >> raw::HORN_DISP_GET_SHIFT
}
pub const NOP_PUT: u32 = 4;
pub const PROBE_WORDS: usize = 11;
pub const UPDATE_PUT: u32 = PROBE_WORDS as u32;

pub fn header(method: u32, count: u32) -> Result<u32> {
    if count == 0 || count > raw::HORN_DISP_COUNT_MASK || method & !raw::HORN_DISP_METHOD_MASK != 0
    {
        return Err(Error::Range);
    }
    // The display method field occupies bits 13:2: the encoded byte method
    // stays in place, not shifted down to bit zero.
    Ok((count << raw::HORN_DISP_COUNT_SHIFT) | method)
}

/// Preload once, then publish two prefixes: NOPs first, then a non-interlocked
/// UPDATE with notifications explicitly disabled. PUT/GET are DWORD indices.
pub fn probe_words() -> Result<[u32; PROBE_WORDS]> {
    Ok([
        raw::NVC77D_DMA_DATA_NOP,
        raw::NVC77D_DMA_DATA_NOP,
        raw::NVC77D_DMA_DATA_NOP,
        raw::NVC77D_DMA_DATA_NOP,
        header(raw::NVC77D_SET_NOTIFIER_CONTROL, 1)?,
        raw::HORN_DISP_NOTIFIER_DISABLED,
        header(raw::NVC77D_SET_INTERLOCK_FLAGS, 2)?,
        0, // no cursor interlocks
        0, // no window interlocks (next consecutive method)
        header(raw::NVC77D_UPDATE, 1)?,
        raw::HORN_DISP_UPDATE,
    ])
}

pub const NOTIFIER_PUT: u32 = UPDATE_PUT + 11;

/// Append once before any publication. No live-ring rewriting or wraparound.
pub fn notifier_words(handle: u32) -> Result<[u32; 11]> {
    if handle == 0 {
        return Err(Error::Range);
    }
    Ok([
        header(raw::NVC77D_SET_CONTEXT_DMA_NOTIFIER, 1)?,
        handle,
        header(raw::NVC77D_SET_NOTIFIER_CONTROL, 1)?,
        raw::HORN_NOTIFIER_ENABLED,
        header(raw::NVC77D_SET_INTERLOCK_FLAGS, 2)?,
        0,
        0,
        header(raw::NVC77D_UPDATE, 1)?,
        raw::HORN_DISP_UPDATE,
        header(raw::NVC77D_SET_NOTIFIER_CONTROL, 1)?,
        raw::HORN_DISP_NOTIFIER_DISABLED,
    ])
}
pub fn notifier_finished(word: u32) -> Result<bool> {
    let status = (word >> raw::HORN_NOTIFIER_STATUS_SHIFT) & raw::HORN_NOTIFIER_STATUS_MASK;
    match status {
        raw::NV_DISP_NOTIFIER__0_STATUS_NOT_BEGUN | raw::NV_DISP_NOTIFIER__0_STATUS_BEGUN => {
            Ok(false)
        }
        raw::NV_DISP_NOTIFIER__0_STATUS_FINISHED => Ok(true),
        _ => Err(Error::State),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifier_is_enabled_only_for_one_update() {
        let words = notifier_words(0x1000).unwrap();
        assert_eq!(words[0], 0x0004_0208);
        assert_eq!(words[1], 0x1000);
        assert_eq!(words[10], 0);
        assert!(!notifier_finished(0).unwrap());
        assert!(
            !notifier_finished(
                raw::NV_DISP_NOTIFIER__0_STATUS_BEGUN << raw::HORN_NOTIFIER_STATUS_SHIFT
            )
            .unwrap()
        );
        assert!(
            notifier_finished(
                raw::NV_DISP_NOTIFIER__0_STATUS_FINISHED << raw::HORN_NOTIFIER_STATUS_SHIFT
            )
            .unwrap()
        );
        assert!(notifier_finished(u32::MAX).is_err());
        assert!(notifier_words(0).is_err());
    }
    #[test]
    fn headers_keep_byte_methods_and_bound_fields() {
        assert_eq!(header(0x218, 2).unwrap(), 0x0008_0218);
        assert!(header(0x219, 1).is_err());
        assert!(header(0x4000, 1).is_err());
        assert!(header(0x200, 0).is_err());
        assert!(header(0x200, 1024).is_err());
    }
    #[test]
    fn probe_is_two_bounded_prefixes_without_notifier_or_modeset() {
        let words = probe_words().unwrap();
        assert_eq!(&words[..NOP_PUT as usize], &[0; 4]);
        assert_eq!(
            &words[4..],
            &[0x0004_020c, 0, 0x0008_0218, 0, 0, 0x0004_0200, 1]
        );
        assert_eq!(
            raw::NVC77D_SET_WINDOW_INTERLOCK_FLAGS,
            raw::NVC77D_SET_INTERLOCK_FLAGS + 4
        );
        const {
            assert!(UPDATE_PUT <= raw::HORN_DISP_PUT_MASK);
        }
        assert_eq!(PUT, 0);
        assert_eq!(GET, 4);
        assert_eq!(put_word(UPDATE_PUT).unwrap(), 44);
        assert_eq!(get_dwords(44), UPDATE_PUT);
        assert!(put_word(1024).is_err());
    }
}
