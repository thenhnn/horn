// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// Adapted from NOVA 3497a403264b094ebd61b8b755165d56e7d9dc37,
// drivers/gpu/nova-core/firmware/tlv.rs. See ../PORTING.md.
use crate::{Error, Result};

/// Borrowed, fully walked NVFW container. At most 32 unique ASCII tags.
/// Unlike NOVA's first-match lookup, duplicates are rejected (including unknown
/// tags). Unknown *unique* tags are preserved for forwards-compatible packaging.
pub struct Tlv<'a> {
    blocks: [Option<([u8; 4], &'a [u8])>; 32],
}
impl<'a> Tlv<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let mut tail = data.strip_prefix(b"NVFW").ok_or(Error::Malformed)?;
        let mut blocks = [None; 32];
        let mut count = 0;
        while !tail.is_empty() {
            let header = tail.get(..8).ok_or(Error::Malformed)?;
            let tag: [u8; 4] = header[..4].try_into().unwrap();
            if !tag.is_ascii()
                || count == blocks.len()
                || blocks[..count].iter().flatten().any(|(t, _)| *t == tag)
            {
                return Err(Error::Malformed);
            }
            let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            let stored = length.checked_add(3).ok_or(Error::Range)? & !3;
            let end = 8usize.checked_add(stored).ok_or(Error::Range)?;
            let block = tail.get(..end).ok_or(Error::Malformed)?;
            blocks[count] = Some((tag, &block[8..8 + length]));
            count += 1;
            tail = &tail[end..];
        }
        let out = Self { blocks };
        if out.bytes(b"VERS")? != b"570.144" {
            return Err(Error::Version);
        }
        Ok(out)
    }
    pub fn bytes(&self, tag: &[u8; 4]) -> Result<&'a [u8]> {
        self.blocks
            .iter()
            .flatten()
            .find(|(t, _)| t == tag)
            .map(|(_, b)| *b)
            .filter(|b| !b.is_empty())
            .ok_or(Error::Malformed)
    }
    pub fn u32(&self, tag: &[u8; 4]) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.bytes(tag)?.try_into().map_err(|_| Error::Malformed)?,
        ))
    }
    /// Official production-signature selection: fuse=0 chooses the last
    /// signature, otherwise index=firmware fuse version - hardware fuse version.
    pub fn signature(&self, hardware_fuse: u32) -> Result<&'a [u8]> {
        let n = self.u32(b"NSIG")? as usize;
        let bytes = self.bytes(b"SIGN")?;
        if n == 0 || bytes.len() % n != 0 {
            return Err(Error::Malformed);
        }
        let index = if hardware_fuse == 0 {
            n - 1
        } else {
            self.u32(b"FUSE")?
                .checked_sub(hardware_fuse)
                .ok_or(Error::Unsupported)? as usize
        };
        if index >= n {
            return Err(Error::Unsupported);
        }
        let size = bytes.len() / n;
        Ok(&bytes[index * size..(index + 1) * size])
    }
}
