// SPDX-License-Identifier: GPL-2.0-only
//! Copy-engine methods and pushbuffer encoding for `AMPERE_DMA_COPY_B`.
//!
//! Method numbers and field positions come from NVIDIA's
//! `class/clc7b5.h` (570.144). A pushbuffer is a sequence of method headers
//! followed by their arguments; the header carries the opcode, the argument
//! count and the method offset in dwords.

use alloc::vec::Vec;

use crate::{Error, Result};

/// NVC56F host DMA_INCR opcode (not CE's older DMA_METHOD packet format).
pub const OPCODE_METHOD: u32 = 1;
/// NVC56F host DMA_NONINCR opcode.
pub const OPCODE_NONINC: u32 = 3;

const COUNT_SHIFT: u32 = crate::raw::HORN_CE_COUNT_SHIFT;
const OFFSET_SHIFT: u32 = 2;
const MAX_COUNT: u32 = crate::raw::HORN_CE_COUNT_MASK;

/// A pushbuffer word that writes `count` consecutive methods starting at
/// `method`, or a single non-incrementing write.
pub fn header(method: u32, count: u32) -> Result<u32> {
    if count == 0 || count > MAX_COUNT || method & 3 != 0 || method > 0x3ffc {
        return Err(Error::Range);
    }
    Ok(crate::raw::HORN_CE_INCR | (count << COUNT_SHIFT) | (method >> OFFSET_SHIFT))
}

pub fn header_noninc(method: u32) -> Result<u32> {
    if method & 3 != 0 || method > 0x3ffc {
        return Err(Error::Range);
    }
    Ok(crate::raw::HORN_CE_NONINCR | (1 << COUNT_SHIFT) | (method >> OFFSET_SHIFT))
}

/// `AMPERE_DMA_COPY_B` method numbers.
pub const SET_SEMAPHORE_A: u32 = crate::raw::NVC7B5_SET_SEMAPHORE_A;
pub const SET_SEMAPHORE_B: u32 = crate::raw::NVC7B5_SET_SEMAPHORE_B;
pub const SET_SEMAPHORE_PAYLOAD: u32 = crate::raw::NVC7B5_SET_SEMAPHORE_PAYLOAD;
pub const SET_SRC_PHYS_MODE: u32 = crate::raw::NVC7B5_SET_SRC_PHYS_MODE;
pub const SET_DST_PHYS_MODE: u32 = crate::raw::NVC7B5_SET_DST_PHYS_MODE;
pub const LAUNCH_DMA: u32 = crate::raw::NVC7B5_LAUNCH_DMA;
pub const OFFSET_IN_UPPER: u32 = crate::raw::NVC7B5_OFFSET_IN_UPPER;
pub const OFFSET_IN_LOWER: u32 = crate::raw::NVC7B5_OFFSET_IN_LOWER;
pub const OFFSET_OUT_UPPER: u32 = crate::raw::NVC7B5_OFFSET_OUT_UPPER;
pub const OFFSET_OUT_LOWER: u32 = crate::raw::NVC7B5_OFFSET_OUT_LOWER;
pub const PITCH_IN: u32 = crate::raw::NVC7B5_PITCH_IN;
pub const PITCH_OUT: u32 = crate::raw::NVC7B5_PITCH_OUT;
pub const SET_DST_BLOCK_SIZE: u32 = crate::raw::NVC7B5_SET_DST_BLOCK_SIZE;
pub const SET_DST_WIDTH: u32 = crate::raw::NVC7B5_SET_DST_WIDTH;
pub const SET_DST_HEIGHT: u32 = crate::raw::NVC7B5_SET_DST_HEIGHT;
pub const SET_DST_DEPTH: u32 = crate::raw::NVC7B5_SET_DST_DEPTH;
pub const SET_SRC_BLOCK_SIZE: u32 = crate::raw::NVC7B5_SET_SRC_BLOCK_SIZE;
pub const SET_SRC_WIDTH: u32 = crate::raw::NVC7B5_SET_SRC_WIDTH;
pub const SET_SRC_HEIGHT: u32 = crate::raw::NVC7B5_SET_SRC_HEIGHT;
pub const SET_SRC_DEPTH: u32 = crate::raw::NVC7B5_SET_SRC_DEPTH;

/// `NVC7B5_SET_*_PHYS_MODE_TARGET_COHERENT_SYSMEM`.
pub const PHYS_MODE_COHERENT_SYSMEM: u32 =
    crate::raw::NVC7B5_SET_SRC_PHYS_MODE_TARGET_COHERENT_SYSMEM;

