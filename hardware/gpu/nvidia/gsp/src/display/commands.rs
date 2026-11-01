// SPDX-License-Identifier: GPL-2.0-only
//! Source-derived C77D init/ownership and C67E surface assembly. No modeset yet.
use super::{header, notifier_words, probe_words, timing::Timing};
use crate::{Error, Result, raw};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug)]
pub struct Topology {
    pub windows: u32,
    pub heads: u32,
}
impl Topology {
    pub fn new(windows: u32, heads: u32) -> Result<Self> {
        if windows & 1 == 0 || windows & !0xff != 0 || heads == 0 || heads > 8 {
            return Err(Error::Unsupported);
        }
        for window in 0..8 {
            if windows & (1 << window) != 0 && window / 2 >= heads {
                return Err(Error::Unsupported);
            }
        }
        Ok(Self { windows, heads })
    }
}

#[derive(Default)]
pub struct Push {
    words: Vec<u32>,
}
impl Push {
    pub fn words(&self) -> &[u32] {
        &self.words
    }
    pub fn put(&self) -> u32 {
        self.words.len() as u32
    }
    pub fn extend(&mut self, words: &[u32]) -> Result<()> {
        if self
            .words
            .len()
            .checked_add(words.len())
            .is_none_or(|n| n > raw::HORN_DISP_PUT_MASK as usize)
        {
            return Err(Error::Range);
        }
        self.words
            .try_reserve(words.len())
            .map_err(|_| Error::Exhausted)?;
        self.words.extend_from_slice(words);
        Ok(())
    }
    pub fn method(&mut self, method: u32, values: &[u32]) -> Result<()> {
        let count = u32::try_from(values.len()).map_err(|_| Error::Range)?;
        let hdr = header(method, count)?;
        // Validate capacity before writing either the header or data.
        if self
            .words
            .len()
            .checked_add(values.len() + 1)
            .is_none_or(|n| n > raw::HORN_DISP_PUT_MASK as usize)
        {
            return Err(Error::Range);
        }
        self.words
            .try_reserve(values.len() + 1)
            .map_err(|_| Error::Exhausted)?;
        self.words.push(hdr);
        self.words.extend_from_slice(values);
        Ok(())
    }
}
pub fn notifier_control(offset: u32) -> Result<u32> {
    if offset & 15 != 0 || offset / 16 > raw::HORN_NOTIFIER_OFFSET_MASK {
        return Err(Error::Range);
    }
    Ok(raw::HORN_NOTIFIER_ENABLED | ((offset / 16) << raw::HORN_NOTIFIER_OFFSET_SHIFT))
}

pub const OWNER_NOTIFIER: usize = 16; // fresh 16-byte slot, never reset/reused
pub struct Core {
    pub push: Push,
    pub topology: Topology,
}
impl Core {
    /// corec57d_init followed by corec37d_wndw_owner and a notifier UPDATE.
    /// Static ownership must be committed *before* programming window formats.
    pub fn new(topology: Topology, notifier: u32) -> Result<Self> {
        let topology = Topology::new(topology.windows, topology.heads)?;
        let mut push = Push::default();
        push.extend(&probe_words()?)?;
        push.extend(&notifier_words(notifier)?)?;
        push.method(raw::NVC77D_SET_CONTEXT_DMA_NOTIFIER, &[notifier])?;
        for i in 0..8 {
            if topology.windows & (1 << i) == 0 {
                continue;
            }
            let stride = i * raw::HORN_WINDOW_STRIDE;
            push.method(
                raw::HORN_WINDOW_FORMAT_BOUNDS + stride,
                &[raw::HORN_WINDOW_RGB_FORMATS, 0],
            )?;
            push.method(
                raw::HORN_WINDOW_USAGE_BOUNDS + stride,
                &[raw::HORN_WINDOW_BOUNDS],
            )?;
        }
        for i in 0..8 {
            if topology.windows & (1 << i) == 0 {
                continue;
            }
            push.method(
                raw::HORN_WINDOW_CONTROL + i * raw::HORN_WINDOW_STRIDE,
                &[(i / 2) << raw::HORN_WINDOW_OWNER_SHIFT],
            )?;
        }
        push.method(
            raw::NVC77D_SET_NOTIFIER_CONTROL,
            &[notifier_control(OWNER_NOTIFIER as u32)?],
        )?;
        push.method(raw::NVC77D_SET_INTERLOCK_FLAGS, &[0, 0])?;
        push.method(raw::NVC77D_UPDATE, &[raw::HORN_DISP_UPDATE])?;
        push.method(
            raw::NVC77D_SET_NOTIFIER_CONTROL,
            &[raw::HORN_DISP_NOTIFIER_DISABLED],
        )?;
        Ok(Self { push, topology })
    }
}

