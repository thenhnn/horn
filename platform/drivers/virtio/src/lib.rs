//! Horn's small adapter around upstream virtio-drivers. PCI discovery,
//! virtqueues and the console implementation are not duplicated here.
#![no_std]
pub mod config;
use core::ptr::NonNull;
pub use virtio_drivers;
use virtio_drivers::transport::{DeviceStatus, DeviceType, InterruptStatus, Transport};
pub use virtio_drivers::{BufferDirection, Hal, PhysAddr, device::console::VirtIOConsole};
use zerocopy::{FromBytes, Immutable, IntoBytes};

const ACCESS_PLATFORM: u64 = 1 << 33;
const VERSION_1: u64 = 1 << 32;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    LegacyTransport,
    MissingPlatformAccess,
    InvalidCommonRegion,
}

/// Upstream 0.13 has PCI/console/virtqueues/HAL, but its console feature mask
/// omits ACCESS_PLATFORM and PciTransport has no MSI-X selector API. This shim
/// adds those two platform requirements; all other transport work is upstream.
pub struct IommuPciTransport<T: Transport> {
    inner: T,
    common: NonNull<u8>,
    vector: u16,
}
impl<T: Transport> IommuPciTransport<T> {
    /// # Safety
    /// `common` is the SAME mapped common-config region used by `inner`, at
    /// least `length` bytes long, naturally aligned, and alive through Drop.
    /// All accesses to it must be serialized through this wrapper. No Rust
    /// references to the registers may exist. `vector` selects a devmgr-owned
    /// MSI-X table entry, not an APIC interrupt vector or a raw MSI address.
    pub unsafe fn new(
        mut inner: T,
        common: NonNull<u8>,
        length: usize,
        vector: u16,
    ) -> Result<Self, Error> {
        if length < 56 || common.as_ptr() as usize & 3 != 0 || vector == u16::MAX {
            return Err(Error::InvalidCommonRegion);
        }
        if inner.requires_legacy_layout() {
            return Err(Error::LegacyTransport);
        }
        if inner.read_device_features() & (ACCESS_PLATFORM | VERSION_1)
            != ACCESS_PLATFORM | VERSION_1
        {
            return Err(Error::MissingPlatformAccess);
        }
        Ok(Self {
            inner,
            common,
            vector,
        })
    }
    fn selector(&mut self, offset: usize, value: u16) {
        // SAFETY: new() establishes the MMIO lifetime/alignment and exclusive
        // serialized access. No borrowed register references are constructed.
        unsafe {
            let pointer = self.common.as_ptr().add(offset).cast::<u16>();
            pointer.write_volatile(value);
            assert_eq!(pointer.read_volatile(), value, "MSI-X selector rejected");
        }
    }
}
impl<T: Transport> Transport for IommuPciTransport<T> {
    fn device_type(&self) -> DeviceType {
        self.inner.device_type()
    }
    fn read_device_features(&mut self) -> u64 {
        self.inner.read_device_features()
    }
    fn write_driver_features(&mut self, features: u64) {
        self.inner.write_driver_features(features | ACCESS_PLATFORM);
    }
    fn max_queue_size(&mut self, queue: u16) -> u32 {
        self.inner.max_queue_size(queue)
    }
    fn notify(&mut self, queue: u16) {
        self.inner.notify(queue);
    }
    fn get_status(&self) -> DeviceStatus {
        self.inner.get_status()
    }
    fn set_status(&mut self, status: DeviceStatus) {
        self.inner.set_status(status);
        if status.contains(DeviceStatus::FEATURES_OK) {
            assert!(
                self.inner.get_status().contains(DeviceStatus::FEATURES_OK),
                "virtio features rejected"
            );
        }
        if status == DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER {
            self.selector(16, self.vector);
        }
    }
    fn set_guest_page_size(&mut self, size: u32) {
        self.inner.set_guest_page_size(size);
    }
    fn requires_legacy_layout(&self) -> bool {
        false
    }
    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: PhysAddr,
        driver_area: PhysAddr,
        device_area: PhysAddr,
    ) {
        // Route BEFORE enabling the queue, so its first completion cannot race
        // assignment of the MSI-X selector. Config and queues share one entry.
        self.selector(22, queue);
        self.selector(26, self.vector);
        self.inner
            .queue_set(queue, size, descriptors, driver_area, device_area);
    }
    fn queue_unset(&mut self, queue: u16) {
        self.inner.queue_unset(queue);
    }
    fn queue_used(&mut self, queue: u16) -> bool {
        self.inner.queue_used(queue)
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        self.inner.ack_interrupt()
    }
    fn read_config_generation(&self) -> u32 {
        self.inner.read_config_generation()
    }
    fn read_config_space<V: FromBytes + IntoBytes>(
        &self,
        offset: usize,
    ) -> virtio_drivers::Result<V> {
        self.inner.read_config_space(offset)
    }
    fn write_config_space<V: IntoBytes + Immutable>(
        &mut self,
        offset: usize,
        value: V,
    ) -> virtio_drivers::Result<()> {
        self.inner.write_config_space(offset, value)
    }
}
#[cfg(test)]
mod tests;
