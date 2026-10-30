// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
//! AD106-only port of NOVA 3497a403: falcon.rs, falcon/hal/ga102.rs,
//! fb/hal/ga100.rs and their register definitions. No PIO/unsigned fallback.
//! No Drop unmaps memory, resets hardware, or establishes a new hardware epoch.
use crate::{Error, Result, firmware::Segment};
use core::ops::Range;
mod sequencer;

#[cfg(test)]
mod tests;

/// Horn adapter: scoped UC BAR0 accesses and granted monotonic clock/alarm.
/// No register access is implemented as a guessed physical-memory operation.
pub trait Platform {
    fn read32(&mut self, offset: u32) -> Result<u32>;
    fn write32(&mut self, offset: u32, value: u32) -> Result<()>;
    fn now_us(&mut self) -> Result<u64>;
    /// Must not return successfully before `deadline`. Use an alarm, not a
    /// fabricated delay counter. May sleep longer; the next check detects it.
    fn sleep_until(&mut self, deadline: u64) -> Result<()>;
    /// Device-visible WB RAM writes must precede UC DMA/start register writes.
    fn dma_write_barrier(&mut self);
    /// Poison the backend too; delegated queue/DMA I/O must not bypass a fault.
    fn quarantine(&mut self) {}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Gsp,
    Sec2,
}
impl Engine {
    fn index(self) -> usize {
        match self {
            Self::Gsp => 0,
            Self::Sec2 => 1,
        }
    }
    fn base(self) -> u32 {
        match self {
            Self::Gsp => 0x110000,
            Self::Sec2 => 0x840000,
        }
    }
    fn second(self) -> u32 {
        self.base() + 0x1000
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Probe,
    Gfw,
    FlushPage,
    ResetReady,
    ResetPulse,
    Scrub,
    CoreSelect,
    Fuse,
    Dma,
    Start,
    Halt,
    Wpr,
    FwsecStatus,
    Riscv,
    Framebuffer,
    QueueNotify,
    BooterStatus,
    Shutdown,
    Backend,
    Rpc,
    Sequencer,
}
#[derive(Clone, Copy, Debug)]
pub struct Diagnostic {
    pub stage: Stage,
    pub register: u32,
    pub value: Option<u32>,
    pub clock_us: u64,
    pub error: Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Unknown,
    Reset,
    Loaded,
    Halted,
}
#[derive(Clone, Copy)]
pub(crate) struct Brom {
    pub pkc: u32,
    pub engines: u16,
    pub ucode: u8,
}
/// CPU-exclusive, production-signature-patched bytes and checked load geometry.
/// It is not a DMA grant and does not assert that hardware authenticated it.
pub struct Prepared<'a> {
    bytes: &'a [u8],
    target: Engine,
    imem: Segment,
    dmem: Segment,
    brom: Brom,
    boot: u32,
    command: Option<crate::vbios::Command>,
}
impl<'a> Prepared<'a> {
    pub fn bytes(&self) -> &[u8] {
        self.bytes
    }
    pub(crate) fn new(
        bytes: &'a [u8],
        target: Engine,
        imem: Segment,
        dmem: Segment,
        brom: Brom,
        boot: u32,
        command: Option<crate::vbios::Command>,
    ) -> Result<Self> {
        for segment in [imem, dmem] {
            if segment.length == 0
                || (segment.source | segment.destination | segment.length) & 255 != 0
                || segment
                    .source
                    .checked_add(segment.length)
                    .is_none_or(|end| end as usize > bytes.len())
                || segment
                    .destination
                    .checked_add(segment.length)
                    .is_none_or(|end| end > 0x100_0000)
            {
                return Err(Error::Range);
            }
        }
        if brom.engines == 0
            || !(1..=16).contains(&brom.ucode)
            || brom
                .pkc
                .checked_add(384)
                .is_none_or(|end| end > dmem.length)
        {
            return Err(Error::Range);
        }
        Ok(Self {
            bytes,
            target,
            imem,
            dmem,
            brom,
            boot,
            command,
        })
    }
}
/// The adapter retains an actual VM pin/IOMMU mapping, separately from this
/// borrowed view. Dropping the view releases nothing.
pub struct PinnedSource<'a> {
    image: &'a Prepared<'a>,
    iova: u64,
}
impl<'a> PinnedSource<'a> {
    /// # Safety
    /// This complete, contiguous IOVA grant must map `image.bytes()` for device
    /// reads on this controller's assigned SID, with those exact staged bytes.
    /// Keep the backing/pin on any uncertain DMA failure until devmgr confirms
    /// invalidate/drain or device teardown. CPU contents stay immutable while
    /// hardware can read them. A numeric IOVA alone is never sufficient.
    pub unsafe fn from_grant(image: &'a Prepared<'a>, iova: u64) -> Result<Self> {
        if iova == 0
            || iova & 255 != 0
            || iova
                .checked_add(image.bytes.len() as u64)
                .is_none_or(|end| end > 1u64 << 47)
        {
            return Err(Error::Range);
        }
        Ok(Self { image, iova })
    }
}
/// Retained inputs for the GA102/Ada path, including mandatory staged cleanup.
/// Nested metadata correctness and DMA ownership are the caller's obligations.
pub struct Handoff<'s, 'image> {
    pub framebuffer: &'s crate::boot::Framebuffer,
    pub flush: crate::boot::Span,
    pub libos: crate::boot::Span,
    pub wpr_meta: crate::boot::Span,
    pub frts: &'s PinnedSource<'image>,
    pub sb: &'s PinnedSource<'image>,
    pub loader: &'s PinnedSource<'image>,
    pub unloader: &'s PinnedSource<'image>,
    pub app_version: u32,
}
impl Handoff<'_, '_> {
    fn validate(&self) -> Result<()> {
        use crate::vbios::Command;
        if self.flush.bytes() != 4096
            || self.libos.bytes() != 4096
            || self.wpr_meta.bytes() != 4096
            || self.frts.image.target != Engine::Gsp
            || self.sb.image.target != Engine::Gsp
            || self.loader.image.target != Engine::Sec2
            || self.unloader.image.target != Engine::Sec2
            || self.loader.image.bytes.len() != 56832
            || self.unloader.image.bytes.len() != 40704
            || self.loader.image.command.is_some()
            || self.unloader.image.command.is_some()
            || !matches!(self.sb.image.command, Some(Command::Sb))
        {
            return Err(Error::State);
        }
        let frts = self.framebuffer.frts();
        if !matches!(self.frts.image.command,Some(Command::Frts{address,bytes}) if address==frts.start&&bytes==frts.end-frts.start)
        {
            return Err(Error::Range);
        }
        let mut spans = [(0u64, 0u64); 7];
        for (i, span) in [self.flush, self.libos, self.wpr_meta]
            .into_iter()
            .enumerate()
        {
            spans[i] = (span.address(), span.address() + span.bytes() as u64);
        }
        for (i, source) in [self.frts, self.sb, self.loader, self.unloader]
            .into_iter()
            .enumerate()
        {
            spans[i + 3] = (source.iova, source.iova + source.image.bytes.len() as u64);
        }
        for (i, a) in spans.iter().enumerate() {
            if spans[..i].iter().any(|b| a.0 < b.1 && b.0 < a.1) {
                return Err(Error::Range);
            }
        }
        Ok(())
    }
}
/// One serialized, assigned AD106 controller. Software quarantine covers both
/// engines; constructing another Rust object is NOT permission to retry/reset.
/// The platform must retain hardware-accessible resources on failure/drop.
pub struct Ad106<P> {
    platform: P,
    stage: Stage,
    register: u32,
    value: Option<u32>,
    clock: u64,
    fault: Option<Diagnostic>,
    boot0: u32,
    ready: bool,
    flush: Option<u64>,
    phase: [Phase; 2],
    /// GA102 RESET_READY may fail to assert. Only that 150-us timeout is soft,
    /// exactly as upstream; memory scrub/core-select/load timeouts are fatal.
    pub reset_ready_observed: bool,
}
impl<P: Platform> Ad106<P> {
    /// No hardware access in construction. Caller first validates the exact
    /// assigned PCI board; probe separately checks the actual AD106 registers.
    pub fn new(platform: P) -> Self {
        Self {
            platform,
            stage: Stage::Probe,
            register: 0,
            value: None,
            clock: 0,
            fault: None,
            boot0: 0,
            ready: false,
            flush: None,
            phase: [Phase::Unknown; 2],
            reset_ready_observed: false,
        }
    }
    /// Serialized resource/IRQ adapter access, never permission to bypass the
    /// Falcon phase machine with raw reset/load writes. Backend errors poison
    /// the same controller and retained DMA views as Falcon failures.
    pub fn with_platform<T>(&mut self, f: impl FnOnce(&mut P) -> Result<T>) -> Result<T> {
        self.execute(|s| {
            s.stage = Stage::Backend;
            s.register = 0;
            s.value = None;
            f(&mut s.platform)
        })
    }
    /// Mark a framing/semantic RPC failure without retry/reset or DMA release.
    pub fn quarantine(&mut self, error: Error) -> Result<()> {
        self.execute(|s| {
            s.stage = Stage::Rpc;
            s.register = 0;
            s.value = None;
            Err(error)
        })
    }
    pub fn time_us(&mut self) -> Result<u64> {
        self.execute(|s| s.now())
    }
    pub fn sleep_until(&mut self, deadline: u64) -> Result<()> {
        self.execute(|s| s.sleep(deadline))
    }
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        self.fault
    }
    fn execute<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.fault.is_some() {
            return Err(Error::Quarantined);
        }
        match f(self) {
            Ok(value) => Ok(value),
            Err(error) => {
                // Nested handoff operations preserve the FIRST hardware failure.
                if self.fault.is_none() {
                    self.platform.quarantine();
                    self.fault = Some(Diagnostic {
                        stage: self.stage,
                        register: self.register,
                        value: self.value,
                        clock_us: self.clock,
                        error,
                    });
                }
                Err(error)
            }
        }
    }
    fn read(&mut self, offset: u32) -> Result<u32> {
        self.register = offset;
        self.value = None;
        let value = self.platform.read32(offset)?;
        self.value = Some(value);
        if value == u32::MAX || value & 0xffff_ff00 == 0xbadf_4100 {
            return Err(Error::Unavailable);
        }
        Ok(value)
    }
    fn write(&mut self, offset: u32, value: u32) -> Result<()> {
        self.register = offset;
        self.value = Some(value);
        self.platform.write32(offset, value)
    }
    fn update(&mut self, offset: u32, mask: u32, value: u32) -> Result<()> {
        let old = self.read(offset)?;
        self.write(offset, (old & !mask) | (value & mask))
    }
    fn now(&mut self) -> Result<u64> {
        let now = self.platform.now_us()?;
        if now < self.clock {
            return Err(Error::ClockRegressed);
        }
        self.clock = now;
        Ok(now)
    }
    fn limit(&mut self, outer: u64, budget: u64) -> Result<u64> {
        let now = self.now()?;
        if now >= outer {
            return Err(Error::Timeout);
        }
        Ok(outer.min(now.checked_add(budget).ok_or(Error::Range)?))
    }
    fn sleep(&mut self, until: u64) -> Result<()> {
        self.platform.sleep_until(until)?;
        if self.now()? < until {
            return Err(Error::ClockStopped);
        }
        Ok(())
    }
    fn poll(&mut self, offset: u32, mask: u32, expected: u32, deadline: u64) -> Result<u32> {
        loop {
            if self.now()? >= deadline {
                return Err(Error::Timeout);
            }
            let value = self.read(offset)?;
            if value & mask == expected {
                return Ok(value);
            }
            let until = deadline.min(self.clock.saturating_add(10));
            self.sleep(until)?;
        }
    }
    pub fn probe(&mut self, outer: u64) -> Result<()> {
        if self.ready {
            return Err(Error::State);
        }
        self.execute(|s| {
            s.stage = Stage::Probe;
            s.limit(outer, 4_000_000)?;
            s.boot0 = s.read(0)?;
            if (s.read(0xa00)? >> 20) & 0x3ff != 0x196 {
                return Err(Error::Unsupported);
            }
            s.stage = Stage::Gfw;
            let deadline = s.limit(outer, 4_000_000)?;
            loop {
                if s.now()? >= deadline {
                    return Err(Error::Timeout);
                }
                // Recheck privilege before EVERY progress read, as upstream.
                if s.read(0x118128)? & 1 != 0 && s.read(0x118234)? & 255 == 255 {
                    break;
                }
                s.sleep(deadline.min(s.clock.saturating_add(1_000)))?;
            }
            s.stage = Stage::Wpr;
            if s.wpr2_inner()?.is_some() {
                return Err(Error::State);
            }
            s.ready = true;
            Ok(())
        })
    }
    /// # Safety
    /// `iova` names a zeroed, committed, device-writable 4-KiB DMA grant owned by
    /// this session. Retain it through ALL reset/shutdown paths and uncertain
    /// failures; it is not safe to release it just because a Falcon halted.
    /// PCI bus mastering/IRQ/IOMMU setup belongs to devmgr/the native adapter.
    pub unsafe fn register_flush_page(&mut self, iova: u64, outer: u64) -> Result<()> {
        if !self.ready || self.flush.is_some() {
            return Err(Error::State);
        }
        if iova == 0
            || iova & 4095 != 0
            || iova.checked_add(4096).is_none_or(|end| end > 1u64 << 47)
        {
            return Err(Error::Range);
        }
        self.execute(|s| {
            s.stage = Stage::FlushPage;
            s.limit(outer, 1_000)?;
            s.platform.dma_write_barrier();
            s.write(0x100c40, (iova >> 40) as u32)?;
            s.write(0x100c10, (iova >> 8) as u32)?;
            let actual =
                ((s.read(0x100c40)? as u64 & 0xff_ffff) << 40) | ((s.read(0x100c10)? as u64) << 8);
            if actual != iova {
                return Err(Error::State);
            }
            s.flush = Some(iova);
            Ok(())
        })
    }
    pub fn signature_fuse(&mut self, engine_mask: u16, ucode: u8) -> Result<u32> {
        if !self.ready || !(1..=16).contains(&ucode) {
            return Err(Error::State);
        }
        let base = if engine_mask & 1 != 0 {
            0x824140
        } else if engine_mask & 4 != 0 {
            0x824100
        } else if engine_mask & 0x400 != 0 {
            0x8241c0
        } else {
            return Err(Error::Unsupported);
        };
        self.execute(|s| {
            s.stage = Stage::Fuse;
            let bits = s.read(base + (ucode as u32 - 1) * 4)? as u16;
            Ok(16 - bits.leading_zeros())
        })
    }
    pub fn reset(&mut self, engine: Engine, outer: u64) -> Result<()> {
        if !self.ready || self.flush.is_none() {
            return Err(Error::State);
        }
        self.execute(|s| {
            let p = engine.base();
            let p2 = engine.second();
            s.stage = Stage::ResetReady;
            let _ = s.read(p + 0xf4)?;
            let deadline = s.limit(outer, 150)?;
            s.reset_ready_observed = match s.poll(p + 0xf4, 1 << 31, 1 << 31, deadline) {
                Ok(_) => true,
                Err(Error::Timeout) => false,
                Err(e) => return Err(e),
            };
            s.stage = Stage::ResetPulse;
            s.limit(outer, 10)?;
            s.update(p + 0x3c0, 1, 1)?;
            let _ = s.read(p + 0x3c0)?; // flush posted reset assertion
            let wake = s
                .now()?
                .checked_add(10)
                .filter(|t| *t < outer)
                .ok_or(Error::Timeout)?;
            s.sleep(wake)?;
            s.update(p + 0x3c0, 1, 0)?;
            s.stage = Stage::Scrub;
            let deadline = s.limit(outer, 20_000)?;
            s.poll(p + 0xf4, 1 << 12, 0, deadline)?;
            s.stage = Stage::CoreSelect;
            if s.read(p2 + 0x668)? & (1 << 4) != 0 {
                s.write(p2 + 0x668, 0)?;
                let deadline = s.limit(outer, 10_000)?;
                s.poll(p2 + 0x668, 1, 1, deadline)?;
            }
            s.stage = Stage::Scrub;
            let deadline = s.limit(outer, 20_000)?;
            s.poll(p + 0xf4, 1 << 12, 0, deadline)?;
            s.write(p + 0x84, s.boot0)?;
            s.phase[engine.index()] = Phase::Reset;
            Ok(())
        })
    }
    /// Source pin stays held on error. This method never frees an allocation.
    pub fn load(&mut self, engine: Engine, source: &PinnedSource<'_>, outer: u64) -> Result<()> {
        if self.phase[engine.index()] != Phase::Reset || source.image.target != engine {
            return Err(Error::State);
        }
        self.execute(|s| {
            s.stage = Stage::Dma;
            // One absolute load budget, not a renewed timeout for every block.
            let deadline = s.limit(outer, 2_000_000)?;
            let p = engine.base();
            let p2 = engine.second();
            s.platform.dma_write_barrier();
            s.update(p + 0x624, 1 << 7, 1 << 7)?; // GPU physical aperture: still SID-translated IOVA
            s.write(p + 0x10c, 0)?;
            s.update(p + 0x600, 7, 5)?; // coherent sysmem + physical, context zero
            s.dma_segment(engine, source, source.image.imem, true, deadline)?;
            s.dma_segment(engine, source, source.image.dmem, false, deadline)?;
            let brom = source.image.brom;
            s.write(p2 + 0x210, brom.pkc)?;
            s.write(p2 + 0x19c, brom.engines as u32)?;
            s.write(p2 + 0x198, brom.ucode as u32)?;
            s.write(p2 + 0x180, 1)?; // RSA3K, never unsigned/dev selection
            s.write(p + 0x104, source.image.boot)?;
            s.phase[engine.index()] = Phase::Loaded;
            Ok(())
        })
    }
    fn dma_segment(
        &mut self,
        engine: Engine,
        source: &PinnedSource<'_>,
        segment: Segment,
        imem: bool,
        deadline: u64,
    ) -> Result<()> {
        let p = engine.base();
        // IMEM uses FBOFFS as a virtual tag; DMEM folds its source offset into
        // the base address, exactly as the GA102 DMA loader does.
        let base = source.iova + if imem { 0 } else { segment.source as u64 };
        self.write(p + 0x110, (base >> 8) as u32)?;
        self.write(p + 0x128, (base >> 40) as u32)?;
        let cmd = (6 << 8) | if imem { (1 << 4) | (1 << 2) } else { 0 };
        for pos in (0..segment.length).step_by(256) {
            if self.now()? >= deadline {
                return Err(Error::Timeout);
            }
            self.write(p + 0x114, segment.destination + pos)?;
            self.write(p + 0x11c, if imem { segment.source + pos } else { pos })?;
            self.write(p + 0x118, cmd)?;
            self.poll(p + 0x118, 2, 2, deadline)?;
        }
        Ok(())
    }
    /// Execute only a successfully loaded Falcon image. Mailbox addresses, if
    /// any, must name retained driver-owned metadata, never arbitrary clients.
    /// A zero return code still does not mean GSP init or RPC is ready.
    pub fn boot(
        &mut self,
        engine: Engine,
        mbox0: Option<u32>,
        mbox1: Option<u32>,
        outer: u64,
    ) -> Result<(u32, u32)> {
        if self.phase[engine.index()] != Phase::Loaded {
            return Err(Error::State);
        }
        self.start_and_wait(engine, mbox0, mbox1, outer)
    }
    /// Start the GSP's own boot-ROM path after reset, not an unsigned image.
    /// This follows gsp/hal/tu102.rs's LibOS mailbox handoff before SEC2 boot.
    ///
    /// # Safety
    /// The complete LibOS metadata grant and every resource it names must be
    /// committed on this assigned SID, device-accessible and retained through
    /// shutdown/isolation, including failures. Caller builds its checked ABI;
    /// this function checks only the address envelope, not nested pointers.
    pub unsafe fn boot_gsp_rom(
        &mut self,
        libos_iova: u64,
        libos_bytes: u64,
        outer: u64,
    ) -> Result<(u32, u32)> {
        if self.phase[0] != Phase::Reset {
            return Err(Error::State);
        }
        if libos_iova == 0
            || libos_iova & 4095 != 0
            || libos_bytes == 0
            || libos_iova
                .checked_add(libos_bytes)
                .is_none_or(|end| end > 1u64 << 47)
        {
            return Err(Error::Range);
        }
        self.start_and_wait(
            Engine::Gsp,
            Some(libos_iova as u32),
            Some((libos_iova >> 32) as u32),
            outer,
        )
    }
    fn start_and_wait(
        &mut self,
        engine: Engine,
        mbox0: Option<u32>,
        mbox1: Option<u32>,
        outer: u64,
    ) -> Result<(u32, u32)> {
        self.execute(|s| {
            s.stage = Stage::Start;
            let deadline = s.limit(outer, 2_000_000)?;
            let p = engine.base();
            if let Some(value) = mbox0 {
                s.write(p + 0x40, value)?;
            }
            if let Some(value) = mbox1 {
                s.write(p + 0x44, value)?;
            }
            let alias = s.read(p + 0x100)? & (1 << 6) != 0;
            s.platform.dma_write_barrier();
            s.write(p + if alias { 0x130 } else { 0x100 }, 2)?;
            s.stage = Stage::Halt;
            s.poll(p + 0x100, 1 << 4, 1 << 4, deadline)?;
            let status = (s.read(p + 0x40)?, s.read(p + 0x44)?);
            s.phase[engine.index()] = Phase::Halted;
            Ok(status)
        })
    }
    /// FWSEC's mailbox result plus the FRTS error field and WPR2 start check
    /// from gsp/hal/tu102.rs. SB deliberately does not claim to remove WPR2;
    /// that requires the separate SEC2 booter-unloader and later verification.
    pub fn run_fwsec(&mut self, source: &PinnedSource<'_>, outer: u64) -> Result<()> {
        let command = source.image.command.ok_or(Error::State)?;
        if source.image.target != Engine::Gsp {
            return Err(Error::State);
        }
        if let crate::vbios::Command::Frts { .. } = command {
            self.execute(|s| {
                s.stage = Stage::Wpr;
                if s.wpr2_inner()?.is_some() {
                    return Err(Error::State);
                }
                Ok(())
            })?;
        }
        self.reset(Engine::Gsp, outer)?;
        self.load(Engine::Gsp, source, outer)?;
        let (status, _) = self.boot(Engine::Gsp, Some(0), None, outer)?;
        self.execute(|s| {
            s.stage = Stage::FwsecStatus;
            if status != 0 {
                return Err(Error::FirmwareStatus(status));
            }
            if let crate::vbios::Command::Frts { address, .. } = command {
                let status = s.read(0x1438)? >> 16;
                if status != 0 {
                    return Err(Error::FirmwareStatus(status));
                }
                s.stage = Stage::Wpr;
                if s.wpr2_inner()?.is_none_or(|range| range.start != address) {
                    return Err(Error::State);
                }
            }
            Ok(())
        })
    }
    pub fn framebuffer(&mut self) -> Result<crate::boot::Framebuffer> {
        if !self.ready {
            return Err(Error::State);
        }
        self.execute(|s| {
            s.stage = Stage::Framebuffer;
            crate::boot::Framebuffer::from_registers(
                s.read(0x1183a4)?,
                s.read(0x820c04)?,
                s.read(0x625f04)?,
            )
        })
    }
    /// After shared-queue publication and DMA barriers. May be used to prequeue
    /// SetSystemInfo/SetRegistry before starting firmware, as in NOVA boot.rs.
    pub fn notify_queue(&mut self) -> Result<()> {
        if !self.ready {
            return Err(Error::State);
        }
        self.execute(|s| {
            s.stage = Stage::QueueNotify;
            s.platform.dma_write_barrier();
            s.write(0x110c00, 0)
        })
    }
    /// Source order: retained sysmembar -> FWSEC-FRTS/WPR2 -> GSP boot ROM ->
    /// signed SEC2 loader -> OS version -> bounded RISC-V-active wait.
    /// Success is ONLY the handoff, not GspInitDone or RPC readiness.
    ///
    /// # Safety
    /// All inputs/nested ABI pointers are committed, correctly staged mappings
    /// on this exact SID, retained through shutdown/isolation and uncertainty.
    /// Queue/log/LibOS/WPR/radix/firmware resources match the checked builders;
    /// the framebuffer plan is from this board's post-GFW samples. IRQ is
    /// prepared and bus mastering authorized. Cleanup SB/unloader inputs must
    /// be prepared BEFORE this call; no Drop or failure path executes them.
    pub unsafe fn handoff(&mut self, inputs: &Handoff<'_, '_>, outer: u64) -> Result<()> {
        inputs.validate()?;
        if !self.ready || self.flush.is_some() || self.phase != [Phase::Unknown; 2] {
            return Err(Error::State);
        }
        self.execute(|s| {
            // SAFETY: delegated retained-grant obligations above.
            unsafe {
                s.register_flush_page(inputs.flush.address(), outer)?;
            }
            s.run_fwsec(inputs.frts, outer)?;
            s.reset(Engine::Gsp, outer)?;
            unsafe {
                s.boot_gsp_rom(inputs.libos.address(), inputs.libos.bytes() as u64, outer)?;
            }
            s.reset(Engine::Sec2, outer)?;
            s.load(Engine::Sec2, inputs.loader, outer)?;
            let address = inputs.wpr_meta.address();
            let (status, _) = s.boot(
                Engine::Sec2,
                Some(address as u32),
                Some((address >> 32) as u32),
                outer,
            )?;
            s.stage = Stage::BooterStatus;
            if status != 0 {
                return Err(Error::FirmwareStatus(status));
            }
            s.write(0x110080, inputs.app_version)?;
            s.stage = Stage::Riscv;
            let deadline = s.limit(outer, 5_000_000)?;
            s.poll(Engine::Gsp.second() + 0x388, 1 << 7, 1 << 7, deadline)?;
            Ok(())
        })
    }
    pub fn wait_suspended(&mut self, outer: u64) -> Result<()> {
        self.execute(|s| {
            s.stage = Stage::Shutdown;
            let deadline = s.limit(outer, 5_000_000)?;
            s.poll(0x110040, 1 << 31, 1 << 31, deadline)?;
            Ok(())
        })
    }
    /// Normal explicit cleanup only. Suspended mailbox bit is required before
    /// SB reset. Even success does NOT prove DMA drain or authorize freeing RAM.
    ///
    /// # Safety
    /// A validated UnloadingGuestDriver RPC completed for this epoch. Inputs and
    /// all DMA backing remain retained. No restart/reassignment is authorized.
    pub unsafe fn unload(&mut self, inputs: &Handoff<'_, '_>, outer: u64) -> Result<()> {
        inputs.validate()?;
        self.execute(|s| {
            s.stage = Stage::Shutdown;
            if s.read(0x110040)? & (1 << 31) == 0 {
                return Err(Error::State);
            }
            s.run_fwsec(inputs.sb, outer)?;
            if s.wpr2_inner()?.is_some() {
                s.reset(Engine::Sec2, outer)?;
                s.load(Engine::Sec2, inputs.unloader, outer)?;
                let (status, _) = s.boot(Engine::Sec2, Some(0xff), Some(0xff), outer)?;
                s.stage = Stage::BooterStatus;
                if status != 0 {
                    return Err(Error::FirmwareStatus(status));
                }
                if s.wpr2_inner()?.is_some() {
                    return Err(Error::State);
                }
            }
            Ok(())
        })
    }
    fn wpr2_inner(&mut self) -> Result<Option<Range<u64>>> {
        let lo = ((self.read(0x1fa824)? >> 4) as u64) << 12;
        let hi = ((self.read(0x1fa828)? >> 4) as u64) << 12;
        if hi == 0 {
            return Ok(None);
        }
        if lo >= hi {
            return Err(Error::Range);
        }
        Ok(Some(lo..hi))
    }
    pub fn wpr2(&mut self) -> Result<Option<Range<u64>>> {
        self.execute(|s| {
            s.stage = Stage::Wpr;
            s.wpr2_inner()
        })
    }
    /// Status only, NOT GspInitDone and NOT a static-info reply.
    pub fn riscv_active(&mut self) -> Result<bool> {
        self.execute(|s| {
            s.stage = Stage::Riscv;
            Ok(s.read(Engine::Gsp.second() + 0x388)? & (1 << 7) != 0)
        })
    }
}
