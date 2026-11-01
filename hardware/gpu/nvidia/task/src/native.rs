// SPDX-License-Identifier: GPL-2.0-only
//! Accounted Horn resources for the AD106 boot core. No Linux DMA wrappers,
//! physical-RAM mapping, Drop cleanup or guessed clock/interrupt vectors.
use crate::{target, time::Time};
use alloc::{rc::Rc, vec::Vec};
use core::cell::Cell;
use core::{
    ptr::NonNull,
    sync::atomic::{Ordering, fence},
};
use horn_devmgr::protocol::*;
use horn_driver::{DeviceAccess, DeviceSession, MmioWindow};
use horn_nvidia_gsp::{Error, Result, falcon::Platform};

/// Raw view only; the DeviceSession and devmgr retain backing/pins. No Drop free.
#[must_use]
pub struct DmaBlock {
    pub iova: u64,
    pointer: NonNull<u8>,
    pub bytes: usize,
    poisoned: Rc<Cell<bool>>,
}
impl DmaBlock {
    /// CPU view of the block. The caller must not hand it to the device as an
    /// alias of anything else; it is the same memory the device sees.
    pub fn pointer(&self) -> NonNull<u8> {
        self.pointer
    }
    pub fn quarantine(&mut self) {
        self.poisoned.set(true);
    }
    fn bounds(&self, offset: usize, length: usize) -> Result<()> {
        if self.poisoned.get() {
            return Err(Error::Quarantined);
        }
        if offset
            .checked_add(length)
            .is_none_or(|end| end > self.bytes)
        {
            return Err(Error::Range);
        }
        Ok(())
    }
    /// # Safety
    /// The owning session/CPU mapping remains live. This span is CPU-owned,
    /// unpublished to the GPU or otherwise excluded from concurrent GPU writes.
    /// # Safety
    /// Same ownership requirements as initialize. Zeroes the whole page-rounded
    /// allocation so block-granular Falcon DMA cannot read uninitialized heap
    /// past the end of a segment whose size is not a multiple of 256.
    pub unsafe fn zero(&mut self) -> Result<()> {
        self.bounds(0, self.bytes)?;
        unsafe { core::ptr::write_bytes(self.pointer.as_ptr(), 0, self.bytes) };
        Ok(())
    }
    pub unsafe fn initialize(&mut self, offset: usize, bytes: &[u8]) -> Result<()> {
        self.bounds(offset, bytes.len())?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.pointer.as_ptr().add(offset),
                bytes.len(),
            );
        }
        Ok(())
    }
    /// # Safety
    /// Retained live mapping; caller obeys queue ownership. No ordinary Rust
    /// references into device-writable memory may alias these volatile accesses.
    pub unsafe fn read32(&self, offset: usize) -> Result<u32> {
        self.bounds(offset, 4)?;
        if offset & 3 != 0 {
            return Err(Error::Range);
        }
        let value = unsafe {
            self.pointer
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .read_volatile()
        };
        fence(Ordering::Acquire);
        Ok(u32::from_le(value))
    }
    /// # Safety
    /// Same retained-mapping/queue-ownership requirements as read32. Device
    /// publication/doorbell ordering is separate and uses DMA release barriers.
    pub unsafe fn write32(&mut self, offset: usize, value: u32) -> Result<()> {
        self.bounds(offset, 4)?;
        if offset & 3 != 0 {
            return Err(Error::Range);
        }
        unsafe {
            self.pointer
                .as_ptr()
                .add(offset)
                .cast::<u32>()
                .write_volatile(value.to_le());
        }
        Ok(())
    }
}
/// Cached on-demand pages. Two more fixed windows and one transient PROM window
/// stay inside the eight-export assignment bound.
const LIVE_WINDOWS: usize = 3;
pub struct Native {
    session: DeviceSession,
    time: Time,
    windows: Vec<(u32, MmioWindow)>,
    fixed: Vec<(u32, MmioWindow)>,
    irq_prepared: bool,
    events_enabled: bool,
    poisoned: Rc<Cell<bool>>,
}
impl Native {
    /// Registers are mapped on demand: one 4-KiB page per export, at most
    /// LIVE_WINDOWS cached plus two fixed GIN/EOI windows. Eagerly holding
    /// large register blocks exhausted the device-page claim budget before any
    /// firmware work started, and every export is a separately revocable lease.
    pub fn new(session: DeviceSession, time: Time) -> Self {
        let mut windows = Vec::new();
        windows
            .try_reserve_exact(LIVE_WINDOWS)
            .expect("AD106 window metadata");
        let mut fixed = Vec::new();
        fixed
            .try_reserve_exact(2)
            .expect("AD106 GIN window metadata");
        Self {
            session,
            time,
            windows,
            fixed,
            irq_prepared: false,
            events_enabled: false,
            poisoned: Rc::new(Cell::new(false)),
        }
    }
    fn healthy(&self) -> Result<()> {
        if self.poisoned.get() {
            Err(Error::Quarantined)
        } else {
            Ok(())
        }
    }
    fn pointer(&mut self, offset: u32) -> Result<*mut u32> {
        self.healthy()?;
        if offset & 3 != 0 {
            return Err(Error::Range);
        }
        for (base, window) in self.fixed.iter().chain(self.windows.iter()) {
            if let Some(within) = offset.checked_sub(*base)
                && (within as usize)
                    .checked_add(4)
                    .is_some_and(|end| end <= window.length())
            {
                return Ok(unsafe { window.pointer().as_ptr().add(within as usize).cast::<u32>() });
            }
        }
        if offset as usize >= horn_nvidia::probe::BAR0_BYTES {
            return Err(Error::Range);
        }
        // Serialized UC access: no raw alias escapes the read/write call.
        // Revoke before recycling an export, not by silently replacing a map.
        if self.windows.len() == LIVE_WINDOWS {
            let (_, window) = self.windows.remove(0);
            self.session.close_window(window);
        }
        let base = offset & !4095;
        let window = self.session.mmio_window(0, base as u64, 4096);
        let pointer = unsafe {
            window
                .pointer()
                .as_ptr()
                .add((offset - base) as usize)
                .cast()
        };
        self.windows.push((base, window));
        Ok(pointer)
    }
    /// Map a window of the VRAM aperture (BAR1). The USERD and the channel
    /// instance live in video memory, which the CPU reaches only this way.
    pub fn vram_window(&mut self, offset: u64, length: usize) -> Result<MmioWindow> {
        self.healthy()?;
        if length == 0 || length > 65536 || length & 4095 != 0 || offset & 4095 != 0 {
            return Err(Error::Range);
        }
        Ok(self.session.mmio_window(1, offset, length))
    }
    /// Release a VRAM window: revokes the CPU alias and the lease.
    pub fn close_vram_window(&mut self, window: MmioWindow) {
        self.session.close_window(window);
    }
    pub fn prom(&mut self) -> Result<Vec<u8>> {
        self.healthy()?;
        crate::prom::snapshot(&mut self.session, horn_nvidia::probe::BAR0_BYTES)
            .map_err(|_| Error::Backend)
    }
    pub fn close(&mut self) -> Result<()> {
        self.healthy()?;
        self.time.cancel(&mut self.session)?;
        self.session.control().close().map_err(|_| Error::Backend)?;
        self.poisoned.set(true);
        Ok(())
    }
    pub fn transport(&mut self) -> &mut horn_ipc::Sel4Transport<horn_ipc::Sel4CapSpace> {
        self.session.transport()
    }
    /// Release a DMA buffer: unmap, unpin and drain through devmgr, returning
    /// the capability slots and the accounting quota. The caller must have
    /// finished every device access to it (for the firmware image: after
    /// INIT_DONE). Uncertain cleanup would be a fail-stop, not a silent reuse.
    pub fn release(&mut self, block: DmaBlock) -> Result<()> {
        self.healthy()?;
        let pointer = core::ptr::NonNull::new(block.pointer.as_ptr()).ok_or(Error::Backend)?;
        let pages = block.bytes / 4096;
        if pages == 0 {
            return Err(Error::Range);
        }
        // SAFETY: the block is consumed here, so no CPU alias survives, and the
        // caller guarantees the device no longer reads it.
        unsafe {
            self.session.dma_dealloc(block.iova, pointer, pages);
        }
        Ok(())
    }
    pub fn allocate(&mut self, bytes: usize, access: DeviceAccess) -> Result<DmaBlock> {
        self.healthy()?;
        let bytes = bytes.checked_add(4095).ok_or(Error::Range)? & !4095;
        if bytes == 0 || bytes > 64 * 1024 * 1024 {
            return Err(Error::Range);
        }
        let (iova, pointer) = self.session.dma_alloc(bytes / 4096, access);
        if iova & 4095 != 0
            || iova
                .checked_add(bytes as u64)
                .is_none_or(|end| end > 1u64 << 47)
        {
            self.poisoned.set(true);
            return Err(Error::Range); // retain, never free on uncertainty
        }
        Ok(DmaBlock {
            iova,
            pointer,
            bytes,
            poisoned: self.poisoned.clone(),
        })
    }
    /// After SB/unloader AND boot sources have been production-patched. The
    /// fuse aperture is explicitly revoked before replacing its quota slot.
    pub fn bind_irq(&mut self) -> Result<()> {
        self.healthy()?;
        if self.irq_prepared {
            return Err(Error::State);
        }
        for (base, _) in self.fixed.iter() {
            if *base == 0x820000 {
                return Err(Error::State);
            }
        }
        // Every production signature is prepared; the fuse aperture is dead.
        if let Some(fuse) = self.windows.iter().position(|(base, _)| *base == 0x820000) {
            let (_, fuse) = self.windows.remove(fuse);
            self.session.close_window(fuse);
        }
        if !self.fixed.is_empty() {
            return Err(Error::State);
        }
        for base in [0xb81000u32, 0x88000] {
            self.fixed
                .push((base, self.session.mmio_window(0, base as u64, 4096)));
        }
        horn_nvidia_gsp::gin::quiesce(self)?;
        self.session.interrupt_on(
            &target(DEVICE_DRIVER_CAPS_IRQ_SIGNAL_SLOT),
            target(DEVICE_DRIVER_CAPS_WAKE_SLOT),
        );
        self.irq_prepared = true;
        Ok(())
    }
    /// # Safety
    /// GIN must be masked/prepared, IRQ bound, all DMA inputs committed and a
    /// fresh firmware epoch established. Clearing bus-master is not a drain.
    pub unsafe fn activate(&mut self) -> Result<()> {
        self.healthy()?;
        if !self.irq_prepared {
            return Err(Error::State);
        }
        self.session.activate();
        Ok(())
    }
    /// Transition from boot polling only AFTER handoff/RPC setup. The caller
    /// immediately drains queues, including notifications posted before clear.
    pub fn enable_gsp_events(&mut self) -> Result<()> {
        self.healthy()?;
        if !self.irq_prepared || self.events_enabled {
            return Err(Error::State);
        }
        horn_nvidia_gsp::gin::quiesce(self)?;
        self.time.take_irq();
        self.session.ack_interrupt();
        horn_nvidia_gsp::gin::enable(self)?;
        self.events_enabled = true;
        Ok(())
    }
    /// Clock waits can consume a wake notification while saving its IRQ badge.
    /// Pollers must service that saved device IRQ before returning to recv.
    pub fn service_pending_gsp_event(&mut self) -> Result<()> {
        self.healthy()?;
        if self.time.take_irq() {
            self.service_gsp_event()?;
        }
        Ok(())
    }
    pub fn service_gsp_event(&mut self) -> Result<horn_nvidia_gsp::gin::Report> {
        self.healthy()?;
        if !self.events_enabled {
            return Err(Error::State);
        }
        let report = horn_nvidia_gsp::gin::service(self)?;
        // Device source/tree/MSI acknowledgment completed before this seL4 ack.
        self.session.ack_interrupt();
        if report.faults != 0 || report.remaining != 0 {
            // A fault bitmap, not an NV status: name it before failing stop.
            sel4::debug_println!(
                "NVIDIA_GIN_FAULT faults={:#x} remaining={:#x}",
                report.faults,
                report.remaining
            );
            self.poisoned.set(true);
            return Err(Error::State);
        }
        Ok(report)
    }
    pub fn disable_gsp_events(&mut self) -> Result<()> {
        self.healthy()?;
        if !self.events_enabled {
            return Err(Error::State);
        }
        horn_nvidia_gsp::gin::disable(self)?;
        self.events_enabled = false;
        Ok(())
    }
    pub fn take_irq(&mut self) -> bool {
        self.time.take_irq()
    }
    /// # Safety
    /// Device-specific cause/latch acknowledgment and GIN rearm are completed.
    pub unsafe fn ack_irq(&mut self) -> Result<()> {
        self.healthy()?;
        self.session.ack_interrupt();
        Ok(())
    }
}
impl Platform for Native {
    fn read32(&mut self, offset: u32) -> Result<u32> {
        let pointer = self.pointer(offset)?;
        Ok(u32::from_le(unsafe { pointer.read_volatile() }))
    }
    fn write32(&mut self, offset: u32, value: u32) -> Result<()> {
        let pointer = self.pointer(offset)?;
        fence(Ordering::SeqCst);
        unsafe {
            pointer.write_volatile(value.to_le());
        }
        Ok(())
    }
    fn now_us(&mut self) -> Result<u64> {
        self.healthy()?;
        self.time.now(&mut self.session)
    }
    fn sleep_until(&mut self, deadline: u64) -> Result<()> {
        self.healthy()?;
        self.time.sleep_until(&mut self.session, deadline)
    }
    fn dma_write_barrier(&mut self) {
        fence(Ordering::SeqCst);
    }
    fn quarantine(&mut self) {
        self.poisoned.set(true);
    }
}