pub fn window(t: &Timing, image_handle: u32) -> Result<Push> {
    if image_handle == 0
        || t.width == 0
        || t.height == 0
        || t.pitch & 63 != 0
        || t.pitch / 64 > raw::HORN_WINDOW_PITCH_MASK
        || t.pitch < u32::from(t.width) * 4
        || u32::from(t.width) > raw::HORN_WINDOW_SIZE_W_MASK
        || u32::from(t.height) > raw::HORN_WINDOW_SIZE_H_MASK
    {
        return Err(Error::Range);
    }
    let size = (u32::from(t.width) << raw::HORN_WINDOW_SIZE_W_SHIFT)
        | (u32::from(t.height) << raw::HORN_WINDOW_SIZE_H_SHIFT);
    let mut push = Push::default();
    push.extend(&[0; 4])?;
    // Clear optional sync/LUT state, then identity CSC, as in wndwc57e_csc_clr.
    push.method(raw::NVC57E_SET_CONTEXT_DMA_NOTIFIER, &[0])?;
    push.method(raw::NVC57E_SET_CONTEXT_DMA_SEMAPHORE, &[0])?;
    push.method(raw::NVC57E_SET_CONTEXT_DMA_ILUT, &[0])?;
    push.method(
        raw::NVC57E_SET_FMT_COEFFICIENT_C00,
        &[0x10000, 0, 0, 0, 0, 0x10000, 0, 0, 0, 0, 0x10000, 0],
    )?;
    push.method(
        raw::NVC57E_SET_COMPOSITION_CONTROL,
        &[
            raw::HORN_WINDOW_BLEND,
            raw::HORN_WINDOW_ALPHA,
            raw::HORN_WINDOW_FACTORS,
            raw::HORN_WINDOW_KEY,
            raw::HORN_WINDOW_KEY,
            raw::HORN_WINDOW_KEY,
            raw::HORN_WINDOW_KEY,
        ],
    )?;
    // wndwc67e_image_set, NOT wndwc57e_image_set: no legacy MEMORY_LAYOUT bit.
    push.method(raw::NVC57E_SET_PRESENT_CONTROL, &[0])?;
    push.method(
        raw::NVC57E_SET_SIZE,
        &[
            size,
            0,
            raw::HORN_WINDOW_PARAMS,
            (t.pitch / 64) << raw::HORN_WINDOW_PITCH_SHIFT,
        ],
    )?;
    // Upstream image.handle is an array: the final PUSH_MTHD argument 1
    // is its element count, not a second context-DMA handle.
    push.method(raw::HORN_WINDOW_ISO, &[image_handle])?;
    push.method(raw::HORN_WINDOW_OFFSET, &[0])?; // relative to bounded DMA descriptor
    push.method(raw::HORN_WINDOW_POINT_IN, &[0])?;
    push.method(raw::NVC57E_SET_SIZE_IN, &[size])?;
    push.method(raw::NVC57E_SET_SIZE_OUT, &[size])?;
    // Deliberately no UPDATE: no scanout until DP/head and interlocks are ready.
    Ok(push)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn methods(words: &[u32]) -> Vec<(u32, &[u32])> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < words.len() {
            if words[i] == 0 {
                i += 1;
                continue;
            }
            let n = (words[i] >> raw::HORN_DISP_COUNT_SHIFT) as usize;
            out.push((
                words[i] & raw::HORN_DISP_METHOD_MASK,
                &words[i + 1..i + 1 + n],
            ));
            i += n + 1;
        }
        out
    }
    #[test]
    fn full_topology_init_retains_probe_and_uses_fresh_completion_slot() {
        let core = Core::new(Topology::new(255, 4).unwrap(), 0x1000).unwrap();
        assert_eq!(core.push.put(), 89);
        assert_eq!(&core.push.words()[..11], &probe_words().unwrap());
        assert_eq!(&core.push.words()[11..22], &notifier_words(0x1000).unwrap());
        let m = methods(&core.push.words()[22..]);
        for i in 0..8 {
            assert!(m.contains(&(
                raw::HORN_WINDOW_CONTROL + i * raw::HORN_WINDOW_STRIDE,
                &[(i / 2) << raw::HORN_WINDOW_OWNER_SHIFT][..]
            )));
            assert!(m.contains(&(
                raw::HORN_WINDOW_FORMAT_BOUNDS + i * raw::HORN_WINDOW_STRIDE,
                &[15, 0][..]
            )));
        }
        assert_eq!(m[m.len() - 4].1, &[raw::HORN_NOTIFIER_ENABLED | 16]);
    }
    #[test]
    fn topology_never_assigns_an_absent_head_or_unknown_window() {
        assert!(Topology::new(255, 3).is_err());
        assert!(Topology::new(256, 4).is_err());
        assert!(Topology::new(0, 4).is_err());
        assert!(Topology::new(3, 1).is_ok());
        assert!(Topology::new(5, 1).is_err());
        let core = Core::new(Topology::new(3, 1).unwrap(), 0x1000).unwrap();
        assert_eq!(core.push.put(), 47);
    }
    #[test]
    fn invalid_packet_does_not_partially_modify_ring() {
        let mut push = Push::default();
        assert!(push.method(0x201, &[1]).is_err());
        assert!(push.words().is_empty());
        assert!(push.extend(&[0; 1024]).is_err());
        assert!(push.words().is_empty());
        assert!(notifier_control(1).is_err());
        assert!(notifier_control(4096).is_err());
        assert!(notifier_control(4080).is_ok());
    }
}