#[cfg(test)]
const LAUNCH_SEMAPHORE_ONE_WORD: u32 = 1 << 3;
#[cfg(test)]
const LAUNCH_SRC_LAYOUT_PITCH: u32 = 1 << 7;
#[cfg(test)]
const LAUNCH_DST_TYPE_PHYSICAL: u32 = 1 << 13;

/// Value the copy engine writes to the fence when the copy retires. Any value
/// the CPU never writes works as a "done" marker.
pub const FENCE_PAYLOAD: u32 = 0x00c0_ffee;

/// One linear copy between device-visible system-memory buffers, fenced by a
/// one-word semaphore write.
#[derive(Clone, Copy, Debug)]
pub struct Copy {
    pub source: u64,
    pub destination: u64,
    pub bytes: u32,
    pub fence: u64,
}

/// Append the pushbuffer words for one copy.
///
/// The engine writes the fence payload after the last byte lands, so a CPU poll
/// of the fence word is a real completion signal rather than a timeout guess.
pub fn encode_copy(copy: &Copy, out: &mut Vec<u32>) -> Result<()> {
    encode_transfer(copy, Target::System, Target::System, true, out)
}
#[derive(Clone, Copy)]
pub enum Target {
    System,
    Vram,
}
impl Target {
    fn value(self) -> u32 {
        match self {
            Self::System => PHYS_MODE_COHERENT_SYSMEM,
            Self::Vram => crate::raw::NVC7B5_SET_SRC_PHYS_MODE_TARGET_LOCAL_FB,
        }
    }
}
/// Ordered physical transfer. Only the final launch releases the virtual fence.
pub fn encode_transfer(
    copy: &Copy,
    source: Target,
    destination: Target,
    release: bool,
    out: &mut Vec<u32>,
) -> Result<()> {
    if copy.bytes == 0
        || copy.bytes & 3 != 0
        || (copy.source | copy.destination | copy.fence) & 3 != 0
        || copy
            .source
            .checked_add(copy.bytes as u64)
            .is_none_or(|v| v > 1 << 40)
        || copy
            .destination
            .checked_add(copy.bytes as u64)
            .is_none_or(|v| v > 1 << 40)
        || (release && copy.fence >= 1 << 49)
    {
        return Err(Error::Range);
    }
    let start = out.len();
    let mut push = |method: u32, value: u32| -> Result<()> {
        out.try_reserve(2).map_err(|_| Error::Exhausted)?;
        out.push(header(method, 1)?);
        out.push(value);
        Ok(())
    };
    push(SET_SRC_PHYS_MODE, source.value())?;
    push(SET_DST_PHYS_MODE, destination.value())?;
    // One GOB block with a linear (pitch) layout: width is the byte count,
    // height and depth are one line, and the pitch equals the width.
    push(SET_SRC_BLOCK_SIZE, 0)?;
    push(SET_SRC_WIDTH, copy.bytes)?;
    push(SET_SRC_HEIGHT, 1)?;
    push(SET_SRC_DEPTH, 1)?;
    push(SET_DST_BLOCK_SIZE, 0)?;
    push(SET_DST_WIDTH, copy.bytes)?;
    push(SET_DST_HEIGHT, 1)?;
    push(SET_DST_DEPTH, 1)?;
    push(PITCH_IN, copy.bytes)?;
    push(PITCH_OUT, copy.bytes)?;
    push(SET_SEMAPHORE_A, (copy.fence >> 32) as u32)?;
    push(SET_SEMAPHORE_B, copy.fence as u32)?;
    push(SET_SEMAPHORE_PAYLOAD, FENCE_PAYLOAD)?;
    push(OFFSET_IN_UPPER, (copy.source >> 32) as u32)?;
    push(OFFSET_IN_LOWER, copy.source as u32)?;
    push(OFFSET_OUT_UPPER, (copy.destination >> 32) as u32)?;
    push(OFFSET_OUT_LOWER, copy.destination as u32)?;
    push(
        LAUNCH_DMA,
        crate::raw::HORN_CE_LAUNCH
            | if release {
                crate::raw::HORN_CE_SEMAPHORE
            } else {
                0
            },
    )?;
    if out.len() == start {
        return Err(Error::State);
    }
    Ok(())
}

