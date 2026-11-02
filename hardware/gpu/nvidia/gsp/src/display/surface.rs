// SPDX-License-Identifier: GPL-2.0-only
//! CPU-built opaque ARGB8888 diagnostic pattern. No CE, BAR1, GPU VA or tiling.
use super::timing::Timing;
use crate::{Error, Result, raw, rm};

pub fn allocation_bytes(t: &Timing) -> Result<usize> {
    let bytes = (t.pitch as usize)
        .checked_mul(usize::from(t.height))
        .ok_or(Error::Range)?;
    if t.width == 0
        || t.height == 0
        || t.pitch & 255 != 0
        || t.pitch < u32::from(t.width) * 4
        || bytes != t.bytes
        || bytes > 64 * 1024 * 1024
    {
        return Err(Error::Range);
    }
    bytes
        .checked_add(0xffff)
        .map(|v| v & !0xffff)
        .ok_or(Error::Range)
}
/// Request an explicitly contiguous, pitch-layout, uncompressed VRAM allocation.
/// Keep instance-memory allocation policy separate from the scanout policy.
pub fn request(owner: u32, t: &Timing) -> Result<raw::NV_MEMORY_ALLOCATION_PARAMS> {
    let mut params = rm::vram_request(owner, allocation_bytes(t)? as u64, 0x10000)?;
    params.attr = raw::HORN_SURFACE_ATTR;
    params.format = raw::HORN_SURFACE_KIND_PITCH;
    Ok(params)
}

/// Allocation 'format' is a PTE kind, not the pitch/block-linear selector.
/// memmgrChooseKind_TU102 may return GENERIC_MEMORY for an IMAGE even when
/// ATTR_FORMAT is PITCH. memmgrIsSurfaceBlockLinear_TU102 uses ATTR_FORMAT.
/// Permit only the published, uncompressed pitch/generic kinds and verify
/// layout, compression, location and physical contiguity independently.
pub fn validate_allocation(t: &Timing, reply: &raw::NV_MEMORY_ALLOCATION_PARAMS) -> Result<()> {
    validate_memory(allocation_bytes(t)? as u64, reply)
}

pub fn validate_memory(requested: u64, reply: &raw::NV_MEMORY_ALLOCATION_PARAMS) -> Result<()> {
    if requested == 0 || requested & 0xffff != 0 || requested > 64 * 1024 * 1024 {
        return Err(Error::Range);
    }
    if reply.size < requested
        || reply.size > 64 * 1024 * 1024
        || reply.size & 0xffff != 0
        || reply.offset & 0xffff != 0
        || reply
            .limit
            .checked_add(1)
            .is_none_or(|length| length < reply.size)
        || reply
            .offset
            .checked_add(reply.size)
            .is_none_or(|end| end > 1 << 40)
    {
        return Err(Error::Range);
    }
    if reply.attr & raw::HORN_SURFACE_ATTR_MASK != raw::HORN_SURFACE_ATTR
        || !matches!(
            reply.format,
            raw::HORN_SURFACE_KIND_PITCH | raw::HORN_SURFACE_KIND_GENERIC
        )
    {
        return Err(Error::Unsupported);
    }
    Ok(())
}

