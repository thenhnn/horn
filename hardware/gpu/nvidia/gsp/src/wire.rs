// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//! NOVA 3497a403 gsp/{fw,cmdq,commands}.rs, r570_144 bare-metal framing.
//! Decode only private snapshots, never Rust references into GPU-writable RAM.
//! Checksums detect corruption, not authenticity or actual hardware readiness.
use crate::{Error, Result, abi, boot::PAGE};
use core::{
    mem::{offset_of, size_of},
    ops::Range,
};
pub const HEADER: usize = size_of::<abi::GSP_MSG_QUEUE_ELEMENT>();
pub const MAX_FRAME: usize = 65536;
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER;
pub const INIT_DONE: u32 = abi::NV_VGPU_MSG_EVENT_GSP_INIT_DONE;
pub const CPU_SEQUENCE: u32 = abi::NV_VGPU_MSG_EVENT_GSP_RUN_CPU_SEQUENCER;
pub const SET_SYSTEM_INFO: u32 = abi::NV_VGPU_MSG_FUNCTION_GSP_SET_SYSTEM_INFO;
pub const SET_REGISTRY: u32 = abi::NV_VGPU_MSG_FUNCTION_SET_REGISTRY;
pub const UNLOADING: u32 = abi::NV_VGPU_MSG_FUNCTION_UNLOADING_GUEST_DRIVER;
/// Events that must stop the driver instead of being logged and dropped.
/// GSP_POST_NOCAT_RECORD is a POST-code notification that upstream explicitly
/// drops, so treating it as fatal aborted a healthy boot.
pub fn fatal_event(function: u32) -> bool {
    matches!(
        function,
        abi::NV_VGPU_MSG_EVENT_OS_ERROR_LOG
            | abi::NV_VGPU_MSG_EVENT_RC_TRIGGERED
            | abi::NV_VGPU_MSG_EVENT_MMU_FAULT_QUEUED
    )
}
pub fn post_event(function: u32) -> bool {
    function == abi::NV_VGPU_MSG_EVENT_GSP_POST_NOCAT_RECORD
}
pub const STATIC_INFO: u32 = abi::NV_VGPU_MSG_FUNCTION_GET_GSP_STATIC_INFO;
pub const CONTINUATION: u32 = abi::NV_VGPU_MSG_FUNCTION_CONTINUATION_RECORD;
pub const STATIC_INFO_BYTES: usize = size_of::<abi::GspStaticConfigInfo_t>();
const RPC: usize = offset_of!(abi::GSP_MSG_QUEUE_ELEMENT, rpc);
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset.checked_add(4).ok_or(Error::Range)?)
            .ok_or(Error::Malformed)?
            .try_into()
            .map_err(|_| Error::Malformed)?,
    ))
}
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset.checked_add(8).ok_or(Error::Range)?)
            .ok_or(Error::Malformed)?
            .try_into()
            .map_err(|_| Error::Malformed)?,
    ))
}
fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
pub fn checksum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .enumerate()
        .fold(0, |sum, (i, b)| sum ^ (u32::from(*b) << ((i & 3) * 8)))
}
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub bytes: usize,
    pub pages: usize,
    /// Transport seqNum, NOT the inner RPC sequence.
    pub transport_sequence: u32,
    pub rpc_sequence: u32,
    pub function: u32,
    pub status: u32,
    pub private_status: u32,
}
impl Header {
    /// Validate the fixed header BEFORE sizing/copying a published ring span.
    /// Caller separately bounds published pages and validates a private snapshot.
    pub fn parse(snapshot: &[u8]) -> Result<Self> {
        if snapshot.len() < HEADER {
            return Err(Error::Malformed);
        }
        if snapshot[..32].iter().any(|b| *b != 0) || snapshot[44..48].iter().any(|b| *b != 0) {
            return Err(Error::Unsupported);
        }
        if u32_at(snapshot, RPC)? != 3 << 24
            || u32_at(snapshot, RPC + 4)? != abi::NV_VGPU_MSG_SIGNATURE_VALID
        {
            return Err(Error::Version);
        }
        let rpc_bytes = u32_at(snapshot, RPC + 8)? as usize;
        if rpc_bytes < size_of::<abi::rpc_message_header_v>() {
            return Err(Error::Malformed);
        }
        let bytes = RPC.checked_add(rpc_bytes).ok_or(Error::Range)?;
        if !(HEADER..=MAX_FRAME).contains(&bytes) {
            return Err(Error::Range);
        }
        let pages = u32_at(snapshot, 40)? as usize;
        if pages != bytes.div_ceil(PAGE) {
            return Err(Error::Malformed);
        }
        Ok(Self {
            bytes,
            pages,
            transport_sequence: u32_at(snapshot, 36)?,
            rpc_sequence: u32_at(snapshot, RPC + 24)?,
            function: u32_at(snapshot, RPC + 12)?,
            status: u32_at(snapshot, RPC + 16)?,
            private_status: u32_at(snapshot, RPC + 20)?,
        })
    }
}
pub struct Packet<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}
impl<'a> Packet<'a> {
    pub fn decode(snapshot: &'a [u8]) -> Result<Self> {
        let header = Header::parse(snapshot)?;
        if snapshot.len() != header.bytes {
            return Err(Error::Malformed);
        }
        if checksum(snapshot) != 0 {
            return Err(Error::Checksum);
        }
        Ok(Self {
            header,
            payload: &snapshot[HEADER..],
        })
    }
    /// Structural event validation only. The runtime must receive this from
    /// its actual assigned GSP queue in the active firmware epoch.
    pub fn init_done(&self) -> Result<()> {
        if self.header.function != INIT_DONE || !self.payload.is_empty() {
            return Err(Error::Malformed);
        }
        Ok(())
    }
    pub fn success(&self) -> Result<()> {
        if self.header.status != 0 {
            return Err(Error::FirmwareStatus(crate::FirmwareStatus(self.header.status)));
        }
        Ok(())
    }
}
/// Construct one element. Inner sequence is explicit, never copied from the
/// outer transport counter. Sources use zero for ordinary host boot commands.
pub fn encode(
    out: &mut [u8],
    function: u32,
    transport_sequence: u32,
    rpc_sequence: u32,
    parts: [&[u8]; 2],
) -> Result<usize> {
    let bytes = HEADER
        .checked_add(parts[0].len())
        .and_then(|n| n.checked_add(parts[1].len()))
        .ok_or(Error::Range)?;
    if bytes > MAX_FRAME || out.len() < bytes {
        return Err(Error::Range);
    }
    let out = &mut out[..bytes];
    out.fill(0);
    put(out, 36, transport_sequence);
    put(out, 40, bytes.div_ceil(PAGE) as u32);
    put(out, RPC, 3 << 24);
    put(out, RPC + 4, abi::NV_VGPU_MSG_SIGNATURE_VALID);
    put(out, RPC + 8, (bytes - RPC) as u32);
    put(out, RPC + 12, function);
    put(out, RPC + 16, u32::MAX);
    put(out, RPC + 20, u32::MAX);
    put(out, RPC + 24, rpc_sequence);
    let split = HEADER + parts[0].len();
    out[HEADER..split].copy_from_slice(parts[0]);
    out[split..].copy_from_slice(parts[1]);
    let sum = checksum(out);
    put(out, 32, sum);
    Ok(bytes)
}
/// Bounded outgoing continuations. Fixed command fields never spill into a
/// continuation; only its variable payload may split, as in NOVA SplitState.
pub struct Chunks<'a> {
    function: u32,
    fixed: &'a [u8],
    variable: &'a [u8],
    first: bool,
}
pub struct Chunk<'a> {
    pub function: u32,
    pub parts: [&'a [u8]; 2],
}
impl<'a> Chunks<'a> {
    pub fn new(function: u32, fixed: &'a [u8], variable: &'a [u8]) -> Result<Self> {
        if fixed.len() > MAX_PAYLOAD
            || fixed
                .len()
                .checked_add(variable.len())
                .is_none_or(|n| n > 1024 * 1024)
        {
            return Err(Error::Range);
        }
        Ok(Self {
            function,
            fixed,
            variable,
            first: true,
        })
    }
}
impl<'a> Iterator for Chunks<'a> {
    type Item = Chunk<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if !self.first && self.variable.is_empty() {
            return None;
        }
        let (function, fixed) = if self.first {
            (self.function, self.fixed)
        } else {
            (CONTINUATION, &[][..])
        };
        self.first = false;
        let n = self.variable.len().min(MAX_PAYLOAD - fixed.len());
        let (part, rest) = self.variable.split_at(n);
        self.variable = rest;
        Some(Chunk {
            function,
            parts: [fixed, part],
        })
    }
}
/// Validated diagnostics, not a GPU memory allocator or backing authority.
pub struct StaticInfo<'a> {
    pub name: &'a str,
    pub framebuffer_bytes: u64,
    pub bar1_pde: u64,
    pub regions: usize,
}
impl<'a> StaticInfo<'a> {
    pub fn parse(packet: &Packet<'a>, observed_fb: u64) -> Result<Self> {
        if packet.header.function != STATIC_INFO || packet.payload.len() != STATIC_INFO_BYTES {
            return Err(Error::Malformed);
        }
        packet.success()?;
        let bytes = packet.payload;
        let raw = &bytes[offset_of!(abi::GspStaticConfigInfo_t, gpuNameString)..][..64];
        let n = raw.iter().position(|b| *b == 0).ok_or(Error::Malformed)?;
        if n == 0 || !raw[..n].iter().all(|b| (0x20..=0x7e).contains(b)) {
            return Err(Error::Malformed);
        }
        let name = core::str::from_utf8(&raw[..n]).map_err(|_| Error::Malformed)?;
        let framebuffer_bytes = u64_at(bytes, offset_of!(abi::GspStaticConfigInfo_t, fb_length))?;
        if framebuffer_bytes == 0 || framebuffer_bytes > observed_fb {
            return Err(Error::Range);
        }
        let bar1_pde = u64_at(bytes, offset_of!(abi::GspStaticConfigInfo_t, bar1PdeBase))?;
        if bar1_pde >= observed_fb || bar1_pde & 4095 != 0 {
            return Err(Error::Range);
        }
        let base = offset_of!(abi::GspStaticConfigInfo_t, fbRegionInfoParams);
        let regions = u32_at(bytes, base)? as usize;
        if !(1..=16).contains(&regions) {
            return Err(Error::Range);
        }
        let mut ranges: [Range<u64>; 16] = core::array::from_fn(|_| 0..0);
        for i in 0..regions {
            let base = base
                + offset_of!(abi::NV2080_CTRL_CMD_FB_GET_FB_REGION_INFO_PARAMS, fbRegion)
                + i * size_of::<abi::NV2080_CTRL_CMD_FB_GET_FB_REGION_FB_REGION_INFO>();
            let start = u64_at(bytes, base)?;
            let end = u64_at(bytes, base + 8)?
                .checked_add(1)
                .ok_or(Error::Range)?;
            if start >= end || end > observed_fb || u64_at(bytes, base + 16)? > end - start {
                return Err(Error::Range);
            }
            if bytes[base + 28..base + 31].iter().any(|v| *v > 1) {
                return Err(Error::Malformed);
            }
            if ranges[..i].iter().any(|r| start < r.end && r.start < end) {
                return Err(Error::Range);
            }
            ranges[i] = start..end;
        }
        Ok(Self {
            name,
            framebuffer_bytes,
            bar1_pde,
            regions,
        })
    }
}
const _: () = assert!(HEADER == 80 && RPC == 48 && STATIC_INFO_BYTES < MAX_PAYLOAD);