/// NVC56F GPFIFO: address low31:2 plus high7:0 in the second word;
/// LENGTH is second-word30:10, not second-word9:0. MAIN/PROCEED are zero.
pub fn gpfifo_entry(pushbuffer: u64, dwords: u32) -> Result<u64> {
    let address_limit = (u64::from(crate::raw::HORN_CE_GP_GET_HI_MASK) + 1) << 32;
    if pushbuffer & 3 != 0
        || pushbuffer >= address_limit
        || dwords == 0
        || dwords > crate::raw::HORN_CE_GP_LENGTH_MASK
        || pushbuffer
            .checked_add(u64::from(dwords) * 4)
            .is_none_or(|end| end > address_limit)
    {
        return Err(Error::Range);
    }
    Ok(pushbuffer | (u64::from(dwords) << (32 + crate::raw::HORN_CE_GP_LENGTH_SHIFT)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_headers_match_the_dma_format() {
        // Incrementing write of one argument at 0x0260.
        assert_eq!(
            header(SET_SRC_PHYS_MODE, 1).unwrap(),
            (1 << 29) | (1 << 16) | (0x260 >> 2)
        );
        // Non-incrementing carries opcode 3 in bits 31:29.
        assert_eq!(
            header_noninc(SET_SRC_PHYS_MODE).unwrap(),
            (3 << 29) | (1 << 16) | (0x260 >> 2)
        );
        assert!(header(0x0261, 1).is_err());
        assert!(header(0x0260, 0).is_err());
    }

    #[test]
    fn copy_sequence_is_ordered_and_fenced() {
        let mut words = Vec::new();
        let copy = Copy {
            source: 0x1_2345_6000,
            destination: 0x2_3456_7000,
            bytes: 4096,
            fence: 0x3_4567_8000,
        };
        encode_copy(&copy, &mut words).unwrap();
        assert_eq!(words.len(), 20 * 2);
        assert_eq!(words[0], header(SET_SRC_PHYS_MODE, 1).unwrap());
        assert_eq!(words[1], PHYS_MODE_COHERENT_SYSMEM);
        // Source address is split across the upper/lower pair.
        let upper = words
            .chunks(2)
            .find(|pair| pair[0] == header(OFFSET_IN_UPPER, 1).unwrap())
            .unwrap()[1];
        let lower = words
            .chunks(2)
            .find(|pair| pair[0] == header(OFFSET_IN_LOWER, 1).unwrap())
            .unwrap()[1];
        assert_eq!((u64::from(upper) << 32) | u64::from(lower), copy.source);
        // The launch word is last and requests a one-word semaphore.
        let launch = words[words.len() - 1];
        assert_eq!(
            launch & LAUNCH_SEMAPHORE_ONE_WORD,
            LAUNCH_SEMAPHORE_ONE_WORD
        );
        assert_eq!(launch & LAUNCH_SRC_LAYOUT_PITCH, LAUNCH_SRC_LAYOUT_PITCH);
        assert_eq!(launch & LAUNCH_DST_TYPE_PHYSICAL, LAUNCH_DST_TYPE_PHYSICAL);
    }

    #[test]
    fn only_final_roundtrip_launch_releases_fence() {
        let mut words = Vec::new();
        let copy = Copy {
            source: 0x1000,
            destination: 0x2000,
            bytes: 65536,
            fence: 0x3000,
        };
        encode_transfer(&copy, Target::System, Target::Vram, false, &mut words).unwrap();
        assert_eq!(words[1], PHYS_MODE_COHERENT_SYSMEM);
        assert_eq!(
            words[3],
            crate::raw::NVC7B5_SET_DST_PHYS_MODE_TARGET_LOCAL_FB
        );
        assert_eq!(words[39] & crate::raw::HORN_CE_SEMAPHORE, 0);
        assert_eq!(
            words[39] & 3,
            crate::raw::NVC7B5_LAUNCH_DMA_DATA_TRANSFER_TYPE_NON_PIPELINED
        );
        encode_transfer(&copy, Target::Vram, Target::System, true, &mut words).unwrap();
        assert_eq!(
            words[79] & crate::raw::HORN_CE_SEMAPHORE,
            crate::raw::HORN_CE_SEMAPHORE
        );
    }

    #[test]
    fn gpfifo_entry_carries_address_and_length() {
        let entry = gpfifo_entry(0x2000_8000, 40).unwrap();
        assert_eq!(entry & 0xffff_fffc, 0x2000_8000);
        assert_eq!(entry >> 42, 40);
        assert_eq!((entry >> 32) & 0x3ff, 0);
        assert!(gpfifo_entry(0x2000_8002, 40).is_err());
        assert!(gpfifo_entry(0x2000_8000, 0).is_err());
        let high = gpfifo_entry(0x1_0000_8000, 40).unwrap();
        assert_eq!((high >> 32) & 0xff, 1);
        assert_eq!(high >> 42, 40);
        assert!(gpfifo_entry(1 << 40, 40).is_err());
        assert!(gpfifo_entry((1 << 40) - 4, 2).is_err());
    }
}