pub fn row(t: &Timing, y: u16, out: &mut [u32]) -> Result<()> {
    allocation_bytes(t)?;
    if y >= t.height || out.len() != t.pitch as usize / 4 {
        return Err(Error::Range);
    }
    out.fill(0); // opaque black padding is unused; no stale VRAM gets published
    const BARS: [u32; 8] = [
        0xffffffff, 0xffffff00, 0xff00ffff, 0xff00ff00, 0xffff00ff, 0xffff0000, 0xff0000ff,
        0xff000000,
    ];
    for x in 0..usize::from(t.width) {
        out[x] = if x < 3
            || x + 3 >= usize::from(t.width)
            || y < 3
            || u32::from(y) + 3 >= u32::from(t.height)
        {
            0xffff0000 // red border exposes clipping and exact viewport edges
        } else if u32::from(y) * 4 < u32::from(t.height) * 3 {
            BARS[x * 8 / usize::from(t.width)]
        } else if ((x / 16) ^ (usize::from(y) / 16)) & 1 == 0 {
            0xffffffff
        } else {
            0xff000000
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edid::Mode;
    pub fn timing() -> Timing {
        Timing::from_mode(&Mode {
            preferred: true,
            has_border: false,
            h_sync_positive: Some(true),
            v_sync_positive: Some(true),
            width: 2560,
            height: 1440,
            refresh_mhz: None,
            interlaced: false,
            pixel_clock_khz: Some(497750),
            h_total: Some(2720),
            h_front_porch: 48,
            h_sync: 32,
            h_back_porch: Some(80),
            v_total: Some(1537),
            v_front_porch: 3,
            v_sync: 5,
            v_back_porch: Some(89),
        })
        .unwrap()
    }
    #[test]
    fn allocation_policy_separates_pte_kind_from_surface_layout() {
        let t = timing();
        let params = request(0xc2d00000, &t).unwrap();
        assert_eq!(params.attr, raw::HORN_SURFACE_ATTR);
        assert_eq!(params.alignment, 0x10000);
        assert_eq!(params.size, 0xe10000);
        let mut reply = params;
        reply.offset = 0x1ee600000;
        reply.size = 0x1000000; // observed RM rounding, not an error
        reply.limit = reply.size - 1;
        reply.format = raw::HORN_SURFACE_KIND_GENERIC;
        assert!(validate_allocation(&t, &reply).is_ok());
        reply.format = raw::HORN_SURFACE_KIND_PITCH;
        assert!(validate_allocation(&t, &reply).is_ok());
        reply.format = 8; // compressed kind must not pass even with pitch attrs
        assert!(matches!(
            validate_allocation(&t, &reply),
            Err(Error::Unsupported)
        ));
        reply.format = raw::HORN_SURFACE_KIND_GENERIC;
        let attr = reply.attr;
        for invalid in [attr | 0x00020000, attr | 0x00001000, 0, 0x08000000] {
            // SDK golden encodings: BLOCK_LINEAR, COMPR_REQUIRED, default and noncontiguous.
            reply.attr = invalid;
            assert!(matches!(
                validate_allocation(&t, &reply),
                Err(Error::Unsupported)
            ));
        }
        reply.attr = attr;
        reply.limit = params.size - 1; // must cover *all* rounded descriptor backing
        assert!(matches!(validate_allocation(&t, &reply), Err(Error::Range)));
        reply.limit = u64::MAX;
        assert!(matches!(validate_allocation(&t, &reply), Err(Error::Range)));
        reply.limit = reply.size - 1;
        reply.offset = (1 << 40) - 0x10000;
        assert!(matches!(validate_allocation(&t, &reply), Err(Error::Range)));
    }

    #[test]
    fn every_active_pixel_is_opaque_and_padding_is_initialized() {
        let t = timing();
        let mut line = alloc::vec![0xdeadbeef;t.pitch as usize/4];
        row(&t, 0, &mut line).unwrap();
        assert!(line.iter().all(|&v| v == 0xffff0000));
        row(&t, 10, &mut line).unwrap();
        for (i, &expected) in [
            0xffffffff, 0xffffff00, 0xff00ffff, 0xff00ff00, 0xffff00ff, 0xffff0000, 0xff0000ff,
            0xff000000,
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(line[i * 320 + 160], expected);
        }
        assert!(line.iter().all(|v| v >> 24 == 255));
        row(&t, 1200, &mut line).unwrap();
        assert_ne!(line[32], line[48]);
        assert_eq!(allocation_bytes(&t).unwrap(), 0xe10000);
        assert!(row(&t, 1440, &mut line).is_err());
        assert!(row(&t, 0, &mut line[..1]).is_err());
        let mut bad = t;
        bad.bytes = 1;
        assert!(allocation_bytes(&bad).is_err());
    }
    #[test]
    fn pitch_padding_is_not_an_uninitialized_tail() {
        let mut t = timing();
        t.width = 2559;
        let mut line = alloc::vec![0xdeadbeef;t.pitch as usize/4];
        row(&t, 10, &mut line).unwrap();
        assert_eq!(line[2559], 0);
    }
    #[test]
    fn window_assembly_is_bounded_and_has_no_update() {
        let t = timing();
        let push = super::super::commands::window(&t, 0x2000).unwrap();
        assert_eq!(&push.words()[..4], &[0; 4]);
        let mut i = 4;
        while i < push.words().len() {
            let header = push.words()[i];
            let method = header & crate::raw::HORN_DISP_METHOD_MASK;
            assert_ne!(method, crate::raw::NVC57E_UPDATE);
            let count = (header >> crate::raw::HORN_DISP_COUNT_SHIFT) as usize;
            for j in 0..count {
                let m = method + j as u32 * 4;
                if m == crate::raw::NVC57E_SET_STORAGE {
                    assert_eq!(push.words()[i + 1 + j], 0);
                }
                if m == crate::raw::HORN_WINDOW_ISO {
                    assert_eq!(push.words()[i + 1 + j], 0x2000);
                }
                assert_ne!(m, crate::raw::HORN_WINDOW_ISO + 4); // one array element, not handle=1
                if m == crate::raw::HORN_WINDOW_OFFSET {
                    assert_eq!(push.words()[i + 1 + j], 0);
                }
            }
            i += 1 + count;
        }
        assert_eq!(i, push.words().len());
        assert!(super::super::commands::window(&t, 0).is_err());
    }
}
