// SPDX-License-Identifier: GPL-2.0-only
//! Live, retained CPU view for the GSP page-ring transport.
use crate::native::DmaBlock;
use core::sync::atomic::{Ordering, fence};
use horn_nvidia_gsp::{Result, queue::Memory};
pub struct Shared<'a> {
    block: &'a mut DmaBlock,
}
impl<'a> Shared<'a> {
    /// # Safety
    /// The block is the committed initialized ReadWrite queue for this SID,
    /// with a stable retained CPU mapping until this view and Queue are gone.
    /// No ordinary Rust references to its GPU-writable RAM survive publication.
    /// No assignment close/revocation may run concurrently with this view.
    pub unsafe fn new(block: &'a mut DmaBlock) -> Self {
        Self { block }
    }
}
impl Memory for Shared<'_> {
    fn bytes(&self) -> usize {
        self.block.bytes
    }
    fn read32(&mut self, offset: usize) -> Result<u32> {
        // SAFETY: constructor retention contract, aligned checked raw access.
        unsafe { self.block.read32(offset) }
    }
    fn write32(&mut self, offset: usize, value: u32) -> Result<()> {
        unsafe { self.block.write32(offset, value) }
    }
    fn acquire(&mut self) {
        fence(Ordering::SeqCst);
    }
    fn release(&mut self) {
        fence(Ordering::SeqCst);
    }
    fn quarantine(&mut self) {
        self.block.quarantine();
    }
}
