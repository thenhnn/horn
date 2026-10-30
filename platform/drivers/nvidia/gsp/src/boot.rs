// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//! NOVA 3497a403: fb.rs, gsp.rs, gsp/fw.rs, firmware/radix3.rs.
//! Checked AD106/r570_144 builders, NOT DMA grants or firmware authentication.
use crate::{
    Error, Result, abi,
    firmware::{Bundle, GSP_BYTES},
};
use alloc::vec::Vec;
use core::{
    mem::{offset_of, size_of},
    ops::Range,
};
pub const PAGE: usize = 4096;
pub const RING_PAGES: usize = 63;
pub const QUEUE_PAGES: usize = 1 + 2 * (1 + RING_PAGES);
pub const QUEUE_BYTES: usize = QUEUE_PAGES * PAGE;
pub const CPU_HEADER: usize = PAGE;
pub const CPU_DATA: usize = CPU_HEADER + PAGE;
pub const GSP_HEADER: usize = CPU_HEADER + (1 + RING_PAGES) * PAGE;
pub const GSP_DATA: usize = GSP_HEADER + PAGE;
pub const LOG_BYTES: usize = 16 * PAGE;
const M: u64 = 1 << 20;

/// Validated numerical extent, NOT authority. Every encoded span must refer to
/// a committed retained mapping for this exact SID, with the right direction.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    base: u64,
    bytes: usize,
}
impl Span {
    pub fn new(base: u64, bytes: usize) -> Result<Self> {
        if base == 0
            || base & 4095 != 0
            || bytes == 0
            || bytes & 4095 != 0
            || base
                .checked_add(bytes as u64)
                .is_none_or(|end| end > 1u64 << 47)
        {
            return Err(Error::Range);
        }
        Ok(Self { base, bytes })
    }
    pub fn address(self) -> u64 {
        self.base
    }
    pub fn bytes(self) -> usize {
        self.bytes
    }
    fn disjoint(self, other: Self) -> Result<()> {
        if self.base < other.base + other.bytes as u64 && other.base < self.base + self.bytes as u64
        {
            Err(Error::Range)
        } else {
            Ok(())
        }
    }
    fn exact(self, bytes: usize) -> Result<()> {
        if self.bytes == bytes {
            Ok(())
        } else {
            Err(Error::Range)
        }
    }
}
fn put32(out: &mut [u8], offset: usize, value: u32) -> Result<()> {
    out.get_mut(offset..offset.checked_add(4).ok_or(Error::Range)?)
        .ok_or(Error::Range)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn put64(out: &mut [u8], offset: usize, value: u64) -> Result<()> {
    out.get_mut(offset..offset.checked_add(8).ok_or(Error::Range)?)
        .ok_or(Error::Range)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn below(end: u64, bytes: u64, alignment: u64) -> Result<Range<u64>> {
    let start = end.checked_sub(bytes).ok_or(Error::Range)? & !(alignment - 1);
    Ok(start..start.checked_add(bytes).ok_or(Error::Range)?)
}
#[derive(Debug)]
pub struct Framebuffer {
    bytes: u64,
    vga: Range<u64>,
    frts: Range<u64>,
    boot: Range<u64>,
    image: Range<u64>,
    heap: Range<u64>,
    wpr: Range<u64>,
    non_wpr: Range<u64>,
}
impl Framebuffer {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn frts(&self) -> Range<u64> {
        self.frts.clone()
    }
    pub fn wpr(&self) -> Range<u64> {
        self.wpr.clone()
    }
    /// Samples come from GA102 usable-FB scratch 0x1183a4 (MiB), display fuse
    /// 0x820c04 and VGA workspace 0x625f04, AFTER privilege-gated GFW completion.
    /// BAR1 aperture length is not framebuffer capacity.
    pub fn from_registers(usable_mb: u32, display_fuse: u32, vga: u32) -> Result<Self> {
        if [usable_mb, display_fuse, vga]
            .into_iter()
            .any(|v| v == u32::MAX || v & 0xffffff00 == 0xbadf4100)
        {
            return Err(Error::Unavailable);
        }
        if !(1024..=32768).contains(&usable_mb) {
            return Err(Error::Range);
        }
        let bytes = u64::from(usable_mb) * M;
        let default_vga = bytes - M;
        let vga_start = if display_fuse & 1 == 0 && vga & 8 != 0 {
            let address = u64::from(vga >> 8) << 16;
            if address < default_vga {
                bytes - (128 << 10)
            } else {
                address
            }
        } else {
            default_vga
        };
        if vga_start >= bytes {
            return Err(Error::Range);
        }
        let vga = vga_start..bytes;
        let frts = below(vga.start & !((128 << 10) - 1), M, 1)?;
        let boot = below(frts.start, 36864, PAGE as u64)?;
        let image = below(boot.start, GSP_BYTES as u64, 64 << 10)?;
        let overhead = (bytes.div_ceil(1 << 30) * u64::from(abi::GSP_FW_HEAP_PARAM_SIZE_PER_GB_FB))
            .next_multiple_of(M);
        let heap_bytes = (u64::from(abi::GSP_FW_HEAP_PARAM_OS_SIZE_LIBOS3_BAREMETAL)
            + u64::from(abi::GSP_FW_HEAP_PARAM_BASE_RM_SIZE_TU10X)
            + u64::from(abi::GSP_FW_HEAP_PARAM_CLIENT_ALLOC_SIZE)
            + overhead)
            .clamp(
                u64::from(abi::GSP_FW_HEAP_SIZE_OVERRIDE_LIBOS3_BAREMETAL_MIN_MB) * M,
                u64::from(abi::GSP_FW_HEAP_SIZE_OVERRIDE_LIBOS3_BAREMETAL_MAX_MB) * M - 1,
            );
        let heap_start = image.start.checked_sub(heap_bytes).ok_or(Error::Range)? & !(M - 1);
        let heap = heap_start..image.start & !(M - 1);
        let wpr_start = heap
            .start
            .checked_sub(size_of::<abi::GspFwWprMeta>() as u64)
            .ok_or(Error::Range)?
            & !(M - 1);
        let wpr = wpr_start..frts.end;
        let non_wpr = below(wpr.start, M, 1)?;
        Ok(Self {
            bytes,
            vga,
            frts,
            boot,
            image,
            heap,
            wpr,
            non_wpr,
        })
    }
    pub fn wpr_metadata(
        &self,
        bundle: &Bundle<'_>,
        radix: Span,
        bootloader: Span,
        signatures: Span,
        out: &mut [u8; PAGE],
    ) -> Result<()> {
        radix.exact(radix_bytes(GSP_BYTES)?)?;
        bootloader.exact(bundle.bootloader.blob.len())?;
        signatures.exact(bundle.signatures.len())?;
        radix.disjoint(bootloader)?;
        radix.disjoint(signatures)?;
        bootloader.disjoint(signatures)?;
        out.fill(0);
        macro_rules! field {
            ($name:ident,$value:expr) => {
                put64(out, offset_of!(abi::GspFwWprMeta, $name), $value)?
            };
        }
        field!(magic, abi::GSP_FW_WPR_META_MAGIC as u64);
        field!(revision, u64::from(abi::GSP_FW_WPR_META_REVISION));
        field!(sysmemAddrOfRadix3Elf, radix.base);
        field!(sizeOfRadix3Elf, bundle.image.len() as u64);
        field!(sysmemAddrOfBootloader, bootloader.base);
        field!(sizeOfBootloader, bootloader.bytes as u64);
        field!(bootloaderCodeOffset, u64::from(bundle.bootloader.code));
        field!(bootloaderDataOffset, u64::from(bundle.bootloader.data));
        field!(
            bootloaderManifestOffset,
            u64::from(bundle.bootloader.manifest)
        );
        let signature = offset_of!(abi::GspFwWprMeta, __bindgen_anon_1);
        put64(out, signature, signatures.base)?;
        put64(out, signature + 8, signatures.bytes as u64)?;
        field!(gspFwRsvdStart, self.non_wpr.start);
        field!(nonWprHeapOffset, self.non_wpr.start);
        field!(nonWprHeapSize, self.non_wpr.end - self.non_wpr.start);
        field!(gspFwWprStart, self.wpr.start);
        field!(gspFwHeapOffset, self.heap.start);
        field!(gspFwHeapSize, self.heap.end - self.heap.start);
        field!(gspFwOffset, self.image.start);
        field!(bootBinOffset, self.boot.start);
        field!(frtsOffset, self.frts.start);
        field!(frtsSize, self.frts.end - self.frts.start);
        field!(gspFwWprEnd, self.vga.start & !((128 << 10) - 1));
        field!(fbSize, self.bytes);
        field!(vgaWorkspaceOffset, self.vga.start);
        field!(vgaWorkspaceSize, self.vga.end - self.vga.start);
        Ok(()) // verified/bootCount/VF/PMU/resume state remain zero
    }
}
/// One L0, one L1, and bounded L2 pages for a contiguous granted IOVA image.
pub fn radix_bytes(bytes: usize) -> Result<usize> {
    if bytes == 0 || bytes > 64 * 1024 * 1024 || bytes & 4095 != 0 {
        return Err(Error::Range);
    }
    Ok((2 + (bytes / PAGE).div_ceil(512)) * PAGE)
}
pub fn radix3(image: Span, tables: Span, out: &mut [u8]) -> Result<()> {
    let bytes = radix_bytes(image.bytes)?;
    tables.exact(bytes)?;
    image.disjoint(tables)?;
    if out.len() != bytes {
        return Err(Error::Range);
    }
    out.fill(0);
    put64(out, 0, tables.base + PAGE as u64)?;
    for n in 0..(image.bytes / PAGE).div_ceil(512) {
        put64(out, PAGE + n * 8, tables.base + ((2 + n) * PAGE) as u64)?;
    }
    for n in 0..image.bytes / PAGE {
        put64(out, 2 * PAGE + n * 8, image.base + (n * PAGE) as u64)?;
    }
    Ok(())
}
pub fn queues(span: Span, out: &mut [u8]) -> Result<()> {
    span.exact(QUEUE_BYTES)?;
    if out.len() != QUEUE_BYTES {
        return Err(Error::Range);
    }
    out.fill(0);
    // Unlike upstream's 512-entry array initialization, leave entries beyond
    // the advertised 129 committed pages zero, not pointing at ungranted RAM.
    for n in 0..QUEUE_PAGES {
        put64(out, n * 8, span.base + (n * PAGE) as u64)?;
    }
    for (n, value) in [
        0,
        ((1 + RING_PAGES) * PAGE) as u32,
        PAGE as u32,
        RING_PAGES as u32,
        0,
        1,
        size_of::<abi::msgqTxHeader>() as u32,
        PAGE as u32,
    ]
    .into_iter()
    .enumerate()
    {
        put32(out, CPU_HEADER + n * 4, value)?;
    }
    Ok(())
}
pub fn rmargs(queue: Span, out: &mut [u8; PAGE]) -> Result<()> {
    queue.exact(QUEUE_BYTES)?;
    out.fill(0);
    put64(
        out,
        offset_of!(abi::MESSAGE_QUEUE_INIT_ARGUMENTS, sharedMemPhysAddr),
        queue.base,
    )?;
    put32(
        out,
        offset_of!(abi::MESSAGE_QUEUE_INIT_ARGUMENTS, pageTableEntryCount),
        QUEUE_PAGES as u32,
    )?;
    put64(
        out,
        offset_of!(abi::MESSAGE_QUEUE_INIT_ARGUMENTS, cmdQueueOffset),
        (CPU_DATA - PAGE) as u64,
    )?;
    put64(
        out,
        offset_of!(abi::MESSAGE_QUEUE_INIT_ARGUMENTS, statQueueOffset),
        (GSP_DATA - PAGE) as u64,
    )?;
    out[offset_of!(abi::GSP_ARGUMENTS_CACHED, bDmemStack)] = 1;
    Ok(())
}
pub fn log_buffer(span: Span, out: &mut [u8]) -> Result<()> {
    span.exact(LOG_BYTES)?;
    if out.len() != LOG_BYTES {
        return Err(Error::Range);
    }
    out.fill(0);
    for n in 0..16 {
        put64(out, 8 + n * 8, span.base + (n * PAGE) as u64)?;
    }
    Ok(())
}
pub fn libos(logs: [Span; 3], rmargs: Span, out: &mut [u8; PAGE]) -> Result<()> {
    rmargs.exact(PAGE)?;
    for log in logs {
        log.exact(LOG_BYTES)?;
        log.disjoint(rmargs)?;
    }
    for i in 0..3 {
        for j in i + 1..3 {
            logs[i].disjoint(logs[j])?;
        }
    }
    out.fill(0);
    for (n, (name, span)) in [
        ("LOGINIT", logs[0]),
        ("LOGINTR", logs[1]),
        ("LOGRM", logs[2]),
        ("RMARGS", rmargs),
    ]
    .into_iter()
    .enumerate()
    {
        let base = n * size_of::<abi::LibosMemoryRegionInitArgument>();
        let id = name.bytes().fold(0u64, |id, b| (id << 8) | u64::from(b));
        put64(out, base, id)?;
        put64(out, base + 8, span.base)?;
        put64(out, base + 16, span.bytes as u64)?;
        out[base + 24] = 1;
        out[base + 25] = 1; // contiguous, sysmem; terminator stays zero
    }
    Ok(())
}
/// Inputs are the assigned function's trusted PCI snapshot, not caller-selected
/// RAM/IOVA authority. Addresses describe guest PCI BARs only.
pub fn system_info(bars: [u64; 3], bdf: u64, ids: [u32; 3]) -> Result<Vec<u8>> {
    if ids[0] != 0x280310de || ids[1] != 0xf3061569 || bars.iter().any(|a| *a == 0 || a & 4095 != 0)
    {
        return Err(Error::Range);
    }
    let mut out = alloc::vec![0;size_of::<abi::GspSystemInfo>()];
    macro_rules! q {
        ($name:ident,$value:expr) => {
            put64(&mut out, offset_of!(abi::GspSystemInfo, $name), $value)?
        };
    }
    macro_rules! d {
        ($name:ident,$value:expr) => {
            put32(&mut out, offset_of!(abi::GspSystemInfo, $name), $value)?
        };
    }
    q!(gpuPhysAddr, bars[0]);
    q!(gpuPhysFbAddr, bars[1]);
    q!(gpuPhysInstAddr, bars[2]);
    q!(nvDomainBusDeviceFunc, bdf);
    q!(maxUserVa, (1u64 << 47) - 4096);
    d!(pciConfigMirrorBase, 0x88000);
    d!(pciConfigMirrorSize, 0x1000);
    d!(PCIDeviceID, ids[0]);
    d!(PCISubDeviceID, ids[1]);
    d!(PCIRevisionID, ids[2]);
    Ok(out)
}
pub fn registry() -> Result<Vec<u8>> {
    // No firmware-initiated secondary-bus reset or product-ID-check bypass.
    let entries = [
        ("RMSecBusResetEnable", 0),
        ("RMForcePcieConfigSave", 1),
        ("RMDevidCheckIgnore", 0),
    ];
    let strings = 8 + entries.len() * 16;
    let bytes = strings
        + entries
            .iter()
            .map(|(name, _)| name.len() + 1)
            .sum::<usize>();
    let mut out = alloc::vec![0;bytes];
    put32(&mut out, 0, bytes as u32)?;
    put32(&mut out, 4, entries.len() as u32)?;
    let mut at = strings;
    for (n, (name, value)) in entries.into_iter().enumerate() {
        let base = 8 + n * 16;
        put32(&mut out, base, at as u32)?;
        out[base + 4] = abi::REGISTRY_TABLE_ENTRY_TYPE_DWORD as u8;
        put32(&mut out, base + 8, value)?;
        out[at..at + name.len()].copy_from_slice(name.as_bytes());
        at += name.len() + 1;
    }
    Ok(out)
}
const _: () = assert!(size_of::<abi::LibosMemoryRegionInitArgument>() == 32);
const _: () = assert!(size_of::<abi::msgqTxHeader>() == 32);
const _: () = assert!(size_of::<abi::GspFwWprMeta>() == 256);
