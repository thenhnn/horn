// SPDX-License-Identifier: GPL-2.0-only
//! Copy-engine methods and pushbuffer encoding for `AMPERE_DMA_COPY_B`.
//!
//! Method numbers and field positions come from NVIDIA's
//! `class/clc7b5.h` (570.144). A pushbuffer is a sequence of method headers
//! followed by their arguments; the header carries the opcode, the argument
//! count and the method offset in dwords.

use alloc::vec::Vec;

use crate::{Error, Result};

/// `NVC7B5_DMA_OPCODE_METHOD`: arguments are written to consecutive methods.
pub const OPCODE_METHOD: u32 = 0;
/// `NVC7B5_DMA_OPCODE_NONINC_METHOD`: the method is written once.
pub const OPCODE_NONINC: u32 = 2;

const COUNT_SHIFT: u32 = 18;
const OFFSET_SHIFT: u32 = 2;
const MAX_COUNT: u32 = 0x3ff;

/// A pushbuffer word that writes `count` consecutive methods starting at
/// `method`, or a single non-incrementing write.
pub fn header(method: u32, count: u32) -> Result<u32> {
    if count == 0 || count > MAX_COUNT || method & 3 != 0 || method > 0x3ffc {
        return Err(Error::Range);
    }
    Ok((count << COUNT_SHIFT) | (method >> OFFSET_SHIFT))
}

pub fn header_noninc(method: u32) -> Result<u32> {
    if method & 3 != 0 || method > 0x3ffc {
        return Err(Error::Range);
    }
    Ok((OPCODE_NONINC << 29) | (1 << COUNT_SHIFT) | (method >> OFFSET_SHIFT))
}

/// `AMPERE_DMA_COPY_B` method numbers.
pub const SET_SEMAPHORE_A: u32 = 0x0240;
pub const SET_SEMAPHORE_B: u32 = 0x0244;
pub const SET_SEMAPHORE_PAYLOAD: u32 = 0x0248;
pub const SET_SRC_PHYS_MODE: u32 = 0x0260;
pub const SET_DST_PHYS_MODE: u32 = 0x0264;
pub const LAUNCH_DMA: u32 = 0x0300;
pub const OFFSET_IN_UPPER: u32 = 0x0400;
pub const OFFSET_IN_LOWER: u32 = 0x0404;
pub const OFFSET_OUT_UPPER: u32 = 0x0408;
pub const OFFSET_OUT_LOWER: u32 = 0x040c;
pub const PITCH_IN: u32 = 0x0410;
pub const PITCH_OUT: u32 = 0x0414;
pub const SET_DST_BLOCK_SIZE: u32 = 0x070c;
pub const SET_DST_WIDTH: u32 = 0x0710;
pub const SET_DST_HEIGHT: u32 = 0x0714;
pub const SET_DST_DEPTH: u32 = 0x0718;
pub const SET_SRC_BLOCK_SIZE: u32 = 0x0728;
pub const SET_SRC_WIDTH: u32 = 0x072c;
pub const SET_SRC_HEIGHT: u32 = 0x0730;
pub const SET_SRC_DEPTH: u32 = 0x0734;

/// `NVC7B5_SET_*_PHYS_MODE_TARGET_COHERENT_SYSMEM`.
pub const PHYS_MODE_COHERENT_SYSMEM: u32 = 1;

const LAUNCH_TRANSFER_PIPELINED: u32 = 1;
const LAUNCH_FLUSH_ENABLE: u32 = 1 << 2;
const LAUNCH_SEMAPHORE_ONE_WORD: u32 = 1 << 3;
const LAUNCH_SRC_LAYOUT_PITCH: u32 = 1 << 7;
const LAUNCH_DST_LAYOUT_PITCH: u32 = 1 << 8;
const LAUNCH_SRC_TYPE_PHYSICAL: u32 = 1 << 12;
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
    if copy.bytes == 0 || copy.bytes & 3 != 0 {
        return Err(Error::Range);
    }
    let start = out.len();
    let mut push = |method: u32, value: u32| -> Result<()> {
        out.try_reserve(2).map_err(|_| Error::Exhausted)?;
        out.push(header(method, 1)?);
        out.push(value);
        Ok(())
    };
    push(SET_SRC_PHYS_MODE, PHYS_MODE_COHERENT_SYSMEM)?;
    push(SET_DST_PHYS_MODE, PHYS_MODE_COHERENT_SYSMEM)?;
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
        LAUNCH_TRANSFER_PIPELINED
            | LAUNCH_FLUSH_ENABLE
            | LAUNCH_SEMAPHORE_ONE_WORD
            | LAUNCH_SRC_LAYOUT_PITCH
            | LAUNCH_DST_LAYOUT_PITCH
            | LAUNCH_SRC_TYPE_PHYSICAL
            | LAUNCH_DST_TYPE_PHYSICAL,
    )?;
    if out.len() == start {
        return Err(Error::State);
    }
    Ok(())
}

/// One GPFIFO entry: the pushbuffer address in bits 31:2 (so the pushbuffer
/// must live below 4 GiB in the channel's vspace) and the length in dwords in
/// bits 41:32. The low two bits carry flags and stay clear for a normal entry.
pub fn gpfifo_entry(pushbuffer: u64, dwords: u32) -> Result<u64> {
    if pushbuffer & 3 != 0 || pushbuffer >= 1 << 32 || dwords == 0 || dwords > 0x3ff {
        return Err(Error::Range);
    }
    Ok(pushbuffer | ((dwords as u64) << 32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_headers_match_the_dma_format() {
        // Incrementing write of one argument at 0x0260.
        assert_eq!(header(SET_SRC_PHYS_MODE, 1).unwrap(), (1 << 18) | (0x260 >> 2));
        // Non-incrementing carries opcode 2 in bits 31:29.
        assert_eq!(
            header_noninc(SET_SRC_PHYS_MODE).unwrap(),
            (2 << 29) | (1 << 18) | (0x260 >> 2)
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
        assert_eq!(launch & LAUNCH_SEMAPHORE_ONE_WORD, LAUNCH_SEMAPHORE_ONE_WORD);
        assert_eq!(
            launch & LAUNCH_SRC_LAYOUT_PITCH,
            LAUNCH_SRC_LAYOUT_PITCH
        );
        assert_eq!(launch & LAUNCH_DST_TYPE_PHYSICAL, LAUNCH_DST_TYPE_PHYSICAL);
    }

    #[test]
    fn gpfifo_entry_carries_address_and_length() {
        let entry = gpfifo_entry(0x2000_8000, 40).unwrap();
        assert_eq!(entry & 0xffff_fffc, 0x2000_8000);
        assert_eq!(entry >> 32, 40);
        assert!(gpfifo_entry(0x2000_8002, 40).is_err());
        assert!(gpfifo_entry(0x2000_8000, 0).is_err());
        // A pushbuffer above 4 GiB cannot be addressed by an entry.
        assert!(gpfifo_entry(0x1_0000_8000, 40).is_err());
    }
}
