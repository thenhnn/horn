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
pub struct Native {
    session: DeviceSession,
    time: Time,
    windows: Vec<(u32, MmioWindow)>,
    irq_prepared: bool,
    events_enabled: bool,
    poisoned: Rc<Cell<bool>>,
}
impl Native {
    /// All PROM/probe temporary windows must have been explicitly closed first.
    /// These seven exports fit the eight-window assignment bound. After ALL
    /// signature preparation, bind_irq revokes the fuse lease and adds GIN plus
    /// the narrow single-MSI EOI mirror window. No giant BAR1/BAR3 map is made.
    pub fn new(mut session: DeviceSession, time: Time) -> Self {
        let mut windows = Vec::new();
        windows.try_reserve_exact(8).expect("AD106 window metadata");
        for (base, bytes) in [
            (0, 65536),
            (0x100000, 4096),
            (0x110000, 65536),
            (0x1fa000, 4096),
            (0x820000, 65536),
            (0x840000, 8192),
            (0x625000, 4096),
        ] {
            windows.push((base, session.mmio_window(0, base as u64, bytes)));
        }
        Self {
            session,
            time,
            windows,
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
        for (base, window) in &self.windows {
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
        if self.windows.len() == 8 {
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
        let fuse = self
            .windows
            .iter()
            .position(|(base, _)| *base == 0x820000)
            .ok_or(Error::State)?;
        let (_, fuse) = self.windows.remove(fuse);
        self.session.close_window(fuse);
        for base in [0xb81000, 0x88000] {
            self.windows
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
    pub fn service_gsp_event(&mut self) -> Result<horn_nvidia_gsp::gin::Report> {
        self.healthy()?;
        if !self.events_enabled {
            return Err(Error::State);
        }
        let report = horn_nvidia_gsp::gin::service(self)?;
        // Device source/tree/MSI acknowledgment completed before this seL4 ack.
        self.session.ack_interrupt();
        if report.faults != 0 || report.remaining != 0 {
            self.poisoned.set(true);
            return Err(Error::FirmwareStatus(report.faults | report.remaining));
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
